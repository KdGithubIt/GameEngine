//! Camera projection and viewport contracts shared by rendering and runtime adapters.

use std::cmp::Reverse;

use glam::Mat4;

use crate::transform::Transform;

/// A perspective 3D camera component.
#[derive(Debug, Clone)]
pub struct Camera3D {
    /// Whether this camera may become the active Game View camera.
    ///
    /// Disabled cameras remain in the world and camera-controller systems may
    /// continue updating them, but gameplay presentation and screen-space
    /// queries ignore them.
    pub enabled: bool,
    /// Selection priority among enabled Game View cameras.
    ///
    /// Higher values win. Equal values are resolved by ascending runtime
    /// entity ID so selection never depends on archetype iteration order.
    pub priority: i32,
    /// The vertical field of view in radians.
    pub fov_y_radians: f32,
    /// The near clipping plane distance.
    pub near: f32,
    /// The far clipping plane distance.
    pub far: f32,
    /// The viewport width divided by its height.
    pub aspect: f32,
}

impl Camera3D {
    /// Creates a perspective camera.
    pub fn new(fov_y_degrees: f32, aspect: f32, near: f32, far: f32) -> Self {
        Self {
            enabled: true,
            priority: 0,
            fov_y_radians: fov_y_degrees.to_radians(),
            aspect,
            near,
            far,
        }
    }

    /// Returns the camera projection matrix.
    pub fn projection_matrix(&self) -> Mat4 {
        Mat4::perspective_rh(self.fov_y_radians, self.aspect, self.near, self.far)
    }

    /// Returns a view matrix derived from `transform`.
    pub fn view_matrix(transform: &Transform) -> Mat4 {
        transform.to_matrix().inverse()
    }

    /// Returns the combined view-projection matrix.
    pub fn view_projection_matrix(&self, transform: &Transform) -> Mat4 {
        self.projection_matrix() * Self::view_matrix(transform)
    }
}

impl Default for Camera3D {
    fn default() -> Self {
        Self::new(60.0, 16.0 / 9.0, 0.1, 1000.0)
    }
}

/// Transient runtime-only override for Game View camera selection.
///
/// Authoring never serializes this resource. Composition layers such as Timeline
/// may point it at one live camera without rewriting [`Camera3D::enabled`] or
/// [`Camera3D::priority`]. A missing target falls back to normal priority
/// selection so stale runtime identities cannot black out the Game View.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameCameraSelectionOverride {
    target: Option<engine_ecs::Entity>,
}

impl GameCameraSelectionOverride {
    /// Returns the currently requested runtime camera entity.
    pub fn target(&self) -> Option<engine_ecs::Entity> {
        self.target
    }

    /// Replaces the transient camera target.
    pub fn set_target(&mut self, target: engine_ecs::Entity) {
        self.target = Some(target);
    }

    /// Removes the transient override and restores normal camera selection.
    pub fn clear(&mut self) {
        self.target = None;
    }
}

/// Builds the total ordering used by every Game View camera consumer.
///
/// This is public only as a cross-crate runtime adapter contract; application
/// code should normally use [`select_active_game_camera`].
#[doc(hidden)]
pub fn camera_selection_key(
    entity_id: u32,
    entity_generation: u32,
    camera: &Camera3D,
) -> Option<(Reverse<i32>, u32, u32)> {
    camera
        .enabled
        .then_some((Reverse(camera.priority), entity_id, entity_generation))
}

