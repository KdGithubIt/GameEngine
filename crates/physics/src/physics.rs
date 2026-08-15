//! Rapier-backed velocity, gravity, and rigid-body integration (ADR 0096).
//!
//! The public `Velocity`, `Gravity`, and `GravityScale` contracts are
//! physics-owned and re-exported by the high-level engine facade. Each fixed step mirrors authored/runtime ECS bodies into a
//! Rapier world, advances the solver, and writes dynamic results back to ECS.
//! Rapier handles and solver state never enter scene documents or save data.

use glam::Vec3;
use hashbrown::HashMap;
use rapier3d::prelude::{
    ColliderBuilder, Group, InteractionGroups, InteractionTestMode, PhysicsWorld,
    RigidBodyBuilder, RigidBodyHandle, RigidBodyType, Vector,
};

use engine_ecs::{Entity, Query, Res, ResMut};

use crate::character_controller::KinematicCharacterController;
use crate::collision::{
    Collider, CollisionEvents, CollisionLayers, PhysicsBody, TriggerVolume,
};
use crate::time::FixedTime;
use crate::transform::Transform;

/// Terminal velocity in the downward direction, in metres per second.
const TERMINAL_VELOCITY_DOWN: f32 = 50.0;

/// Linear velocity of a physics body in world space, in metres per second.
#[derive(Clone, Debug)]
pub struct Velocity {
    /// The current linear velocity.
    pub linear: Vec3,
}

impl Default for Velocity {
    fn default() -> Self {
        Self { linear: Vec3::ZERO }
    }
}

/// Per-entity multiplier applied to the global [`Gravity`] acceleration.
#[derive(Clone, Debug)]
pub struct GravityScale(pub f32);

impl Default for GravityScale {
    fn default() -> Self {
        Self(1.0)
    }
}

/// Global gravitational acceleration applied by Rapier to dynamic bodies.
#[derive(Clone, Debug)]
pub struct Gravity(pub Vec3);

impl Default for Gravity {
    fn default() -> Self {
        Self(Vec3::new(0.0, -9.81, 0.0))
    }
}

/// Non-authored gameplay solver state retained for diagnostics and future
/// incremental synchronization. The ECS remains authoritative each step.
pub struct GameplayPhysicsWorld {
    world: PhysicsWorld,
    entity_bodies: HashMap<Entity, RigidBodyHandle>,
}

impl Default for GameplayPhysicsWorld {
    fn default() -> Self {
        Self {
            world: PhysicsWorld::new(),
            entity_bodies: HashMap::new(),
        }
    }
}

impl GameplayPhysicsWorld {
    /// Creates an empty Rapier gameplay world.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of ECS entities backed by Rapier rigid bodies in
    /// the most recently completed fixed step.
    pub fn rigid_body_count(&self) -> usize {
        self.entity_bodies.len()
    }
}

/// Compatibility scheduling stage retained as the first public physics
/// system. Gravity itself is applied by Rapier in [`velocity_system`].
pub fn gravity_system(_gravity: Res<Gravity>, _fixed_time: Res<FixedTime>) {}

type PhysicsQuery<'a> = (
    &'a PhysicsBody,
    Option<&'a Collider>,
    &'a mut Transform,
    Option<&'a mut Velocity>,
    Option<&'a GravityScale>,
    Option<&'a KinematicCharacterController>,
    Option<&'a TriggerVolume>,
    Option<&'a CollisionLayers>,
);

