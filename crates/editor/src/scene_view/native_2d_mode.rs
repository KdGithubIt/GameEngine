//! Transient Native 2D Scene View camera and grid presentation.

use super::{EditorViewCamera, GizmoMode};
use eframe::egui;
use engine::glam::{Mat4, Vec3};
use engine::{DebugLines, Transform};

/// Transient Scene View projection mode. This is never persisted into scene data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SceneViewMode {
    /// Perspective orbit/fly editing.
    #[default]
    ThreeD,
    /// Orthographic XY authoring using the runtime Camera2D projection contract.
    TwoD,
}

impl SceneViewMode {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ThreeD => "3D",
            Self::TwoD => "2D",
        }
    }
}

pub(crate) fn camera(camera: &EditorViewCamera) -> engine::native_2d::Camera2d {
    engine::native_2d::Camera2d {
        orthographic_height: (camera.distance.max(0.5) * 1.154_700_5).max(0.01),
        ..engine::native_2d::Camera2d::default()
    }
}

pub(crate) fn transform(camera: &EditorViewCamera) -> Transform {
    Transform::from_translation(Vec3::new(camera.target.x, camera.target.y, 100.0))
}

pub(crate) fn view_projection(camera_state: &EditorViewCamera, viewport: egui::Rect) -> Mat4 {
    let width = viewport.width().round().max(1.0) as u32;
    let height = viewport.height().round().max(1.0) as u32;
    camera(camera_state)
        .view_projection_matrix(&transform(camera_state), [width, height])
        .unwrap_or(Mat4::IDENTITY)
}

pub(crate) fn handle_input(camera_state: &mut EditorViewCamera, response: &egui::Response) {
    let rect = response.rect;
    if rect.height() <= f32::EPSILON {
        return;
    }
    let delta = response.ctx.input(|input| input.pointer.delta());
    if response.dragged_by(egui::PointerButton::Middle)
        || response.dragged_by(egui::PointerButton::Secondary)
    {
        let units_per_point = camera(camera_state).orthographic_height / rect.height();
        camera_state.target.x -= delta.x * units_per_point;
        camera_state.target.y += delta.y * units_per_point;
    }
    let scroll = if response.hovered() {
        response.ctx.input(|input| input.smooth_scroll_delta.y)
    } else {
        0.0
    };
    if scroll == 0.0 {
        return;
    }
    let pointer = response.hover_pos().unwrap_or(rect.center());
    let before = screen_to_xy(pointer, rect, view_projection(camera_state, rect));
    camera_state.distance =
        (camera_state.distance * (1.0 - scroll * 0.002)).clamp(0.5, 500.0);
    let after = screen_to_xy(pointer, rect, view_projection(camera_state, rect));
    if let (Some(before), Some(after)) = (before, after) {
        camera_state.target.x += before.x - after.x;
        camera_state.target.y += before.y - after.y;
    }
}

pub(crate) fn draw_grid(lines: &mut DebugLines, camera_state: &EditorViewCamera) {
    let span = (camera_state.distance.ceil() as i32).clamp(10, 100);
    for index in -span..=span {
        let value = index as f32;
        let x_color = if index == 0 {
            Vec3::new(0.8, 0.1, 0.1)
        } else {
            Vec3::splat(0.25)
        };
        let y_color = if index == 0 {
            Vec3::new(0.1, 0.8, 0.1)
        } else {
            Vec3::splat(0.25)
        };
        lines.line(
            Vec3::new(value, -(span as f32), 0.0),
            Vec3::new(value, span as f32, 0.0),
            x_color,
        );
        lines.line(
            Vec3::new(-(span as f32), value, 0.0),
            Vec3::new(span as f32, value, 0.0),
            y_color,
        );
    }
}

pub(crate) const fn axis_allowed(mode: GizmoMode, axis: crate::gizmo::GizmoAxis) -> bool {
    match mode {
        GizmoMode::Translate | GizmoMode::Scale => {
            matches!(axis, crate::gizmo::GizmoAxis::X | crate::gizmo::GizmoAxis::Y)
        }
        GizmoMode::Rotate => matches!(axis, crate::gizmo::GizmoAxis::Z),
    }
}

fn screen_to_xy(position: egui::Pos2, viewport: egui::Rect, vp: Mat4) -> Option<Vec3> {
    let x = ((position.x - viewport.left()) / viewport.width()) * 2.0 - 1.0;
    let y = 1.0 - ((position.y - viewport.top()) / viewport.height()) * 2.0;
    let inverse = vp.inverse();
    let near = inverse.project_point3(Vec3::new(x, y, 0.0));
    let far = inverse.project_point3(Vec3::new(x, y, 1.0));
    let direction = far - near;
    if direction.z.abs() <= f32::EPSILON {
        return None;
    }
    let distance = -near.z / direction.z;
    Some(near + direction * distance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_d_camera_is_planar_and_orthographic() {
        let state = EditorViewCamera::default();
        let transform = transform(&state);
        assert_eq!(transform.translation.x, state.target.x);
        assert_eq!(transform.translation.y, state.target.y);
        assert!(camera(&state)
            .view_projection_matrix(&transform, [1280, 720])
            .is_ok());
    }

    #[test]
    fn planar_gizmo_contract_allows_xy_and_z_rotation_only() {
        assert!(axis_allowed(GizmoMode::Translate, crate::gizmo::GizmoAxis::X));
        assert!(!axis_allowed(GizmoMode::Translate, crate::gizmo::GizmoAxis::Z));
        assert!(axis_allowed(GizmoMode::Rotate, crate::gizmo::GizmoAxis::Z));
        assert!(!axis_allowed(GizmoMode::Rotate, crate::gizmo::GizmoAxis::X));
    }
}
