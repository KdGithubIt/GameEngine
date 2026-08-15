//! Physics-owned kinematic character-controller state and pure movement solver.

use std::collections::BTreeMap;

use engine_ecs::Entity;
use engine_rig::transform::{GlobalTransform, Transform};
use glam::Vec3;

use crate::collision::{should_collide, world_shapes_overlap, Collider, CollisionLayers};

/// Default number of push-out resolve passes per fixed step.
const DEFAULT_MAX_RESOLVE_ITERATIONS: u32 = 3;
/// Maximum movement subdivisions used to keep fast characters from tunneling.
const MAX_MOTION_SUBSTEPS: u32 = 128;

/// State and tuning for the engine kinematic character controller.
#[derive(Debug, Clone)]
pub struct KinematicCharacterController {
    /// Current linear velocity, integrated into translation each fixed step.
    pub velocity: Vec3,
    /// Multiplier applied to the world gravity acceleration.
    pub gravity_scale: f32,
    /// Whether the previous resolve pass found walkable ground.
    pub grounded: bool,
    /// Maximum number of push-out resolve passes per fixed step.
    pub max_resolve_iterations: u32,
    /// Steepest contact normal treated as walkable ground, in degrees.
    pub slope_limit_degrees: f32,
    /// Maximum vertical ledge height that horizontal motion may climb.
    pub step_offset: f32,
    /// Distance used to retain contact with descending ground.
    pub ground_snap_distance: f32,
    /// Small separation margin used for motion subdivision and contact tests.
    pub skin_width: f32,
}

impl Default for KinematicCharacterController {
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            gravity_scale: 1.0,
            grounded: false,
            max_resolve_iterations: DEFAULT_MAX_RESOLVE_ITERATIONS,
            slope_limit_degrees: 50.0,
            step_offset: 0.3,
            ground_snap_distance: 0.15,
            skin_width: 0.02,
        }
    }
}

/// One solid obstacle snapshot consumed by the pure character solver.
#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct CharacterObstacle {
    /// Runtime entity represented by this snapshot.
    pub entity: Entity,
    /// Collision shape of the obstacle.
    pub collider: Collider,
    /// World transform used for collision queries.
    pub global_transform: GlobalTransform,
    /// Collision filter applied against the moving character.
    pub layers: CollisionLayers,
}

/// Immutable per-step inputs supplied by the high-level movement adapter.
#[doc(hidden)]
pub struct CharacterStepContext<'a> {
    /// Entity being moved.
    pub self_entity: Entity,
    /// Character collision shape.
    pub collider: &'a Collider,
    /// Character collision filter.
    pub layers: &'a CollisionLayers,
    /// Stable obstacle snapshots for this fixed step.
    pub obstacles: &'a [CharacterObstacle],
    /// World gravity acceleration.
    pub gravity_accel: Vec3,
    /// Fixed-step duration in seconds.
    pub fixed_delta: f32,
    /// One-shot displacement supplied by a higher domain, such as root motion.
    pub extra_displacement: Vec3,
}

