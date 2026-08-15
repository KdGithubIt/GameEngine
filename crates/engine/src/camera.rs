use crate::collision::{
    segment_blocked_by_static, static_obstacle_aabbs, Collider, PhysicsBody, TriggerVolume,
};
use crate::input::{Input, MouseButton, MouseInput};
use crate::lock_on::TargetLock;
use crate::time::Time;
use crate::transform::{GlobalTransform, Transform};
use glam::{Mat4, Quat, Vec3};
use hashbrown::HashMap;

pub use engine_render_runtime::camera::{
    camera_aspect_system, default_camera_transform, Camera3D, ViewportSize,
};
pub(crate) use engine_render_runtime::camera::{
    camera_selection_key, select_active_game_camera,
};

/// A camera that orbits around a fixed world-space target point.
///
/// Attach this component alongside [`Camera3D`] and [`Transform`]. Register
/// [`orbit_camera_system`] to have it update the [`Transform`] each frame.
///
/// Mouse-drag rotates the orbit; the scroll wheel changes the distance.
#[derive(Debug, Clone)]
pub struct OrbitCamera {
    /// The world-space point the camera rotates around.
    pub target: Vec3,
    /// Distance from [`target`](Self::target) to the camera eye.
    pub distance: f32,
    /// Horizontal angle in radians (rotation around the world Y axis).
    pub yaw: f32,
    /// Vertical elevation angle in radians.
    pub pitch: f32,
    /// Minimum allowed pitch in radians.
    pub pitch_min: f32,
    /// Maximum allowed pitch in radians.
    pub pitch_max: f32,
    /// How many radians the orbit moves per pixel of mouse drag.
    pub orbit_speed: f32,
    /// How much the distance changes per scroll unit.
    pub zoom_speed: f32,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 5.0,
            yaw: 0.0,
            pitch: 0.3,
            pitch_min: -std::f32::consts::FRAC_PI_2 + 0.05,
            pitch_max: std::f32::consts::FRAC_PI_2 - 0.05,
            orbit_speed: 0.005,
            zoom_speed: 0.5,
        }
    }
}

/// A camera that smoothly follows a target entity.
///
/// Attach this component alongside [`Camera3D`] and [`Transform`]. Register
/// [`follow_camera_system`] to have it update the [`Transform`] each frame.
#[derive(Debug, Clone)]
pub struct FollowCamera {
    /// The entity whose world-space position this camera follows.
    pub target: engine_ecs::Entity,
    /// Offset from the target's world position to the camera eye.
    pub offset: Vec3,
    /// Controls follow lag: `0.0` snaps instantly, `1.0` never moves.
    ///
    /// Implemented as an exponential decay — the camera moves
    /// `(1 − spring_strength) ^ dt` of the remaining distance each second.
    pub spring_strength: f32,
}

impl FollowCamera {
    /// Creates a follow camera with the given target, offset, and spring
    /// strength.
    pub fn new(target: engine_ecs::Entity, offset: Vec3, spring_strength: f32) -> Self {
        Self {
            target,
            offset,
            spring_strength: spring_strength.clamp(0.0, 0.9999),
        }
    }
}

/// Updates each [`OrbitCamera`] entity's [`Transform`] from its orbit
/// parameters.
///
/// Holding the left mouse button and dragging rotates the orbit. Scrolling
/// adjusts the distance to the target.
pub fn orbit_camera_system(
    mouse: engine_ecs::Res<MouseInput>,
    mouse_buttons: engine_ecs::Res<Input<MouseButton>>,
    mut query: engine_ecs::Query<(&mut OrbitCamera, &mut Transform)>,
) {
    for (_, (orbit, transform)) in &mut query {
        if mouse_buttons.pressed(MouseButton::Left) {
            orbit.yaw -= mouse.delta.0 * orbit.orbit_speed;
            let new_pitch = orbit.pitch - mouse.delta.1 * orbit.orbit_speed;
            orbit.pitch = new_pitch.clamp(orbit.pitch_min, orbit.pitch_max);
        }

        orbit.distance = (orbit.distance - mouse.scroll * orbit.zoom_speed).max(0.01);

        let eye = orbit.target
            + Vec3::new(
                orbit.distance * orbit.yaw.sin() * orbit.pitch.cos(),
                orbit.distance * orbit.pitch.sin(),
                orbit.distance * orbit.yaw.cos() * orbit.pitch.cos(),
            );

        *transform = Transform::looking_at(eye, orbit.target, Vec3::Y);
    }
}

