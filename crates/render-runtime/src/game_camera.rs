//! Internal Camera2D/Camera3D arbitration for the shared world renderer.

use crate::camera::{
    select_game_camera_intent, Camera3D, GameCameraIntent, GameCameraKind, GameCameraSelection,
    ViewportSize,
};
use crate::native_2d::{Camera2d, Camera2dDiagnostic};
use crate::transform::Transform;

#[derive(Clone)]
pub(crate) struct PreparedCamera {
    pub(crate) view_projection: glam::Mat4,
    pub(crate) view: glam::Mat4,
    pub(crate) position: glam::Vec3,
    pub(crate) viewport_aspect: f32,
    pub(crate) shadow_camera: Option<(Camera3D, Transform)>,
}

impl PreparedCamera {
    pub(crate) fn three_d(camera: Camera3D, transform: Transform) -> Self {
        Self {
            view_projection: camera.view_projection_matrix(&transform),
            view: Camera3D::view_matrix(&transform),
            position: transform.translation,
            viewport_aspect: valid_aspect(camera.aspect),
            shadow_camera: Some((camera, transform)),
        }
    }

    pub(crate) fn two_d(
        camera: Camera2d,
        transform: Transform,
        viewport: [u32; 2],
    ) -> Result<Self, Camera2dDiagnostic> {
        Ok(Self {
            view_projection: camera.view_projection_matrix(&transform, viewport)?,
            view: transform.to_matrix().inverse(),
            position: transform.translation,
            viewport_aspect: valid_aspect(
                viewport[0].max(1) as f32 / viewport[1].max(1) as f32,
            ),
            shadow_camera: None,
        })
    }
}

pub(crate) fn active_camera(
    world: &mut engine_ecs::World,
) -> Result<Option<PreparedCamera>, Camera2dDiagnostic> {
    let viewport = world
        .get_resource::<ViewportSize>()
        .map(|value| [value.width.max(1), value.height.max(1)])
        .unwrap_or([1280, 720]);
    let three_d = {
        let query = engine_ecs::Query::<(&Camera3D, &Transform)>::new(world);
        query
            .iter()
            .map(|(entity, (camera, transform))| (entity, camera.clone(), transform.clone()))
            .collect::<Vec<_>>()
    };
    let two_d = {
        let query = engine_ecs::Query::<(&Camera2d, &Transform)>::new(world);
        query
            .iter()
            .map(|(entity, (camera, transform))| (entity, *camera, transform.clone()))
            .collect::<Vec<_>>()
    };
    let selection = select_game_camera_intent(
        three_d
            .iter()
            .map(|(entity, camera, _)| GameCameraIntent {
                entity: *entity,
                kind: GameCameraKind::ThreeD,
                enabled: camera.enabled,
                priority: camera.priority,
            })
            .chain(two_d.iter().map(|(entity, camera, _)| GameCameraIntent {
                entity: *entity,
                kind: GameCameraKind::TwoD,
                enabled: camera.enabled,
                priority: camera.priority,
            })),
    );
    let GameCameraSelection::Selected(selected) = selection else {
        return Ok(None);
    };
    match selected.kind {
        GameCameraKind::ThreeD => Ok(three_d
            .into_iter()
            .find(|(entity, _, _)| *entity == selected.entity)
            .map(|(_, camera, transform)| PreparedCamera::three_d(camera, transform))),
        GameCameraKind::TwoD => two_d
            .into_iter()
            .find(|(entity, _, _)| *entity == selected.entity)
            .map(|(_, camera, transform)| PreparedCamera::two_d(camera, transform, viewport))
            .transpose(),
    }
}

fn valid_aspect(aspect: f32) -> f32 {
    if aspect.is_finite() && aspect > 0.0 {
        aspect
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_priority_camera_2d_wins_shared_arbitration() {
        let mut world = engine_ecs::World::new();
        let three_d = world.spawn().expect("spawn Camera3D");
        world.add_component(three_d, Camera3D::default()).expect("Camera3D");
        world.add_component(three_d, Transform::default()).expect("3D transform");
        let two_d = world.spawn().expect("spawn Camera2D");
        world.add_component(two_d, Camera2d { priority: 5, ..Camera2d::default() }).expect("Camera2D");
        world.add_component(two_d, Transform::from_translation(glam::Vec3::new(0.0, 0.0, 10.0))).expect("2D transform");
        let prepared = active_camera(&mut world)
            .expect("valid camera")
            .expect("selected camera");
        assert!(prepared.shadow_camera.is_none());
    }
}
