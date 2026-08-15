//! Combat compatibility facade with render-only debugging kept at composition level.

use std::collections::BTreeMap;

use engine_ecs::{Query, Res, ResMut};
use glam::Vec3;

use crate::character_controller::KinematicCharacterController;
use crate::collision::{Collider, CollisionEvents, WorldShape};
use crate::debug_draw::DebugLines;
use crate::hitbox::AttackHitbox;
use crate::transform::GlobalTransform;

pub use engine_gameplay::combat::*;

/// Draws fixed-step combat state for the Play debugger.
pub fn combat_debug_draw_system(
    collisions: Option<Res<CollisionEvents>>,
    controllers: Query<(&KinematicCharacterController, &GlobalTransform)>,
    hitboxes: Query<(&AttackHitbox, &Collider, &GlobalTransform)>,
    mut lines: Option<ResMut<DebugLines>>,
) {
    let Some(lines) = lines.as_deref_mut() else {
        return;
    };
    let mut positions = BTreeMap::new();
    for (entity, (controller, transform)) in &controllers {
        let position = transform.matrix().col(3).truncate();
        positions.insert(entity, position);
        let color = if controller.grounded {
            Vec3::new(0.2, 1.0, 0.2)
        } else {
            Vec3::new(1.0, 0.8, 0.1)
        };
        lines.line(position, position + controller.velocity * 0.1, color);
        lines.line(position - Vec3::X * 0.15, position + Vec3::X * 0.15, color);
    }
    for (entity, (hitbox, collider, transform)) in &hitboxes {
        if !hitbox.enabled {
            continue;
        }
        let red = Vec3::new(1.0, 0.1, 0.1);
        match collider.world_shape(transform) {
            WorldShape::Aabb(aabb) => lines.aabb(aabb.center, aabb.half_extents, red),
            WorldShape::Sphere(sphere) => lines.sphere_wire(sphere.center, sphere.radius, red),
            WorldShape::CapsuleY(capsule) => {
                lines.capsule_y_wire(capsule.segment_a, capsule.segment_b, capsule.radius, red)
            }
        }
        positions
            .entry(entity)
            .or_insert_with(|| transform.matrix().col(3).truncate());
    }
    if let Some(collisions) = collisions {
        for collision in collisions.iter() {
            let Some(first) = positions.get(&collision.entity_a) else {
                continue;
            };
            let Some(second) = positions.get(&collision.entity_b) else {
                continue;
            };
            let normal = collision.push_out.normalize_or_zero();
            if normal.length_squared() > 0.0 {
                let origin = (*first + *second) * 0.5;
                lines.line(origin, origin + normal * 0.5, Vec3::new(0.1, 0.8, 1.0));
            }
        }
    }
}