/// Selects a transient override when it is live, otherwise the normal Game View camera.
///
/// The override intentionally bypasses [`Camera3D::enabled`] and priority: a
/// Timeline-owned standby camera can therefore become active without changing
/// persisted camera fields. If the requested entity is absent from `cameras`,
/// normal enabled-camera ordering is used.
#[doc(hidden)]
pub fn select_active_game_camera_with_override<'camera, T>(
    cameras: impl Iterator<Item = (engine_ecs::Entity, (&'camera Camera3D, T))>,
    override_target: Option<engine_ecs::Entity>,
) -> Option<(engine_ecs::Entity, (&'camera Camera3D, T))> {
    let mut fallback = None;
    for candidate in cameras {
        let entity = candidate.0;
        if override_target == Some(entity) {
            return Some(candidate);
        }
        let camera = candidate.1.0;
        let Some(key) = camera_selection_key(entity.id(), entity.generation(), camera) else {
            continue;
        };
        let replace = fallback
            .as_ref()
            .is_none_or(|(best_key, _)| key < *best_key);
        if replace {
            fallback = Some((key, candidate));
        }
    }
    fallback.map(|(_, candidate)| candidate)
}

/// Selects the enabled Game View camera with the highest priority.
///
/// The payload is generic so rendering, movement, LOD, and lock-on can attach
/// the transform or controller data they need without reimplementing camera
/// ordering. If no camera is enabled, this returns `None`.
#[doc(hidden)]
pub fn select_active_game_camera<'camera, T>(
    cameras: impl Iterator<Item = (engine_ecs::Entity, (&'camera Camera3D, T))>,
) -> Option<(engine_ecs::Entity, (&'camera Camera3D, T))> {
    select_active_game_camera_with_override(cameras, None)
}

/// Stores the current viewport dimensions.
#[derive(Debug, Clone)]
pub struct ViewportSize {
    /// The viewport width in physical pixels.
    pub width: u32,
    /// The viewport height in physical pixels.
    pub height: u32,
}

impl ViewportSize {
    /// Returns a valid aspect ratio for the current dimensions.
    pub fn aspect(&self) -> f32 {
        if self.height == 0 {
            1.0
        } else {
            self.width as f32 / self.height as f32
        }
    }
}

impl Default for ViewportSize {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
        }
    }
}

/// Updates camera aspect ratios from the viewport size resource.
pub fn camera_aspect_system(
    viewport: engine_ecs::Res<ViewportSize>,
    mut query: engine_ecs::Query<&mut Camera3D>,
) {
    let aspect = viewport.aspect();
    for (_, camera) in &mut query {
        camera.aspect = aspect;
    }
}

/// Returns a useful default transform for a camera looking at the origin.
pub fn default_camera_transform() -> Transform {
    Transform::looking_at(
        glam::Vec3::new(0.0, 2.0, 5.0),
        glam::Vec3::ZERO,
        glam::Vec3::Y,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_override_can_select_a_disabled_standby_camera() {
        let normal_entity = engine_ecs::Entity::from_raw(1, 0);
        let standby_entity = engine_ecs::Entity::from_raw(2, 0);
        let normal = Camera3D {
            priority: 50,
            ..Camera3D::default()
        };
        let standby = Camera3D {
            enabled: false,
            ..Camera3D::default()
        };

        let selected = select_active_game_camera_with_override(
            [
                (normal_entity, (&normal, ())),
                (standby_entity, (&standby, ())),
            ]
            .into_iter(),
            Some(standby_entity),
        );

        assert_eq!(selected.map(|(entity, _)| entity), Some(standby_entity));
        assert!(!standby.enabled);
        assert_eq!(standby.priority, 0);
    }

    #[test]
    fn missing_override_target_falls_back_to_normal_priority_order() {
        let low_entity = engine_ecs::Entity::from_raw(1, 0);
        let high_entity = engine_ecs::Entity::from_raw(2, 0);
        let missing = engine_ecs::Entity::from_raw(99, 0);
        let low = Camera3D::default();
        let high = Camera3D {
            priority: 10,
            ..Camera3D::default()
        };

        let selected = select_active_game_camera_with_override(
            [(low_entity, (&low, ())), (high_entity, (&high, ()))].into_iter(),
            Some(missing),
        );

        assert_eq!(selected.map(|(entity, _)| entity), Some(high_entity));
    }
}