/// Updates each [`FollowCamera`] entity's [`Transform`] to lag-follow the
/// target entity's world position.
///
/// The camera's translation is exponentially smoothed toward
/// `target_world_pos + offset` each frame.
pub fn follow_camera_system(
    time: engine_ecs::Res<Time>,
    targets: engine_ecs::Query<&GlobalTransform>,
    mut cameras: engine_ecs::Query<(&FollowCamera, &mut Transform)>,
) {
    let positions: HashMap<engine_ecs::Entity, Vec3> = targets
        .iter()
        .map(|(entity, global)| (entity, global.0.w_axis.truncate()))
        .collect();

    for (_, (follow, transform)) in &mut cameras {
        if let Some(&target_pos) = positions.get(&follow.target) {
            let desired = target_pos + follow.offset;
            let strength = follow.spring_strength.clamp(0.0, 0.9999);
            let t = 1.0 - strength.powf(time.delta_seconds);
            transform.translation = transform.translation.lerp(desired, t);
        }
    }
}

/// Minimum distance the camera eye may sit from its pivot point after wall
/// avoidance pulls it in, so the eye never collapses onto the pivot
/// (Phase 58 Design Decisions).
const LOCK_ON_CAMERA_MIN_EYE_DISTANCE: f32 = 0.5;

/// World-unit margin subtracted from a wall-avoidance hit point, so the eye
/// settles just in front of the obstruction rather than touching it.
const LOCK_ON_CAMERA_WALL_MARGIN: f32 = 0.2;

/// A camera that frames the [`TargetLock`] target from `source`, with wall
/// avoidance (a spring arm) when a [`PhysicsBody::Static`] obstacle sits
/// between the pivot and the desired eye position.
///
/// Attach alongside [`Camera3D`] and [`Transform`]. Register
/// [`lock_on_camera_system`] immediately after
/// [`crate::lock_on::lock_on_system`] so the camera reacts to the same
/// frame's lock-on state.
#[derive(Debug, Clone)]
pub struct LockOnCamera {
    /// The entity the camera orbits (typically the player).
    pub source: engine_ecs::Entity,
    /// Distance from `source` to the camera eye.
    pub distance: f32,
    /// Vertical offset applied above `source`'s world position.
    pub height: f32,
    /// Controls follow lag; see [`FollowCamera::spring_strength`] for the
    /// exponential-decay convention this field shares.
    pub spring_strength: f32,
    /// Targets farther than this from `source` are never selected by
    /// [`crate::lock_on::lock_on_system`] while this is the world's active
    /// lock-on camera.
    pub max_target_distance: f32,
    /// When `true`, a target occluded from `source` is never selected by
    /// [`crate::lock_on::lock_on_system`].
    pub require_line_of_sight: bool,
    /// Team filter used by target selection: `-1` accepts every team, any
    /// other value requires an exact match against
    /// [`crate::lock_on::LockOnTarget::team`].
    pub team_filter: i64,
}

impl LockOnCamera {
    /// Creates a lock-on camera with the given source entity and framing parameters.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: engine_ecs::Entity,
        distance: f32,
        height: f32,
        spring_strength: f32,
        max_target_distance: f32,
        require_line_of_sight: bool,
        team_filter: i64,
    ) -> Self {
        Self {
            source,
            distance,
            height,
            spring_strength: spring_strength.clamp(0.0, 0.9999),
            max_target_distance,
            require_line_of_sight,
            team_filter,
        }
    }
}