/// Mirrors ECS physics bodies into Rapier, advances one fixed step, and
/// writes dynamic positions and velocities back to ECS.
///
/// Static entities are collider-only objects. Ordinary kinematic entities
/// use position-based Rapier bodies. Custom character controllers remain
/// collider-only, preserving their tuned engine-owned movement algorithm.
pub fn velocity_system(
    gravity: Res<Gravity>,
    fixed_time: Res<FixedTime>,
    mut retained_world: Option<ResMut<GameplayPhysicsWorld>>,
    mut query: Query<PhysicsQuery<'_>>,
) {
    let mut fallback = GameplayPhysicsWorld::new();
    let retained = retained_world.as_deref_mut().unwrap_or(&mut fallback);
    let mut world = PhysicsWorld::new();
    world.gravity = vector(gravity.0);
    world.integration_parameters.dt = fixed_time.fixed_delta.max(f32::EPSILON);
    let mut entity_bodies = HashMap::new();

    for (entity, (body_kind, collider, transform, velocity, gravity_scale, controller, trigger, layers)) in
        query.iter_mut()
    {
        let pose = pose(transform);
        let rigid_body_type = match body_kind {
            PhysicsBody::Static => None,
            PhysicsBody::Kinematic if controller.is_some() => None,
            PhysicsBody::Kinematic => Some(RigidBodyType::KinematicPositionBased),
            PhysicsBody::Dynamic => Some(RigidBodyType::Dynamic),
        };

        let body_handle = rigid_body_type.map(|kind| {
            let initial_velocity = velocity
                .as_deref()
                .map(|velocity| velocity.linear)
                .unwrap_or(Vec3::ZERO);
            let mut builder = RigidBodyBuilder::new(kind)
                .pose(pose)
                .linvel(vector(initial_velocity));
            if kind == RigidBodyType::Dynamic {
                builder = builder
                    .gravity_scale(gravity_scale.map(|scale| scale.0).unwrap_or(1.0))
                    .ccd_enabled(true)
                    // Public gameplay bodies expose linear velocity only and
                    // their AABB/CapsuleY shapes are axis-aligned contracts.
                    .lock_rotations();
            }
            let handle = world.bodies.insert(builder);
            entity_bodies.insert(entity, handle);
            handle
        });

        if let Some(collider) = collider {
            let collision_layers = layers.cloned().unwrap_or_default();
            let mut collider_builder = ColliderBuilder::new(collider.rapier_shape(transform.scale))
                .position(if body_handle.is_some() {
                    rapier3d::prelude::Pose::identity()
                } else {
                    pose
                })
                .sensor(trigger.is_some())
                .collision_groups(InteractionGroups::new(
                    Group::from_bits_truncate(collision_layers.membership),
                    Group::from_bits_truncate(collision_layers.mask),
                    InteractionTestMode::And,
                ));
            if *body_kind == PhysicsBody::Dynamic {
                collider_builder = collider_builder.restitution(0.3).friction(0.5);
            }
            let collider = collider_builder.build();
            if let Some(parent) = body_handle {
                world
                    .colliders
                    .insert_with_parent(collider, parent, &mut world.bodies);
            } else {
                world.colliders.insert(collider);
            }
        }
    }

    world.step();

    for (entity, (body_kind, _, transform, velocity, ..)) in query.iter_mut() {
        if *body_kind != PhysicsBody::Dynamic {
            continue;
        }
        let Some(handle) = entity_bodies.get(&entity).copied() else {
            continue;
        };
        let Some(body) = world.bodies.get_mut(handle) else {
            continue;
        };
        let mut linear = vec3(body.linvel());
        linear.y = linear.y.max(-TERMINAL_VELOCITY_DOWN);
        body.set_linvel(vector(linear), true);
        transform.translation = vec3(body.translation());
        if let Some(velocity) = velocity {
            velocity.linear = linear;
        }
    }

    retained.world = world;
    retained.entity_bodies = entity_bodies;
}

/// Compatibility scheduling stage retained after collision detection.
/// Rapier has already applied friction and restitution during
/// [`velocity_system`], so this function intentionally performs no second
/// reflection against [`CollisionEvents`].
pub fn restitution_system(_events: Res<CollisionEvents>) {}

fn vector(value: Vec3) -> Vector {
    Vector::new(value.x, value.y, value.z)
}

fn vec3(value: Vector) -> Vec3 {
    Vec3::new(value.x, value.y, value.z)
}

fn pose(transform: &Transform) -> rapier3d::prelude::Pose {
    let rotation = rapier3d::prelude::Rotation::from_xyzw(
        transform.rotation.x,
        transform.rotation.y,
        transform.rotation.z,
        transform.rotation.w,
    );
    rapier3d::prelude::Pose::from_parts(
        Vector::new(
            transform.translation.x,
            transform.translation.y,
            transform.translation.z,
        ),
        rotation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::GlobalTransform;

    #[test]
    fn gravity_default_points_down() {
        assert!(Gravity::default().0.y < 0.0);
    }

    #[test]
    fn rapier_dynamic_body_falls_and_updates_public_velocity() {
        let mut app = engine_ecs::App::new();
        app.insert_resource(Gravity::default());
        app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
        app.insert_resource(GameplayPhysicsWorld::new());
        let entity = app.world_mut().spawn().expect("dynamic body");
        app.world_mut()
            .add_component(entity, Transform::from_translation(Vec3::Y * 2.0))
            .expect("transform");
        app.world_mut()
            .add_component(entity, GlobalTransform::identity())
            .expect("global transform");
        app.world_mut()
            .add_component(entity, Collider::sphere(0.25))
            .expect("collider");
        app.world_mut()
            .add_component(entity, PhysicsBody::Dynamic)
            .expect("body kind");
        app.world_mut()
            .add_component(entity, Velocity::default())
            .expect("velocity");
        app.add_system(velocity_system);
        app.update().expect("Rapier step");

        assert!(app.world().get_component::<Transform>(entity).unwrap().translation.y < 2.0);
        assert!(app.world().get_component::<Velocity>(entity).unwrap().linear.y < 0.0);
        assert_eq!(
            app.world()
                .get_resource::<GameplayPhysicsWorld>()
                .unwrap()
                .rigid_body_count(),
            1
        );
    }
}
