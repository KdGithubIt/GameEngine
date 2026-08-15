//! Kinematic character-controller composition adapter (Phase 57).
//!
//! Physics owns the movement and collision solver. This top-level adapter only
//! gathers ECS obstacle snapshots and consumes animation root-motion requests
//! before invoking that solver.

use engine_ecs::{Query, Res};
use engine_physics::character_controller::{
    character_separation_deltas, step_character_controller, CharacterObstacle,
    CharacterSeparationBody, CharacterStepContext,
};
use glam::Vec3;

use crate::animation::RootMotionRequest;
use crate::collision::{Collider, CollisionLayers, PhysicsBody};
use crate::physics::Gravity;
use crate::time::FixedTime;
use crate::transform::Transform;

/// Physics-owned character-controller state re-exported through the engine facade.
pub use engine_physics::character_controller::KinematicCharacterController;

type CharacterControllerQuery<'a> = (
    &'a mut KinematicCharacterController,
    &'a Collider,
    &'a mut Transform,
    Option<&'a CollisionLayers>,
    Option<&'a mut RootMotionRequest>,
);

/// Integrates velocity, gravity, and one-shot root motion, then resolves the
/// character against solid obstacles using the physics-owned solver.
///
/// Register this in the fixed-update schedule before
/// [`crate::collision::collision_detection_system`]. Root motion remains an
/// animation-domain input and is consumed exactly once in this adapter; all
/// movement, ground detection, step/snap behavior, and character separation
/// are delegated to `engine-physics`.
pub fn character_controller_system(
    gravity: Option<Res<Gravity>>,
    fixed_time: Res<FixedTime>,
    mut controllers: Query<CharacterControllerQuery<'_>>,
    obstacles: Query<crate::collision::CollisionQueryData>,
) {
    let gravity_accel = gravity.map(|gravity| gravity.0).unwrap_or(Vec3::ZERO);
    let fixed_delta = fixed_time.fixed_delta;

    let mut solids: Vec<CharacterObstacle> = obstacles
        .iter()
        .filter_map(|(entity, (collider, body, gt, layers, trigger, hitbox))| {
            if trigger.is_some() || hitbox.is_some_and(|hitbox| !hitbox.enabled) {
                return None;
            }
            if *body != PhysicsBody::Static && *body != PhysicsBody::Kinematic {
                return None;
            }
            Some(CharacterObstacle {
                entity,
                collider: collider.clone(),
                global_transform: gt.clone(),
                layers: layers.cloned().unwrap_or_default(),
            })
        })
        .collect();
    solids.sort_by_key(|obstacle| obstacle.entity);

    for (self_entity, (controller, collider, transform, self_layers, root_motion)) in
        controllers.iter_mut()
    {
        let extra_displacement = root_motion.map_or(Vec3::ZERO, |root_motion| {
            let delta = root_motion.delta;
            root_motion.delta = Vec3::ZERO;
            delta
        });
        let self_layers = self_layers.cloned().unwrap_or_default();
        step_character_controller(
            controller,
            transform,
            CharacterStepContext {
                self_entity,
                collider,
                layers: &self_layers,
                obstacles: &solids,
                gravity_accel,
                fixed_delta,
                extra_displacement,
            },
        );
    }

    let characters = controllers
        .iter_mut()
        .map(
            |(entity, (_controller, collider, transform, layers, _root_motion))| {
                CharacterSeparationBody {
                    entity,
                    collider: collider.clone(),
                    transform: transform.clone(),
                    layers: layers.cloned().unwrap_or_default(),
                }
            },
        )
        .collect::<Vec<_>>();
    let separation = character_separation_deltas(&characters);
    for (entity, (_controller, _collider, transform, _layers, _root_motion)) in
        controllers.iter_mut()
    {
        if let Some(delta) = separation.get(&entity) {
            transform.translation += *delta;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collision::TriggerVolume;
    use crate::transform::GlobalTransform;
    use engine_ecs::App;

    fn make_app_with_gravity() -> App {
        let mut app = App::new();
        app.insert_resource(FixedTime::with_delta(1.0 / 60.0));
        app.insert_resource(Gravity::default());
        app
    }

    #[test]
    fn controller_default_uses_three_resolve_iterations_and_unit_gravity_scale() {
        let controller = KinematicCharacterController::default();
        assert_eq!(controller.max_resolve_iterations, 3);
        assert_eq!(controller.gravity_scale, 1.0);
        assert!(!controller.grounded);
        assert_eq!(controller.velocity, Vec3::ZERO);
    }

    #[test]
    fn falling_controller_lands_on_static_floor_and_becomes_grounded() {
        let mut app = make_app_with_gravity();

        let floor = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::ZERO))
            .expect("spawn floor");
        app.world_mut()
            .add_component(
                floor,
                GlobalTransform(Transform::from_translation(Vec3::ZERO).to_matrix()),
            )
            .expect("add floor global transform");
        app.world_mut()
            .add_component(floor, Collider::aabb(Vec3::new(5.0, 0.5, 5.0)))
            .expect("add floor collider");
        app.world_mut()
            .add_component(floor, PhysicsBody::Static)
            .expect("add floor body");

        let character = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::new(0.0, 3.0, 0.0)))
            .expect("spawn character");
        app.world_mut()
            .add_component(character, Collider::capsule_y(0.5, 0.4))
            .expect("add character collider");
        app.world_mut()
            .add_component(
                character,
                KinematicCharacterController {
                    velocity: Vec3::ZERO,
                    gravity_scale: 1.0,
                    grounded: false,
                    max_resolve_iterations: 3,
                    ..KinematicCharacterController::default()
                },
            )
            .expect("add character controller");

        app.add_fixed_system(character_controller_system);

        for _ in 0..240 {
            app.run_fixed_update().expect("fixed update must run");
        }

        let controller = app
            .world()
            .get_component::<KinematicCharacterController>(character)
            .expect("controller must remain attached");
        assert!(controller.grounded, "character must come to rest grounded");
        assert!(
            controller.velocity.y.abs() < 1e-3,
            "downward velocity must be zeroed once grounded, got {}",
            controller.velocity.y
        );

        let transform = app
            .world()
            .get_component::<Transform>(character)
            .expect("character must keep Transform");
        assert!(
            transform.translation.y > 1.0,
            "character must rest above the floor, y={}",
            transform.translation.y
        );
    }

    #[test]
    fn controller_moving_into_a_wall_is_stopped_by_push_out() {
        let mut app = make_app_with_gravity();
        app.insert_resource(Gravity(Vec3::ZERO));

        let wall = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)))
            .expect("spawn wall");
        app.world_mut()
            .add_component(
                wall,
                GlobalTransform(Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)).to_matrix()),
            )
            .expect("add wall global transform");
        app.world_mut()
            .add_component(wall, Collider::aabb(Vec3::new(0.5, 2.0, 2.0)))
            .expect("add wall collider");
        app.world_mut()
            .add_component(wall, PhysicsBody::Static)
            .expect("add wall body");

        let character = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::ZERO))
            .expect("spawn character");
        app.world_mut()
            .add_component(character, Collider::sphere(0.4))
            .expect("add character collider");
        app.world_mut()
            .add_component(
                character,
                KinematicCharacterController {
                    velocity: Vec3::new(5.0, 0.0, 0.0),
                    gravity_scale: 0.0,
                    grounded: false,
                    max_resolve_iterations: 3,
                    ..KinematicCharacterController::default()
                },
            )
            .expect("add character controller");

        app.add_fixed_system(character_controller_system);

        for _ in 0..30 {
            app.run_fixed_update().expect("fixed update must run");
        }

        let transform = app
            .world()
            .get_component::<Transform>(character)
            .expect("character must keep Transform");
        assert!(
            transform.translation.x > 1.0 && transform.translation.x < 1.2,
            "character must be stopped at the wall, x={}",
            transform.translation.x
        );
    }

    #[test]
    fn root_motion_request_is_applied_once_and_then_consumed() {
        let mut app = make_app_with_gravity();
        app.insert_resource(Gravity(Vec3::ZERO));

        let character = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::ZERO))
            .expect("spawn character");
        app.world_mut()
            .add_component(character, Collider::sphere(0.4))
            .expect("add character collider");
        app.world_mut()
            .add_component(
                character,
                KinematicCharacterController {
                    velocity: Vec3::ZERO,
                    gravity_scale: 0.0,
                    grounded: false,
                    max_resolve_iterations: 3,
                    ..KinematicCharacterController::default()
                },
            )
            .expect("add character controller");
        app.world_mut()
            .add_component(
                character,
                RootMotionRequest {
                    delta: Vec3::new(1.25, 0.0, -0.5),
                },
            )
            .expect("add root motion request");
        app.add_fixed_system(character_controller_system);

        app.run_fixed_update().expect("first motor step must run");
        let first = app
            .world()
            .get_component::<Transform>(character)
            .expect("character transform");
        assert!(
            first
                .translation
                .abs_diff_eq(Vec3::new(1.25, 0.0, -0.5), 1e-5),
            "subdivided root motion must reach the requested destination"
        );
        assert_eq!(
            app.world()
                .get_component::<RootMotionRequest>(character)
                .expect("root motion request")
                .delta,
            Vec3::ZERO,
            "the motor must consume the displacement in the same fixed step"
        );

        app.run_fixed_update().expect("second motor step must run");
        let second = app
            .world()
            .get_component::<Transform>(character)
            .expect("character transform");
        assert!(
            second
                .translation
                .abs_diff_eq(Vec3::new(1.25, 0.0, -0.5), 1e-5),
            "a paused or stopped animator must not replay stale root motion"
        );
    }

    #[test]
    fn trigger_volumes_do_not_stop_or_ground_the_controller() {
        let mut app = make_app_with_gravity();
        app.insert_resource(Gravity(Vec3::ZERO));

        let trigger = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)))
            .expect("spawn trigger");
        app.world_mut()
            .add_component(
                trigger,
                GlobalTransform(Transform::from_translation(Vec3::new(2.0, 0.0, 0.0)).to_matrix()),
            )
            .expect("add trigger global transform");
        app.world_mut()
            .add_component(trigger, Collider::aabb(Vec3::new(0.5, 2.0, 2.0)))
            .expect("add trigger collider");
        app.world_mut()
            .add_component(trigger, PhysicsBody::Static)
            .expect("add trigger body");
        app.world_mut()
            .add_component(trigger, TriggerVolume)
            .expect("add trigger marker");

        let character = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::ZERO))
            .expect("spawn character");
        app.world_mut()
            .add_component(character, Collider::sphere(0.4))
            .expect("add character collider");
        app.world_mut()
            .add_component(
                character,
                KinematicCharacterController {
                    velocity: Vec3::new(5.0, 0.0, 0.0),
                    gravity_scale: 0.0,
                    grounded: false,
                    max_resolve_iterations: 3,
                    ..KinematicCharacterController::default()
                },
            )
            .expect("add character controller");

        app.add_fixed_system(character_controller_system);

        for _ in 0..30 {
            app.run_fixed_update().expect("fixed update must run");
        }

        let transform = app
            .world()
            .get_component::<Transform>(character)
            .expect("character must keep Transform");
        assert!(
            transform.translation.x > 2.0,
            "trigger volume must not stop the controller, x={}",
            transform.translation.x
        );
        let controller = app
            .world()
            .get_component::<KinematicCharacterController>(character)
            .expect("controller must remain attached");
        assert!(
            !controller.grounded,
            "trigger volume must not ground the controller"
        );
    }

    #[test]
    fn entity_without_collider_is_skipped_without_panicking() {
        let mut app = make_app_with_gravity();

        let character = app
            .world_mut()
            .spawn_with(Transform::from_translation(Vec3::ZERO))
            .expect("spawn character");
        app.world_mut()
            .add_component(character, KinematicCharacterController::default())
            .expect("add character controller");

        app.add_fixed_system(character_controller_system);
        assert!(app.run_fixed_update().is_ok());
    }

    #[test]
    fn fast_dash_does_not_tunnel_through_thin_wall() {
        let mut app = make_app_with_gravity();
        app.insert_resource(Gravity(Vec3::ZERO));
        let wall_transform = Transform::from_translation(Vec3::new(2.0, 0.0, 0.0));
        let wall = app.world_mut().spawn_with(wall_transform.clone()).unwrap();
        app.world_mut()
            .add_component(wall, GlobalTransform(wall_transform.to_matrix()))
            .unwrap();
        app.world_mut()
            .add_component(wall, Collider::aabb(Vec3::new(0.05, 2.0, 2.0)))
            .unwrap();
        app.world_mut()
            .add_component(wall, PhysicsBody::Static)
            .unwrap();

        let character = app.world_mut().spawn_with(Transform::default()).unwrap();
        app.world_mut()
            .add_component(character, Collider::sphere(0.4))
            .unwrap();
        app.world_mut()
            .add_component(
                character,
                KinematicCharacterController {
                    velocity: Vec3::X * 600.0,
                    gravity_scale: 0.0,
                    ..KinematicCharacterController::default()
                },
            )
            .unwrap();
        app.add_fixed_system(character_controller_system);

        app.run_fixed_update().unwrap();
        let x = app
            .world()
            .get_component::<Transform>(character)
            .unwrap()
            .translation
            .x;
        assert!(x < 1.6, "dash crossed the validation wall: x={x}");
    }

    #[test]
    fn overlapping_characters_receive_symmetric_separation() {
        let mut app = make_app_with_gravity();
        app.insert_resource(Gravity(Vec3::ZERO));
        let mut entities = Vec::new();
        for x in [-0.2, 0.2] {
            let entity = app
                .world_mut()
                .spawn_with(Transform::from_translation(Vec3::X * x))
                .unwrap();
            app.world_mut()
                .add_component(entity, Collider::sphere(0.5))
                .unwrap();
            app.world_mut()
                .add_component(entity, KinematicCharacterController::default())
                .unwrap();
            entities.push(entity);
        }
        app.add_fixed_system(character_controller_system);
        app.run_fixed_update().unwrap();

        let first = app.world().get_component::<Transform>(entities[0]).unwrap();
        let second = app.world().get_component::<Transform>(entities[1]).unwrap();
        assert!((second.translation.x - first.translation.x - 1.0).abs() < 1e-5);
        assert!((first.translation.x + second.translation.x).abs() < 1e-5);
    }
}