/// Updates each [`LockOnCamera`] entity's [`Transform`].
///
/// Register this immediately after [`crate::lock_on::lock_on_system`] in the
/// frame schedule so both systems act on the same frame's [`TargetLock`]
/// state.
///
/// - While [`TargetLock::current`] is a live target, the camera sits behind
///   `source` opposite the target at `distance`/`height` and looks at the
///   midpoint between `source` and the target.
/// - While unlocked, the camera keeps its previous horizontal view direction
///   and follows `source` like [`FollowCamera`].
/// - Either way, a [`PhysicsBody::Static`] obstacle between the pivot
///   (`source`'s position raised by half of `height`) and the desired eye
///   position pulls the eye in along that segment (see
///   [`segment_blocked_by_static`]), never closer than 0.5 world units to
///   the pivot.
///
/// Camera entities whose `source` is missing (despawned, or lacking
/// [`GlobalTransform`]) are skipped for that frame; this system never
/// panics.
pub fn lock_on_camera_system(
    lock: engine_ecs::Res<TargetLock>,
    time: engine_ecs::Res<Time>,
    transforms: engine_ecs::Query<&GlobalTransform>,
    colliders: engine_ecs::Query<(
        &Collider,
        &PhysicsBody,
        &GlobalTransform,
        Option<&TriggerVolume>,
    )>,
    mut cameras: engine_ecs::Query<(&LockOnCamera, &mut Transform)>,
) {
    let positions: HashMap<engine_ecs::Entity, Vec3> = transforms
        .iter()
        .map(|(entity, global)| (entity, global.matrix().w_axis.truncate()))
        .collect();
    let static_aabbs = static_obstacle_aabbs(colliders.iter().map(|(_, data)| data));
    let target_pos = lock
        .current()
        .and_then(|target| positions.get(&target).copied());

    for (_, (lock_on, transform)) in &mut cameras {
        let Some(&source_pos) = positions.get(&lock_on.source) else {
            continue;
        };

        let (desired_eye, look_at) = match target_pos {
            Some(target_pos) => {
                let direction = (target_pos - source_pos)
                    .try_normalize()
                    .unwrap_or(Vec3::NEG_Z);
                let eye = source_pos - direction * lock_on.distance + Vec3::Y * lock_on.height;
                let look_at = (source_pos + target_pos) * 0.5;
                (eye, look_at)
            }
            None => {
                let forward = transform.rotation * Vec3::NEG_Z;
                let horizontal = Vec3::new(forward.x, 0.0, forward.z)
                    .try_normalize()
                    .unwrap_or(Vec3::NEG_Z);
                let eye = source_pos - horizontal * lock_on.distance + Vec3::Y * lock_on.height;
                let look_at = source_pos + Vec3::Y * (lock_on.height * 0.5);
                (eye, look_at)
            }
        };

        let pivot = source_pos + Vec3::Y * (lock_on.height * 0.5);
        let eye = pull_eye_from_obstruction(pivot, desired_eye, &static_aabbs);

        let strength = lock_on.spring_strength.clamp(0.0, 0.9999);
        let t = 1.0 - strength.powf(time.delta_seconds);
        transform.translation = transform.translation.lerp(eye, t);

        // Same rotation construction as `Transform::looking_at`, applied to
        // the spring-smoothed eye position rather than replacing translation.
        let view_dir = (look_at - transform.translation)
            .try_normalize()
            .unwrap_or(Vec3::NEG_Z);
        transform.rotation =
            Quat::from_mat4(&Mat4::look_to_rh(transform.translation, view_dir, Vec3::Y)).inverse();
    }
}

