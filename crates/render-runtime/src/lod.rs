//! Level-of-detail (LOD) component and selection system (Phase 47).

use crate::asset::Handle;
use crate::camera::{
    select_active_game_camera_with_override, Camera3D, GameCameraSelectionOverride,
};
use crate::mesh::Mesh;
use crate::transform::GlobalTransform;
use engine_ecs::Query;

/// One detail level inside a [`LodGroup`].
pub struct LodLevel {
    /// Camera-distance threshold: this level is active when the entity is
    /// closer than `max_distance` to the camera.
    pub max_distance: f32,
    /// Mesh asset used at this detail level.
    pub mesh: Handle<Mesh>,
}

/// Selects one of several mesh assets based on camera distance.
///
/// Add this component alongside a [`Handle<Mesh>`] to enable LOD switching.
/// [`lod_selection_system`] updates the entity's [`Handle<Mesh>`] each frame.
///
/// `levels` must be sorted by `max_distance` **ascending**. The first level
/// whose `max_distance` exceeds the current camera distance is used. When no
/// level matches (camera is beyond all thresholds), the last level is used as
/// the lowest-detail fallback.
pub struct LodGroup {
    /// LOD levels, sorted by `max_distance` ascending.
    pub levels: Vec<LodLevel>,
}

/// Frame-level GPU instancing statistics updated by the render batch pass.
///
/// Insert into the world with [`Default::default()`] to enable collection.
/// Read each frame to display batch count and instance totals in debug UIs.
#[derive(Debug, Default, Clone, Copy)]
pub struct InstanceStats {
    /// Number of distinct draw batches issued this frame.
    pub batch_count: usize,
    /// Total number of instances (entities) across all batches this frame.
    pub total_instances: usize,
}

/// Updates the [`Handle<Mesh>`] of each entity with a [`LodGroup`] based on
/// its distance to the selected active [`Camera3D`].
///
/// Registered by the built-in frame schedule after transform propagation, so
/// the selected mesh is in place before GPU upload. Entities need an
/// existing [`Handle<Mesh>`] alongside the [`LodGroup`]; groups with no
/// levels are skipped.
pub fn lod_selection_system(
    camera_override: Option<engine_ecs::Res<GameCameraSelectionOverride>>,
    cameras: Query<(&Camera3D, &GlobalTransform)>,
    mut groups: Query<(&LodGroup, &GlobalTransform, &mut Handle<Mesh>)>,
) {
    let override_target = camera_override
        .as_deref()
        .and_then(GameCameraSelectionOverride::target);
    let camera_pos: glam::Vec3 =
        select_active_game_camera_with_override(cameras.iter(), override_target)
            .map(|(_, (_, tf))| tf.matrix().col(3).truncate())
            .unwrap_or(glam::Vec3::ZERO);

    for (_, (lod, tf, handle)) in groups.iter_mut() {
        let Some(lowest_detail) = lod.levels.last() else {
            continue;
        };
        let entity_pos = tf.matrix().col(3).truncate();
        let dist = entity_pos.distance(camera_pos);
        let level = lod
            .levels
            .iter()
            .find(|level| dist < level.max_distance)
            .unwrap_or(lowest_detail);
        *handle = level.mesh;
    }
}

#[cfg(test)]
mod tests {
    use engine_ecs::World;
    use glam::{Mat4, Vec3};

    use crate::asset::Assets;
    use crate::transform::Transform;

    use super::*;

    fn spawn_with_global(world: &mut World, translation: Vec3) -> engine_ecs::Entity {
        let entity = world
            .spawn_with(Transform::from_translation(translation))
            .expect("entity must spawn");
        world
            .add_component(entity, GlobalTransform(Mat4::from_translation(translation)))
            .expect("entity must accept GlobalTransform");
        entity
    }

    #[test]
    fn lod_selection_switches_mesh_by_camera_distance() {
        let mut world = World::new();
        let mut meshes = Assets::<Mesh>::default();
        let near_mesh = meshes.add(Mesh::cube());
        let far_mesh = meshes.add(Mesh::quad());

        let camera = spawn_with_global(&mut world, Vec3::ZERO);
        world
            .add_component(camera, Camera3D::default())
            .expect("camera must accept Camera3D");

        let entity = spawn_with_global(&mut world, Vec3::new(0.0, 0.0, -50.0));
        world
            .add_component(entity, near_mesh)
            .expect("entity must accept Handle<Mesh>");
        world
            .add_component(
                entity,
                LodGroup {
                    levels: vec![
                        LodLevel {
                            max_distance: 10.0,
                            mesh: near_mesh,
                        },
                        LodLevel {
                            max_distance: 100.0,
                            mesh: far_mesh,
                        },
                    ],
                },
            )
            .expect("entity must accept LodGroup");

        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), &mut world);
        app.add_system(lod_selection_system);
        app.update().expect("lod schedule must run");
        std::mem::swap(app.world_mut(), &mut world);

        let selected = world
            .get_component::<Handle<Mesh>>(entity)
            .expect("entity must keep Handle<Mesh>");
        assert_eq!(*selected, far_mesh, "distant entity must use the far mesh");
    }

    #[test]
    fn lod_selection_uses_the_highest_priority_enabled_camera() {
        let mut world = World::new();
        let mut meshes = Assets::<Mesh>::default();
        let near_mesh = meshes.add(Mesh::cube());
        let far_mesh = meshes.add(Mesh::quad());

        let low_priority_camera = spawn_with_global(&mut world, Vec3::ZERO);
        world
            .add_component(low_priority_camera, Camera3D::default())
            .expect("low-priority camera must accept Camera3D");

        let high_priority_camera =
            spawn_with_global(&mut world, Vec3::new(0.0, 0.0, -49.0));
        let high_priority = Camera3D {
            priority: 10,
            ..Camera3D::default()
        };
        world
            .add_component(high_priority_camera, high_priority)
            .expect("high-priority camera must accept Camera3D");

        let entity = spawn_with_global(&mut world, Vec3::new(0.0, 0.0, -50.0));
        world
            .add_component(entity, far_mesh)
            .expect("entity must accept Handle<Mesh>");
        world
            .add_component(
                entity,
                LodGroup {
                    levels: vec![
                        LodLevel {
                            max_distance: 10.0,
                            mesh: near_mesh,
                        },
                        LodLevel {
                            max_distance: 100.0,
                            mesh: far_mesh,
                        },
                    ],
                },
            )
            .expect("entity must accept LodGroup");

        let mut app = engine_ecs::App::new();
        std::mem::swap(app.world_mut(), &mut world);
        app.add_system(lod_selection_system);
        app.update().expect("lod schedule must run");
        std::mem::swap(app.world_mut(), &mut world);

        assert_eq!(
            world
                .get_component::<Handle<Mesh>>(entity)
                .copied()
                .expect("entity must keep Handle<Mesh>"),
            near_mesh
        );
    }
}