/// Advances one character against environment obstacles without depending on
/// animation, input, gameplay, or the top-level engine composition crate.
#[doc(hidden)]
pub fn step_character_controller(
    controller: &mut KinematicCharacterController,
    transform: &mut Transform,
    context: CharacterStepContext<'_>,
) {
    let may_step = controller.grounded;
    controller.velocity.y +=
        context.gravity_accel.y * controller.gravity_scale * context.fixed_delta;
    let displacement = controller.velocity * context.fixed_delta + context.extra_displacement;
    let iterations = controller.max_resolve_iterations.max(1);
    let mut grounded = false;

    let shape_extent = context
        .collider
        .world_shape(&GlobalTransform(transform.to_matrix()))
        .enclosing_aabb()
        .half_extents
        .min_element()
        .max(controller.skin_width.max(0.001));
    let substep_count = ((displacement.length() / (shape_extent * 0.5)).ceil() as u32)
        .clamp(1, MAX_MOTION_SUBSTEPS);
    let substep = displacement / substep_count as f32;

    for _ in 0..substep_count {
        transform.translation += substep;
        for _ in 0..iterations {
            let mut resolved_any = false;
            for obstacle in context.obstacles {
                if obstacle.entity == context.self_entity
                    || !should_collide(context.layers, &obstacle.layers)
                {
                    continue;
                }
                let self_shape = context
                    .collider
                    .world_shape(&GlobalTransform(transform.to_matrix()));
                let other_shape = obstacle.collider.world_shape(&obstacle.global_transform);
                if let Some(push) = world_shapes_overlap(&self_shape, &other_shape) {
                    let normal = push.vector.normalize_or_zero();
                    let walkable_y = controller.slope_limit_degrees.to_radians().cos();
                    if normal.y >= walkable_y {
                        grounded = true;
                    }
                    if normal.y < -0.5 && controller.velocity.y > 0.0 {
                        controller.velocity.y = 0.0;
                    }
                    if normal.y.abs() < 0.5 {
                        if may_step && controller.step_offset > 0.0 {
                            let original = transform.translation;
                            transform.translation.y += controller.step_offset;
                            let stepped_shape = context
                                .collider
                                .world_shape(&GlobalTransform(transform.to_matrix()));
                            if world_shapes_overlap(&stepped_shape, &other_shape).is_none() {
                                grounded = true;
                                resolved_any = true;
                                continue;
                            }
                            transform.translation = original;
                        }
                        controller.velocity -= normal * controller.velocity.dot(normal).min(0.0);
                    }
                    transform.translation += push.vector;
                    resolved_any = true;
                }
            }
            if !resolved_any {
                break;
            }
        }
    }

    if !grounded && controller.velocity.y <= 0.0 && controller.ground_snap_distance > 0.0 {
        let before_snap = transform.translation;
        transform.translation.y -= controller.ground_snap_distance;
        for obstacle in context.obstacles {
            if obstacle.entity == context.self_entity
                || !should_collide(context.layers, &obstacle.layers)
            {
                continue;
            }
            let self_shape = context
                .collider
                .world_shape(&GlobalTransform(transform.to_matrix()));
            let other_shape = obstacle.collider.world_shape(&obstacle.global_transform);
            if let Some(push) = world_shapes_overlap(&self_shape, &other_shape) {
                let normal = push.vector.normalize_or_zero();
                if normal.y >= controller.slope_limit_degrees.to_radians().cos() {
                    transform.translation += push.vector;
                    grounded = true;
                    break;
                }
            }
        }
        if !grounded {
            transform.translation = before_snap;
        }
    }

    controller.grounded = grounded;
    if grounded && controller.velocity.y < 0.0 {
        controller.velocity.y = 0.0;
    }
}

/// One character snapshot used for deterministic symmetric separation.
#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct CharacterSeparationBody {
    /// Runtime entity represented by this snapshot.
    pub entity: Entity,
    /// Character collision shape.
    pub collider: Collider,
    /// Local transform after environment resolution.
    pub transform: Transform,
    /// Collision filter applied against other characters.
    pub layers: CollisionLayers,
}

/// Computes symmetric push-out deltas for overlapping character snapshots.
#[doc(hidden)]
pub fn character_separation_deltas(
    characters: &[CharacterSeparationBody],
) -> BTreeMap<Entity, Vec3> {
    let mut separation = BTreeMap::<Entity, Vec3>::new();
    for first_index in 0..characters.len() {
        for second_index in (first_index + 1)..characters.len() {
            let first = &characters[first_index];
            let second = &characters[second_index];
            if !should_collide(&first.layers, &second.layers) {
                continue;
            }
            let first_shape = first
                .collider
                .world_shape(&GlobalTransform(first.transform.to_matrix()));
            let second_shape = second
                .collider
                .world_shape(&GlobalTransform(second.transform.to_matrix()));
            if let Some(push) = world_shapes_overlap(&first_shape, &second_shape) {
                *separation.entry(first.entity).or_default() += push.vector * 0.5;
                *separation.entry(second.entity).or_default() -= push.vector * 0.5;
            }
        }
    }
    separation
}
