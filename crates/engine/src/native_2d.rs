//! Cross-domain Native 2D physics composition (ADR 0127).

pub use engine_physics::native_2d::*;

use crate::transform::{GlobalTransform, Parent, Transform};
use engine_authoring::Project2dSettings;
use engine_ecs::{Entity, Query, Res, ResMut};
use glam::{Quat, Vec2};
use std::collections::BTreeSet;

/// One structured reason an authored Transform could not participate in 2D physics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Physics2dDiagnosticKind {
    /// The effective world transform cannot be represented by the planar contract.
    InvalidPlanarPose(PlanarPoseError),
    /// Dynamic writeback through a parent hierarchy is not silently approximated.
    ParentedDynamicBody,
}

/// Runtime diagnostic for one Native 2D physics entity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Physics2dDiagnostic {
    /// Runtime entity carrying the invalid physics state.
    pub entity: Entity,
    /// Structured diagnostic classification.
    pub kind: Physics2dDiagnosticKind,
}

/// Latest fixed-step Native 2D diagnostics.
#[derive(Debug, Default)]
pub struct Physics2dDiagnostics {
    entries: Vec<Physics2dDiagnostic>,
}

impl Physics2dDiagnostics {
    /// Iterates diagnostics emitted by the most recent fixed step.
    pub fn iter(&self) -> impl Iterator<Item = &Physics2dDiagnostic> {
        self.entries.iter()
    }
}

/// Dedicated 2D solver state and latest transition events.
#[derive(Debug, Default)]
pub struct PhysicsRuntime2d {
    world: PhysicsWorld2d,
    events: Vec<ContactEvent2d>,
}

impl PhysicsRuntime2d {
    /// Returns the dedicated 2D world for read-only gameplay queries.
    pub fn world(&self) -> &PhysicsWorld2d {
        &self.world
    }

    /// Returns transition events emitted by the latest fixed step.
    pub fn events(&self) -> &[ContactEvent2d] {
        &self.events
    }
}

/// Applies persisted project 2D settings to one runtime host.
///
/// Editor Play and the packaged Player call this same function after loading
/// [`Project2dSettings`], preventing host-specific gravity interpretation.
pub fn apply_project_2d_settings(app: &mut crate::App, settings: &Project2dSettings) {
    app.insert_resource(Gravity2d(Vec2::new(
        settings.gravity[0] as f32,
        settings.gravity[1] as f32,
    )));
}

fn runtime_key(entity: Entity) -> u64 {
    (u64::from(entity.generation()) << 32) | u64::from(entity.id())
}

type Physics2dQuery<'a> = (
    &'a mut Transform,
    &'a GlobalTransform,
    Option<&'a Parent>,
    Option<&'a mut RigidBody2d>,
    &'a Collider2d,
);

/// Synchronizes ECS components into the dedicated 2D world, steps it, and
/// writes root dynamic poses back through the existing Transform authority.
pub fn physics_2d_fixed_system(
    gravity: Res<Gravity2d>,
    fixed_time: Res<crate::time::FixedTime>,
    mut runtime: ResMut<PhysicsRuntime2d>,
    mut diagnostics: ResMut<Physics2dDiagnostics>,
    mut query: Query<Physics2dQuery<'_>>,
) {
    diagnostics.entries.clear();
    let mut active = BTreeSet::new();

    for (entity, (transform, global, parent, body, collider)) in query.iter_mut() {
        let key = runtime_key(entity);
        let authored_body = body
            .as_deref()
            .copied()
            .unwrap_or_else(RigidBody2d::default);
        if authored_body.mode == RigidBodyMode2d::Dynamic && parent.is_some() {
            diagnostics.entries.push(Physics2dDiagnostic {
                entity,
                kind: Physics2dDiagnosticKind::ParentedDynamicBody,
            });
            continue;
        }
        let matrix = if parent.is_some() {
            global.matrix()
        } else {
            transform.to_matrix()
        };
        let pose = match project_planar_transform(matrix) {
            Ok(pose) => pose,
            Err(error) => {
                diagnostics.entries.push(Physics2dDiagnostic {
                    entity,
                    kind: Physics2dDiagnosticKind::InvalidPlanarPose(error),
                });
                continue;
            }
        };
        runtime.world.upsert(BodyEntry2d {
            entity: key,
            pose,
            body: authored_body,
            collider: collider.clone(),
        });
        active.insert(key);
    }

    runtime.world.retain_entities(&active);
    runtime.events = runtime.world.step(fixed_time.fixed_delta, gravity.0);

    for (entity, (transform, _, parent, body, _)) in query.iter_mut() {
        let Some(body) = body else {
            continue;
        };
        if body.mode != RigidBodyMode2d::Dynamic || parent.is_some() {
            continue;
        }
        let Some(resolved) = runtime.world.body(runtime_key(entity)) else {
            continue;
        };
        transform.translation.x = resolved.pose.translation.x;
        transform.translation.y = resolved.pose.translation.y;
        transform.rotation = Quat::from_rotation_z(resolved.pose.rotation);
        body.velocity = resolved.body.velocity;
        body.angular_velocity = resolved.body.angular_velocity;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_settings_apply_same_typed_gravity_resource() {
        let mut app = crate::App::new();
        let mut settings = Project2dSettings::default();
        settings.gravity = [2.5, -7.0];
        apply_project_2d_settings(&mut app, &settings);
        assert_eq!(
            app.world().get_resource::<Gravity2d>().unwrap().0,
            Vec2::new(2.5, -7.0)
        );
    }
}
