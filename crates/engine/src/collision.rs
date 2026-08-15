//! Compatibility facade for physics collision plus cross-domain adapters.

pub use engine_physics::collision::*;

use crate::debug_draw::DebugLines;
use crate::hitbox::AttackHitbox;
use crate::transform::{GlobalTransform, Transform};
use engine_ecs::{Entity, Query, ResMut};
use glam::Vec3;
use hashbrown::HashMap;
use rapier3d::prelude::{ColliderBuilder as RapierColliderBuilder, InteractionTestMode};
use std::collections::BTreeSet;

/// Component tuple queried by collision detection and character-controller scans.
///
/// `AttackHitbox` remains an engine-composition concern: the physics crate owns
/// the collision contract, while this adapter preserves the existing gameplay
/// rule that disabled attack hitboxes are excluded from broad-phase detection.
pub(crate) type CollisionQueryData<'w> = (
    &'w Collider,
    &'w PhysicsBody,
    &'w GlobalTransform,
    Option<&'w CollisionLayers>,
    Option<&'w TriggerVolume>,
    Option<&'w AttackHitbox>,
);

/// Detects shape overlaps and applies push-out corrections to non-static bodies.
pub fn collision_detection_system(
    query: Query<CollisionQueryData>,
    mut transforms: Query<&mut Transform>,
    mut events: ResMut<CollisionEvents>,
    mut stats: Option<ResMut<CollisionStats>>,
) {
    events.begin_detection();

    let mut bodies: Vec<(
        Entity,
        WorldShape,
        PhysicsBody,
        CollisionLayers,
        bool,
    )> = query
        .iter()
        .filter_map(|(entity, (collider, body, gt, layers, trigger, hitbox))| {
            if hitbox.is_some_and(|hitbox| !hitbox.enabled) {
                return None;
            }
            Some((
                entity,
                collider.world_shape(gt),
                body.clone(),
                layers.cloned().unwrap_or_default(),
                trigger.is_some(),
            ))
        })
        .collect();
    bodies.sort_by_key(|(entity, ..)| *entity);

    if let Some(stats) = stats.as_deref_mut() {
        *stats = CollisionStats {
            proxy_count: bodies.len(),
            ..CollisionStats::default()
        };
    }

    let mut pipeline = rapier3d::prelude::CollisionPipeline::new();
    let mut islands = rapier3d::prelude::IslandManager::new();
    let mut broad_phase = rapier3d::prelude::BroadPhaseBvh::new();
    let mut narrow_phase = rapier3d::prelude::NarrowPhase::new();
    let mut rigid_bodies = rapier3d::prelude::RigidBodySet::new();
    let mut colliders = rapier3d::prelude::ColliderSet::new();
    let mut collider_to_body = HashMap::new();

    for (index, (_, shape, _, layers, is_trigger)) in bodies.iter().enumerate() {
        let (pose, shared_shape) = rapier_world_shape(shape);
        let mut collider = RapierColliderBuilder::new(shared_shape)
            .position(pose)
            .sensor(*is_trigger)
            .active_collision_types(rapier3d::prelude::ActiveCollisionTypes::all())
            .build();
        collider.set_collision_groups(rapier3d::prelude::InteractionGroups::new(
            rapier3d::prelude::Group::from_bits_truncate(layers.membership),
            rapier3d::prelude::Group::from_bits_truncate(layers.mask),
            InteractionTestMode::And,
        ));
        let handle = colliders.insert(collider);
        collider_to_body.insert(handle, index);
    }

    pipeline.step(
        0.0,
        &mut islands,
        &mut broad_phase,
        &mut narrow_phase,
        &mut rigid_bodies,
        &mut colliders,
        &(),
        &(),
    );

    let mut push_outs = Vec::new();
    let mut reported_pairs = BTreeSet::new();
    let mut candidates = Vec::new();
    candidates.extend(
        narrow_phase
            .contact_pairs()
            .map(|pair| (pair.collider1, pair.collider2)),
    );
    candidates.extend(
        narrow_phase
            .intersection_pairs()
            .filter_map(|(a, b, intersects)| intersects.then_some((a, b))),
    );

    for (handle_a, handle_b) in candidates {
        let Some(&index_a) = collider_to_body.get(&handle_a) else {
            continue;
        };
        let Some(&index_b) = collider_to_body.get(&handle_b) else {
            continue;
        };
        let (first, second) = if index_a <= index_b {
            (index_a, index_b)
        } else {
            (index_b, index_a)
        };
        if !reported_pairs.insert((first, second)) {
            continue;
        }
        if let Some(stats) = stats.as_deref_mut() {
            stats.candidate_pair_count += 1;
            stats.narrow_phase_count += 1;
        }
        record_overlap(
            &bodies[first],
            &bodies[second],
            &mut events,
            stats.as_deref_mut(),
            &mut push_outs,
        );
    }

    // Preserve the established continuous trigger fallback. Solid contacts are
    // still entirely delegated to Rapier's discrete broad/narrow phase.
    for first in 0..bodies.len() {
        for second in (first + 1)..bodies.len() {
            if reported_pairs.contains(&(first, second))
                || (!bodies[first].4 && !bodies[second].4)
            {
                continue;
            }
            let (entity_a, shape_a, _, layers_a, _) = &bodies[first];
            let (entity_b, shape_b, _, layers_b, _) = &bodies[second];
            if !should_collide(layers_a, layers_b) {
                continue;
            }
            let Some(previous_a) = events.previous_shape(*entity_a) else {
                continue;
            };
            let Some(previous_b) = events.previous_shape(*entity_b) else {
                continue;
            };
            if swept_shapes_overlap(previous_a, *shape_a, previous_b, *shape_b) {
                reported_pairs.insert((first, second));
                if let Some(stats) = stats.as_deref_mut() {
                    stats.candidate_pair_count += 1;
                    stats.narrow_phase_count += 1;
                    stats.contact_count += 1;
                }
                events.push_event(CollisionEvent {
                    entity_a: *entity_a,
                    entity_b: *entity_b,
                    push_out: Vec3::ZERO,
                    is_trigger: true,
                });
            }
        }
    }

    for (entity, delta) in push_outs {
        if let Some((_, transform)) = transforms.iter_mut().find(|(candidate, _)| *candidate == entity)
        {
            transform.translation += delta;
        }
    }
    events.finish_detection(bodies.iter().map(|(entity, shape, ..)| (*entity, *shape)));
}