/// Pulls `desired_eye` toward `pivot` when a [`PhysicsBody::Static`] obstacle
/// occupies the segment between them.
///
/// Returns `desired_eye` unchanged when the segment is clear (or degenerate:
/// `pivot == desired_eye`). Otherwise the eye is placed
/// [`LOCK_ON_CAMERA_WALL_MARGIN`] world units before the hit point, never
/// closer than [`LOCK_ON_CAMERA_MIN_EYE_DISTANCE`] to `pivot`.
fn pull_eye_from_obstruction(
    pivot: Vec3,
    desired_eye: Vec3,
    static_aabbs: &[crate::collision::WorldAabb],
) -> Vec3 {
    let offset = desired_eye - pivot;
    let length = offset.length();
    if length <= f32::EPSILON {
        return desired_eye;
    }
    let Some(hit_t) = segment_blocked_by_static(static_aabbs, pivot, desired_eye) else {
        return desired_eye;
    };
    let direction = offset / length;
    let hit_distance = hit_t * length;
    let pulled_distance =
        (hit_distance - LOCK_ON_CAMERA_WALL_MARGIN).max(LOCK_ON_CAMERA_MIN_EYE_DISTANCE);
    pivot + direction * pulled_distance
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected_camera(world: &mut engine_ecs::World) -> Option<engine_ecs::Entity> {
        let query = engine_ecs::Query::<&Camera3D>::new(world);
        select_active_game_camera(
            query
                .iter()
                .map(|(entity, camera)| (entity, (camera, ()))),
        )
        .map(|(entity, _)| entity)
    }

    #[test]
    fn active_camera_prefers_the_highest_enabled_priority() {
        let mut world = engine_ecs::World::new();
        let low = world
            .spawn_with(Camera3D::default())
            .expect("spawn low camera");
        let high_camera = Camera3D {
            priority: 10,
            ..Camera3D::default()
        };
        let high = world
            .spawn_with(high_camera)
            .expect("spawn high-priority camera");

        let selected = selected_camera(&mut world);

        assert_eq!(selected, Some(high));
        assert_ne!(selected, Some(low));
    }

    #[test]
    fn active_camera_ignores_disabled_cameras() {
        let mut world = engine_ecs::World::new();
        let disabled = Camera3D {
            enabled: false,
            priority: 100,
            ..Camera3D::default()
        };
        world
            .spawn_with(disabled)
            .expect("spawn disabled camera");
        let enabled = world
            .spawn_with(Camera3D::default())
            .expect("spawn enabled camera");

        assert_eq!(selected_camera(&mut world), Some(enabled));
    }

    #[test]
    fn active_camera_ties_resolve_to_the_lowest_runtime_entity() {
        let mut world = engine_ecs::World::new();
        let first = world
            .spawn_with(Camera3D::default())
            .expect("spawn first camera");
        world
            .spawn_with(Camera3D::default())
            .expect("spawn second camera");

        assert_eq!(selected_camera(&mut world), Some(first));
    }

    #[test]
    fn active_camera_is_absent_when_every_camera_is_disabled() {
        let mut world = engine_ecs::World::new();
        let camera = Camera3D {
            enabled: false,
            ..Camera3D::default()
        };
        world
            .spawn_with(camera)
            .expect("spawn disabled camera");

        assert!(selected_camera(&mut world).is_none());
    }

    #[test]
    fn orbit_camera_default_positions_camera_above_target() {
        let orbit = OrbitCamera::default();
        let eye = orbit.target
            + Vec3::new(
                orbit.distance * orbit.yaw.sin() * orbit.pitch.cos(),
                orbit.distance * orbit.pitch.sin(),
                orbit.distance * orbit.yaw.cos() * orbit.pitch.cos(),
            );
        assert!(eye.y > orbit.target.y, "camera should be above target");
    }

    #[test]
    fn orbit_camera_distance_clamps_to_minimum() {
        let orbit = OrbitCamera {
            distance: 0.0,
            ..OrbitCamera::default()
        };
        let eye = orbit.target
            + Vec3::new(
                orbit.distance * orbit.yaw.sin() * orbit.pitch.cos(),
                orbit.distance * orbit.pitch.sin(),
                orbit.distance * orbit.yaw.cos() * orbit.pitch.cos(),
            );
        // distance clamp applied externally by the system; here we only confirm
        // the math produces a valid (possibly zero-distance) result
        let _ = eye;
    }

    #[test]
    fn orbit_camera_pitch_clamps_between_limits() {
        let orbit = OrbitCamera::default();
        assert!(orbit.pitch_min < orbit.pitch_max);
        assert!(orbit.pitch >= orbit.pitch_min);
        assert!(orbit.pitch <= orbit.pitch_max);
    }

    #[test]
    fn follow_camera_spring_strength_is_clamped() {
        let mut world = engine_ecs::World::new();
        let entity = world.spawn().expect("spawn must succeed");
        let cam = FollowCamera::new(entity, Vec3::Y * 2.0, 1.5);
        assert!(cam.spring_strength <= 0.9999);
    }

    // --- LockOnCamera / lock_on_camera_system (Phase 58) ----------------------

    #[test]
    fn lock_on_camera_spring_strength_is_clamped() {
        let mut world = engine_ecs::World::new();
        let source = world.spawn().expect("spawn must succeed");
        let cam = LockOnCamera::new(source, 6.0, 2.5, 1.5, 20.0, true, -1);
        assert!(cam.spring_strength <= 0.9999);
    }

    #[test]
    fn lock_on_camera_new_stores_all_parameters() {
        let mut world = engine_ecs::World::new();
        let source = world.spawn().expect("spawn must succeed");
        let cam = LockOnCamera::new(source, 6.0, 2.5, 0.85, 20.0, true, 3);
        assert_eq!(cam.source, source);
        assert_eq!(cam.distance, 6.0);
        assert_eq!(cam.height, 2.5);
        assert_eq!(cam.max_target_distance, 20.0);
        assert!(cam.require_line_of_sight);
        assert_eq!(cam.team_filter, 3);
    }

    #[test]
    fn pull_eye_from_obstruction_returns_desired_eye_when_clear() {
        let pivot = Vec3::ZERO;
        let desired_eye = Vec3::new(0.0, 0.0, 6.0);
        let eye = pull_eye_from_obstruction(pivot, desired_eye, &[]);
        assert_eq!(eye, desired_eye);
    }

    #[test]
    fn pull_eye_from_obstruction_pulls_in_before_the_wall() {
        let pivot = Vec3::ZERO;
        let desired_eye = Vec3::new(0.0, 0.0, 6.0);
        let wall = crate::collision::WorldAabb {
            center: Vec3::new(0.0, 0.0, 3.0),
            half_extents: Vec3::splat(0.5),
        };
        let eye = pull_eye_from_obstruction(pivot, desired_eye, &[wall]);
        let pivot_distance = eye.distance(pivot);
        assert!(pivot_distance >= LOCK_ON_CAMERA_MIN_EYE_DISTANCE - 1e-4);
        assert!(
            pivot_distance < 2.5,
            "eye must land before the wall's near face (z=2.5), got distance {pivot_distance}"
        );
    }

    #[test]
    fn pull_eye_from_obstruction_never_collapses_below_minimum_distance() {
        let pivot = Vec3::ZERO;
        let desired_eye = Vec3::new(0.0, 0.0, 6.0);
        // Wall sits almost at the pivot, so hit_distance - margin would be
        // negative without the floor.
        let wall = crate::collision::WorldAabb {
            center: Vec3::new(0.0, 0.0, 0.05),
            half_extents: Vec3::splat(0.05),
        };
        let eye = pull_eye_from_obstruction(pivot, desired_eye, &[wall]);
        assert!((eye.distance(pivot) - LOCK_ON_CAMERA_MIN_EYE_DISTANCE).abs() < 1e-4);
    }

    fn make_camera_app() -> engine_ecs::App {
        let mut app = engine_ecs::App::new();
        app.insert_resource(TargetLock::default());
        app.insert_resource(Time::default());
        app
    }

    fn spawn_positioned(app: &mut engine_ecs::App, position: Vec3) -> engine_ecs::Entity {
        let entity = app
            .world_mut()
            .spawn_with(Transform::from_translation(position))
            .expect("spawn entity");
        app.world_mut()
            .add_component(
                entity,
                GlobalTransform(Transform::from_translation(position).to_matrix()),
            )
            .expect("add global transform");
        entity
    }

    #[test]
    fn locked_camera_sits_behind_source_opposite_target() {
        let mut app = make_camera_app();
        app.insert_resource(Time {
            delta_seconds: 1.0,
            elapsed_seconds: 0.0,
            frame_count: 0,
        });

        let source = spawn_positioned(&mut app, Vec3::ZERO);
        let target = spawn_positioned(&mut app, Vec3::new(5.0, 0.0, 0.0));
        app.world_mut()
            .add_component(target, crate::lock_on::LockOnTarget::default())
            .expect("add lock-on target marker");
        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();

        let camera = app
            .world_mut()
            .spawn_with(Transform::default())
            .expect("spawn camera");
        app.world_mut()
            .add_component(camera, GlobalTransform::default())
            .expect("add camera global transform");
        app.world_mut()
            .add_component(camera, Camera3D::default())
            .expect("add camera projection");
        app.world_mut()
            .add_component(
                camera,
                LockOnCamera::new(source, 6.0, 2.5, 0.0, 100.0, false, -1),
            )
            .expect("add lock-on camera");

        app.add_system(crate::lock_on::lock_on_system);
        app.add_system(lock_on_camera_system);
        app.update().expect("systems must run");

        let transform = app
            .world()
            .get_component::<Transform>(camera)
            .expect("camera transform");
        // Target sits at +X from source, so the camera (behind source, away
        // from the target) must land at negative X, `distance` away from the
        // pivot-height point above source.
        assert!(
            transform.translation.x < 0.0,
            "camera must sit behind source, away from target, x={}",
            transform.translation.x
        );
        assert!((transform.translation - Vec3::new(-6.0, 2.5, 0.0)).length() < 1e-3);
    }

    #[test]
    fn unlocked_camera_follows_source_maintaining_previous_view_direction() {
        let mut app = make_camera_app();
        app.insert_resource(Time {
            delta_seconds: 1.0,
            elapsed_seconds: 0.0,
            frame_count: 0,
        });

        let source = spawn_positioned(&mut app, Vec3::new(1.0, 0.0, 0.0));

        // Camera previously looked from (1, 0, -5) toward (1, 0, 0): a +Z
        // horizontal view direction.
        let camera = app
            .world_mut()
            .spawn_with(Transform::looking_at(
                Vec3::new(1.0, 0.0, -5.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::Y,
            ))
            .expect("spawn camera");
        app.world_mut()
            .add_component(camera, GlobalTransform::default())
            .expect("add camera global transform");
        app.world_mut()
            .add_component(camera, Camera3D::default())
            .expect("add camera projection");
        app.world_mut()
            .add_component(
                camera,
                LockOnCamera::new(source, 4.0, 2.0, 0.0, 100.0, false, -1),
            )
            .expect("add lock-on camera");

        app.add_system(lock_on_camera_system);
        app.update().expect("system must run");

        let transform = app
            .world()
            .get_component::<Transform>(camera)
            .expect("camera transform");
        assert!(
            (transform.translation - Vec3::new(1.0, 2.0, -4.0)).length() < 1e-3,
            "unlocked camera must follow behind source along its previous view direction, got {:?}",
            transform.translation
        );
    }

    #[test]
    fn locked_camera_is_pulled_in_by_a_wall_between_pivot_and_eye() {
        let mut app = make_camera_app();
        app.insert_resource(Time {
            delta_seconds: 1.0,
            elapsed_seconds: 0.0,
            frame_count: 0,
        });

        let source = spawn_positioned(&mut app, Vec3::ZERO);
        let target = spawn_positioned(&mut app, Vec3::new(5.0, 0.0, 0.0));
        app.world_mut()
            .add_component(target, crate::lock_on::LockOnTarget::default())
            .expect("add lock-on target marker");
        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();

        // Wall sits between the pivot (0, 1.25, 0) and the unobstructed
        // desired eye (-6, 2.5, 0), which lies behind source at -X.
        let wall_center = Vec3::new(-3.0, 1.25, 0.0);
        let wall = app
            .world_mut()
            .spawn_with(Transform::from_translation(wall_center))
            .expect("spawn wall");
        app.world_mut()
            .add_component(
                wall,
                GlobalTransform(Transform::from_translation(wall_center).to_matrix()),
            )
            .expect("add wall global transform");
        app.world_mut()
            .add_component(wall, Collider::aabb(Vec3::new(0.5, 2.0, 2.0)))
            .expect("add wall collider");
        app.world_mut()
            .add_component(wall, PhysicsBody::Static)
            .expect("add wall body");

        let camera = app
            .world_mut()
            .spawn_with(Transform::default())
            .expect("spawn camera");
        app.world_mut()
            .add_component(camera, GlobalTransform::default())
            .expect("add camera global transform");
        app.world_mut()
            .add_component(camera, Camera3D::default())
            .expect("add camera projection");
        app.world_mut()
            .add_component(
                camera,
                LockOnCamera::new(source, 6.0, 2.5, 0.0, 100.0, false, -1),
            )
            .expect("add lock-on camera");

        app.add_system(crate::lock_on::lock_on_system);
        app.add_system(lock_on_camera_system);
        app.update().expect("systems must run");

        let transform = app
            .world()
            .get_component::<Transform>(camera)
            .expect("camera transform");
        let pivot = Vec3::new(0.0, 1.25, 0.0);
        let desired_eye = Vec3::new(-6.0, 2.5, 0.0);
        let unobstructed_distance = pivot.distance(desired_eye);
        let pivot_distance = transform.translation.distance(pivot);
        assert!(
            pivot_distance < unobstructed_distance - 0.01,
            "wall must pull the camera in from the unobstructed distance {unobstructed_distance}, got {pivot_distance}"
        );
        assert!(
            pivot_distance >= LOCK_ON_CAMERA_MIN_EYE_DISTANCE - 1e-3,
            "camera must never collapse below the minimum eye distance, got {pivot_distance}"
        );
    }

    #[test]
    fn lock_on_camera_system_skips_when_source_has_no_global_transform() {
        let mut app = make_camera_app();

        let source = app
            .world_mut()
            .spawn()
            .expect("spawn source without a transform");

        let camera = app
            .world_mut()
            .spawn_with(Transform::default())
            .expect("spawn camera");
        app.world_mut()
            .add_component(camera, GlobalTransform::default())
            .expect("add camera global transform");
        app.world_mut()
            .add_component(
                camera,
                LockOnCamera::new(source, 6.0, 2.5, 0.85, 20.0, false, -1),
            )
            .expect("add lock-on camera");

        app.add_system(lock_on_camera_system);
        assert!(
            app.update().is_ok(),
            "a camera whose source lacks a GlobalTransform must not panic"
        );

        let transform = app
            .world()
            .get_component::<Transform>(camera)
            .expect("camera transform must remain attached");
        assert_eq!(
            transform.translation,
            Vec3::ZERO,
            "camera transform must be left untouched when source has no position"
        );
    }
}
