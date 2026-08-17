//! Lock-on target selection, persistence, and validation (Phase 58).
//!
//! [`LockOnTarget`] marks an entity as selectable. [`TargetLock`] tracks the
//! currently locked entity and queues Acquire/Cycle/Release requests for
//! [`lock_on_system`] to process once per frame, using the world's
//! [`crate::camera::LockOnCamera`] for selection parameters.
//! [`crate::camera::lock_on_camera_system`] reads [`TargetLock::current`] to
//! frame the locked target and should be registered immediately after
//! [`lock_on_system`] in the frame schedule.

use engine_ecs::{Entity, Query, Res, ResMut};
use glam::Vec3;
use hashbrown::HashMap;

use crate::camera::{
    select_active_game_camera_with_override, Camera3D, GameCameraSelectionOverride, LockOnCamera,
};
use crate::collision::{
    segment_blocked_by_static, static_obstacle_aabbs, Collider, PhysicsBody, TriggerVolume,
};
use crate::transform::GlobalTransform;

pub use engine_gameplay::lock_on::{LockOnTarget, TargetLock};
use engine_gameplay::lock_on::LockRequest;

/// Processes the pending [`TargetLock`] request and validates the current
/// lock, using the world's first [`LockOnCamera`] for selection parameters
/// (`source`, `max_target_distance`, `require_line_of_sight`, `team_filter`).
///
/// Register this in the frame schedule, immediately before
/// [`crate::camera::lock_on_camera_system`], so the camera reacts to the
/// same frame's selection.
///
/// # Selection
///
/// A candidate entity is valid when it carries [`LockOnTarget`], is not the
/// camera's `source`, passes `team_filter` (`-1` accepts every team; any
/// other value requires an exact match against [`LockOnTarget::team`]), lies
/// within `max_target_distance` of `source`'s world position, and — when
/// `require_line_of_sight` is `true` — has an unoccluded segment from
/// `source` to the candidate (see [`segment_blocked_by_static`]).
///
/// - **Acquire** locks onto the nearest valid candidate.
/// - **Cycle** advances to the next valid candidate in ascending-distance
///   order, wrapping from the last candidate back to the nearest. With no
///   current lock this behaves like Acquire.
/// - **Release** clears the current lock.
///
/// Every run also re-validates the current lock (despawned, out of range, or
/// newly occluded) and clears it when it is no longer valid, independent of
/// any pending request.
///
/// # No camera
///
/// When the active game camera has no [`LockOnCamera`], or its `source` entity
/// has no [`GlobalTransform`], the pending request is cleared and logged via
/// `log::debug!` without changing [`TargetLock::current`]: without that
/// configuration there is no `source` position or selection policy to use.
pub fn lock_on_system(
    mut lock: ResMut<TargetLock>,
    camera_override: Option<Res<GameCameraSelectionOverride>>,
    cameras: Query<(&Camera3D, Option<&LockOnCamera>)>,
    lock_targets: Query<&LockOnTarget>,
    transforms: Query<&GlobalTransform>,
    colliders: Query<(
        &Collider,
        &PhysicsBody,
        &GlobalTransform,
        Option<&TriggerVolume>,
    )>,
) {
    let request = lock.pending.take();

    let override_target = camera_override
        .as_deref()
        .and_then(GameCameraSelectionOverride::target);
    let Some((_, (_, Some(camera)))) =
        select_active_game_camera_with_override(cameras.iter(), override_target)
    else {
        log::debug!(
            "lock_on_system: active camera has no LockOnCamera; lock-on request ignored"
        );
        return;
    };

    let positions: HashMap<Entity, Vec3> = transforms
        .iter()
        .map(|(entity, global)| (entity, global.matrix().w_axis.truncate()))
        .collect();

    let Some(&source_pos) = positions.get(&camera.source) else {
        log::debug!(
            "lock_on_system: LockOnCamera source has no GlobalTransform; lock-on request ignored"
        );
        return;
    };

    let static_aabbs = if camera.require_line_of_sight {
        static_obstacle_aabbs(colliders.iter().map(|(_, data)| data))
    } else {
        Vec::new()
    };

    let is_valid = |candidate: Entity, target: &LockOnTarget| -> Option<f32> {
        if candidate == camera.source {
            return None;
        }
        if camera.team_filter != -1 && i64::from(target.team) != camera.team_filter {
            return None;
        }
        let target_pos = *positions.get(&candidate)?;
        let distance = source_pos.distance(target_pos);
        if distance > camera.max_target_distance {
            return None;
        }
        if camera.require_line_of_sight
            && segment_blocked_by_static(&static_aabbs, source_pos, target_pos).is_some()
        {
            return None;
        }
        Some(distance)
    };

    if let Some(current) = lock.current {
        let mut still_valid = false;
        for (entity, target) in lock_targets.iter() {
            if entity == current {
                still_valid = is_valid(entity, target).is_some();
                break;
            }
        }
        if !still_valid {
            lock.current = None;
        }
    }

    match request {
        None => {}
        Some(LockRequest::Release) => lock.current = None,
        Some(LockRequest::Acquire) => {
            lock.current = lock_targets
                .iter()
                .filter_map(|(entity, target)| {
                    is_valid(entity, target).map(|distance| (entity, distance))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(entity, _)| entity);
        }
        Some(LockRequest::Cycle) => {
            let mut valid: Vec<(Entity, f32)> = lock_targets
                .iter()
                .filter_map(|(entity, target)| {
                    is_valid(entity, target).map(|distance| (entity, distance))
                })
                .collect();
            valid.sort_by(|a, b| a.1.total_cmp(&b.1));
            lock.current = if valid.is_empty() {
                None
            } else {
                let next_index = lock
                    .current
                    .and_then(|current| valid.iter().position(|(entity, _)| *entity == current))
                    .map_or(0, |index| (index + 1) % valid.len());
                Some(valid[next_index].0)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::Transform;
    use engine_ecs::App;

    fn make_app() -> App {
        let mut app = App::new();
        app.insert_resource(TargetLock::default());
        app.add_system(lock_on_system);
        app
    }

    fn spawn_positioned(app: &mut App, position: Vec3) -> Entity {
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

    fn spawn_camera(
        app: &mut App,
        source: Entity,
        max_target_distance: f32,
        require_los: bool,
        team_filter: i64,
    ) -> Entity {
        let camera = spawn_positioned(app, Vec3::ZERO);
        app.world_mut()
            .add_component(camera, Camera3D::default())
            .expect("add camera projection");
        app.world_mut()
            .add_component(
                camera,
                LockOnCamera::new(
                    source,
                    6.0,
                    2.5,
                    0.85,
                    max_target_distance,
                    require_los,
                    team_filter,
                ),
            )
            .expect("add lock-on camera");
        camera
    }

    fn spawn_target(app: &mut App, position: Vec3, team: u32) -> Entity {
        let entity = spawn_positioned(app, position);
        app.world_mut()
            .add_component(entity, LockOnTarget { team })
            .expect("add lock-on target");
        entity
    }

    fn spawn_static_wall(app: &mut App, position: Vec3, half_extents: Vec3) -> Entity {
        let wall = spawn_positioned(app, position);
        app.world_mut()
            .add_component(wall, Collider::aabb(half_extents))
            .expect("add wall collider");
        app.world_mut()
            .add_component(wall, PhysicsBody::Static)
            .expect("add wall body");
        wall
    }

    fn run(app: &mut App) {
        app.update().expect("lock_on_system must run");
    }

    #[test]
    fn acquire_locks_onto_nearest_valid_target() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, -1);
        let near = spawn_target(&mut app, Vec3::new(2.0, 0.0, 0.0), 0);
        spawn_target(&mut app, Vec3::new(5.0, 0.0, 0.0), 0);

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .expect("target lock resource")
            .request_acquire();
        run(&mut app);

        let lock = app.world().get_resource::<TargetLock>().unwrap();
        assert_eq!(lock.current(), Some(near));
    }

    #[test]
    fn lock_on_uses_only_the_active_game_camera_configuration() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, -1);
        let target = spawn_target(&mut app, Vec3::new(2.0, 0.0, 0.0), 0);

        let active = spawn_positioned(&mut app, Vec3::ZERO);
        let active_camera = Camera3D {
            priority: 10,
            ..Camera3D::default()
        };
        app.world_mut()
            .add_component(active, active_camera)
            .expect("add active camera projection");

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .expect("target lock resource")
            .request_acquire();
        run(&mut app);
        assert_eq!(
            app.world()
                .get_resource::<TargetLock>()
                .expect("target lock resource")
                .current(),
            None,
            "a standby LockOnCamera must not configure the active camera"
        );

        app.world_mut()
            .get_component_mut::<Camera3D>(active)
            .expect("active camera projection")
            .enabled = false;
        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .expect("target lock resource")
            .request_acquire();
        run(&mut app);
        assert_eq!(
            app.world()
                .get_resource::<TargetLock>()
                .expect("target lock resource")
                .current(),
            Some(target)
        );
    }

    #[test]
    fn cycle_advances_through_targets_and_wraps() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, -1);
        let near = spawn_target(&mut app, Vec3::new(1.0, 0.0, 0.0), 0);
        let mid = spawn_target(&mut app, Vec3::new(2.0, 0.0, 0.0), 0);
        let far = spawn_target(&mut app, Vec3::new(3.0, 0.0, 0.0), 0);

        for expected in [near, mid, far, near] {
            app.world_mut()
                .get_resource_mut::<TargetLock>()
                .unwrap()
                .request_cycle();
            run(&mut app);
            let lock = app.world().get_resource::<TargetLock>().unwrap();
            assert_eq!(lock.current(), Some(expected));
        }
    }

    #[test]
    fn team_filter_excludes_other_teams() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, 1);
        spawn_target(&mut app, Vec3::new(1.0, 0.0, 0.0), 0);
        let allowed = spawn_target(&mut app, Vec3::new(5.0, 0.0, 0.0), 1);

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);

        let lock = app.world().get_resource::<TargetLock>().unwrap();
        assert_eq!(lock.current(), Some(allowed));
    }

    #[test]
    fn max_target_distance_excludes_far_targets() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 3.0, false, -1);
        spawn_target(&mut app, Vec3::new(10.0, 0.0, 0.0), 0);

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);

        let lock = app.world().get_resource::<TargetLock>().unwrap();
        assert_eq!(lock.current(), None);
    }

    #[test]
    fn line_of_sight_blocks_acquire_when_required() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, true, -1);
        spawn_target(&mut app, Vec3::new(5.0, 0.0, 0.0), 0);
        spawn_static_wall(&mut app, Vec3::new(2.5, 0.0, 0.0), Vec3::new(0.5, 2.0, 2.0));

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);

        let lock = app.world().get_resource::<TargetLock>().unwrap();
        assert_eq!(lock.current(), None, "occluded target must not be acquired");
    }

    #[test]
    fn line_of_sight_not_required_permits_occluded_acquire() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, -1);
        let target = spawn_target(&mut app, Vec3::new(5.0, 0.0, 0.0), 0);
        spawn_static_wall(&mut app, Vec3::new(2.5, 0.0, 0.0), Vec3::new(0.5, 2.0, 2.0));

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);

        let lock = app.world().get_resource::<TargetLock>().unwrap();
        assert_eq!(lock.current(), Some(target));
    }

    #[test]
    fn current_target_auto_releases_when_despawned() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, -1);
        let target = spawn_target(&mut app, Vec3::new(2.0, 0.0, 0.0), 0);

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);
        assert_eq!(
            app.world().get_resource::<TargetLock>().unwrap().current(),
            Some(target)
        );

        app.world_mut().despawn(target).expect("despawn target");
        run(&mut app);
        assert_eq!(
            app.world().get_resource::<TargetLock>().unwrap().current(),
            None
        );
    }

    #[test]
    fn current_target_auto_releases_when_out_of_range() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 10.0, false, -1);
        let target = spawn_target(&mut app, Vec3::new(2.0, 0.0, 0.0), 0);

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);
        assert_eq!(
            app.world().get_resource::<TargetLock>().unwrap().current(),
            Some(target)
        );

        let far_position = Vec3::new(50.0, 0.0, 0.0);
        app.world_mut()
            .get_component_mut::<Transform>(target)
            .unwrap()
            .translation = far_position;
        app.world_mut()
            .get_component_mut::<GlobalTransform>(target)
            .unwrap()
            .0 = Transform::from_translation(far_position).to_matrix();
        run(&mut app);
        assert_eq!(
            app.world().get_resource::<TargetLock>().unwrap().current(),
            None
        );
    }

    #[test]
    fn current_target_auto_releases_when_occlusion_appears() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, true, -1);
        let target = spawn_target(&mut app, Vec3::new(5.0, 0.0, 0.0), 0);

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);
        assert_eq!(
            app.world().get_resource::<TargetLock>().unwrap().current(),
            Some(target)
        );

        spawn_static_wall(&mut app, Vec3::new(2.5, 0.0, 0.0), Vec3::new(0.5, 2.0, 2.0));
        run(&mut app);
        assert_eq!(
            app.world().get_resource::<TargetLock>().unwrap().current(),
            None
        );
    }

    #[test]
    fn release_clears_current_lock() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, -1);
        spawn_target(&mut app, Vec3::new(2.0, 0.0, 0.0), 0);

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);
        assert!(app
            .world()
            .get_resource::<TargetLock>()
            .unwrap()
            .current()
            .is_some());

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_release();
        run(&mut app);
        assert_eq!(
            app.world().get_resource::<TargetLock>().unwrap().current(),
            None
        );
    }

    #[test]
    fn last_request_within_a_frame_wins_and_is_cleared_after_processing() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, -1);
        spawn_target(&mut app, Vec3::new(2.0, 0.0, 0.0), 0);

        {
            let lock = app.world_mut().get_resource_mut::<TargetLock>().unwrap();
            lock.request_acquire();
            lock.request_release();
        }
        run(&mut app);

        let lock = app.world().get_resource::<TargetLock>().unwrap();
        assert_eq!(
            lock.current(),
            None,
            "release must win over the earlier acquire"
        );
        assert!(
            lock.pending.is_none(),
            "the request must be cleared after processing"
        );
    }

    #[test]
    fn no_camera_clears_request_without_changing_current() {
        let mut app = make_app();
        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);

        let lock = app.world().get_resource::<TargetLock>().unwrap();
        assert_eq!(lock.current(), None);
        assert!(lock.pending.is_none());
    }

    #[test]
    fn source_entity_is_never_selected_as_its_own_target() {
        let mut app = make_app();
        let source = spawn_positioned(&mut app, Vec3::ZERO);
        spawn_camera(&mut app, source, 100.0, false, -1);
        app.world_mut()
            .add_component(source, LockOnTarget::default())
            .expect("attach LockOnTarget to the source itself");

        app.world_mut()
            .get_resource_mut::<TargetLock>()
            .unwrap()
            .request_acquire();
        run(&mut app);

        let lock = app.world().get_resource::<TargetLock>().unwrap();
        assert_eq!(lock.current(), None);
    }

    #[test]
    fn lock_on_target_default_team_is_zero() {
        assert_eq!(LockOnTarget::default().team, 0);
    }
}