fn record_overlap(
    a: &(Entity, WorldShape, PhysicsBody, CollisionLayers, bool),
    b: &(Entity, WorldShape, PhysicsBody, CollisionLayers, bool),
    events: &mut CollisionEvents,
    stats: Option<&mut CollisionStats>,
    push_outs: &mut Vec<(Entity, Vec3)>,
) {
    let (entity_a, shape_a, body_a, layers_a, trigger_a) = a;
    let (entity_b, shape_b, body_b, layers_b, trigger_b) = b;
    if !should_collide(layers_a, layers_b) {
        return;
    }
    let is_trigger = *trigger_a || *trigger_b;
    let push = if is_trigger {
        PushOut { vector: Vec3::ZERO }
    } else if let Some(push) = world_shapes_overlap(shape_a, shape_b) {
        push
    } else {
        return;
    };
    events.push_event(CollisionEvent {
        entity_a: *entity_a,
        entity_b: *entity_b,
        push_out: push.vector,
        is_trigger,
    });
    if let Some(stats) = stats {
        stats.contact_count += 1;
    }
    if !is_trigger {
        if *body_a != PhysicsBody::Static {
            push_outs.push((*entity_a, push.vector));
        }
        if *body_b != PhysicsBody::Static {
            push_outs.push((*entity_b, -push.vector));
        }
    }
}

/// Draws physics colliders through the render-runtime debug-line adapter.
pub fn collider_debug_draw_system(
    query: Query<(&Collider, &GlobalTransform)>,
    mut debug_lines: ResMut<DebugLines>,
) {
    let green = Vec3::new(0.0, 1.0, 0.0);
    for (_, (collider, gt)) in &query {
        match collider.world_shape(gt) {
            WorldShape::Aabb(aabb) => debug_lines.aabb(aabb.center, aabb.half_extents, green),
            WorldShape::Sphere(sphere) => {
                debug_lines.sphere_wire(sphere.center, sphere.radius, green)
            }
            WorldShape::CapsuleY(capsule) => debug_lines.capsule_y_wire(
                capsule.segment_a,
                capsule.segment_b,
                capsule.radius,
                green,
            ),
        }
    }
}
