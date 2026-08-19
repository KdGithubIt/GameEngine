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

/// Projection family participating in the shared Game View arbitration contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameCameraKind {
    /// Existing perspective Camera3D.
    ThreeD,
    /// Native orthographic Camera2D.
    TwoD,
}

/// Backend-neutral enabled/priority intent contributed by any Game View camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameCameraIntent {
    /// Runtime entity identity used only as a deterministic payload key.
    pub entity: engine_ecs::Entity,
    /// Projection family of this candidate.
    pub kind: GameCameraKind,
    /// Whether the candidate may drive the Game View.
    pub enabled: bool,
    /// Shared authored priority; higher values win.
    pub priority: i32,
}

/// Shared Camera2D/Camera3D arbitration result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameCameraSelection {
    /// No enabled game camera exists.
    None,
    /// Exactly one enabled camera owns the highest authored priority.
    Selected(GameCameraIntent),
    /// Multiple enabled cameras share the highest authored priority.
    Ambiguous {
        /// Highest tied priority.
        priority: i32,
        /// Number of candidates tied at that priority.
        count: usize,
    },
}

/// Generalizes ADR 0107 camera intent across Camera3D and Camera2D.
///
/// Runtime entity order is never used to hide an authored highest-priority tie;
/// authoring surfaces can therefore report the same ambiguity the runtime sees.
pub fn select_game_camera_intent(
    candidates: impl IntoIterator<Item = GameCameraIntent>,
) -> GameCameraSelection {
    let mut enabled = candidates.into_iter().filter(|candidate| candidate.enabled);
    let Some(mut best) = enabled.next() else {
        return GameCameraSelection::None;
    };
    let mut tied = 1_usize;
    for candidate in enabled {
        if candidate.priority > best.priority {
            best = candidate;
            tied = 1;
        } else if candidate.priority == best.priority {
            tied += 1;
        }
    }
    if tied > 1 {
        GameCameraSelection::Ambiguous {
            priority: best.priority,
            count: tied,
        }
    } else {
        GameCameraSelection::Selected(best)
    }
}

/// Builds the total ordering used by every Camera3D-only compatibility consumer.
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

/// Selects the enabled Game View camera with the highest priority.
///
/// The payload is generic so rendering, movement, LOD, and lock-on can attach
/// the transform or controller data they need without reimplementing camera
/// ordering. If no camera is enabled, this returns `None`.
#[doc(hidden)]
pub fn select_active_game_camera<'camera, T>(
    cameras: impl Iterator<Item = (engine_ecs::Entity, (&'camera Camera3D, T))>,
) -> Option<(engine_ecs::Entity, (&'camera Camera3D, T))> {
    cameras
        .filter_map(|candidate| {
            let entity = candidate.0;
            let camera = candidate.1.0;
            camera_selection_key(entity.id(), entity.generation(), camera)
                .map(|key| (key, candidate))
        })
        .min_by_key(|(key, _)| *key)
        .map(|(_, candidate)| candidate)
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
