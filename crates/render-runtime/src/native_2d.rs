//! Deterministic native 2D camera, sprite ordering, batching, and tile culling (ADR 0127).

use engine_authoring::{SortingLayerId, SpriteRef, TileChunkCoord};
use glam::{Mat4, Vec2, Vec3};

/// Orthographic viewport-fit policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportFit2d {
    /// Preserve the complete reference frame and allow unused viewport space.
    Fit,
    /// Fill the viewport and crop reference-frame overflow.
    Fill,
    /// Stretch reference dimensions to the viewport shape.
    Stretch,
}

/// Runtime Camera2D contract. It uses the normal Transform for pose.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera2d {
    /// Whether this camera participates in Game View arbitration.
    pub enabled: bool,
    /// Selection priority shared with other game-camera kinds.
    pub priority: i32,
    /// Vertical orthographic world span before zoom.
    pub orthographic_height: f32,
    /// Positive projection zoom multiplier.
    pub zoom: f32,
    /// Near clipping plane in camera space.
    pub near: f32,
    /// Far clipping plane in camera space.
    pub far: f32,
    /// Whether projection follows deterministic reference-pixel scaling.
    pub pixel_perfect: bool,
    /// Reference texture pixels represented by one world unit.
    pub reference_pixels_per_unit: f32,
    /// Reference pixel-perfect resolution `[width, height]`.
    pub reference_resolution: [u32; 2],
    /// Viewport fitting policy.
    pub fit: ViewportFit2d,
}

impl Default for Camera2d {
    fn default() -> Self { Self { enabled: true, priority: 0, orthographic_height: 10.0, zoom: 1.0, near: -1000.0, far: 1000.0, pixel_perfect: false, reference_pixels_per_unit: 100.0, reference_resolution: [320, 180], fit: ViewportFit2d::Fit } }
}

/// Why a Camera2D pose cannot be represented by the XY convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Camera2dDiagnostic {
    /// Camera transform contains non-finite values.
    NonFinitePose,
    /// Camera rotation tilts away from the XY gameplay plane.
    NonPlanarTilt,
    /// Projection settings cannot form a finite orthographic matrix.
    InvalidProjection,
}

impl Camera2d {
    /// Builds a deterministic orthographic projection without changing authored transforms.
    pub fn projection(self, viewport: [u32; 2]) -> Result<Mat4, Camera2dDiagnostic> {
        if !self.orthographic_height.is_finite() || self.orthographic_height <= 0.0 || !self.zoom.is_finite() || self.zoom <= 0.0 || self.far <= self.near { return Err(Camera2dDiagnostic::InvalidProjection); }
        let width = viewport[0].max(1) as f32; let height = viewport[1].max(1) as f32;
        let mut world_h = self.orthographic_height / self.zoom;
        if self.pixel_perfect {
            let ppu = self.reference_pixels_per_unit.max(1.0);
            let ref_h = self.reference_resolution[1].max(1) as f32 / ppu;
            let integer_scale = (height / self.reference_resolution[1].max(1) as f32).floor().max(1.0);
            world_h = ref_h * (height / (self.reference_resolution[1].max(1) as f32 * integer_scale));
        }
        let aspect = width / height;
        Ok(Mat4::orthographic_rh(-world_h * aspect * 0.5, world_h * aspect * 0.5, -world_h * 0.5, world_h * 0.5, self.near, self.far))
    }
}

/// Validates that an ordinary 3D transform is safe for a Camera2D.
pub fn validate_camera_pose(forward: Vec3, up: Vec3) -> Result<(), Camera2dDiagnostic> {
    if !forward.is_finite() || !up.is_finite() { return Err(Camera2dDiagnostic::NonFinitePose); }
    if forward.normalize_or_zero().dot(Vec3::NEG_Z).abs() < 0.999 || up.normalize_or_zero().dot(Vec3::Y).abs() < 0.999 { return Err(Camera2dDiagnostic::NonPlanarTilt); }
    Ok(())
}

/// Common camera-arbitration input shared by Camera2D and Camera3D adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveCameraCandidate {
    /// Stable deterministic runtime entity key.
    pub entity_key: u64,
    /// Whether the camera may drive the Game View.
    pub enabled: bool,
    /// Shared camera-selection priority; higher values win.
    pub priority: i32,
    /// Projection family owned by the candidate.
    pub kind: CameraKind,
}

/// Projection family participating in shared Game View arbitration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CameraKind {
    /// Native orthographic Camera2D.
    TwoD,
    /// Existing perspective Camera3D.
    ThreeD,
}

/// Deterministic result of shared Game View camera arbitration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveCameraSelection {
    /// No enabled camera exists.
    None,
    /// Exactly one highest-priority camera was selected.
    Selected(ActiveCameraCandidate),
    /// More than one enabled camera shares the highest priority.
    Ambiguous {
        /// Highest priority shared by the ambiguous candidates.
        priority: i32,
    },
}

/// Selects one camera by shared enabled/priority intent; highest-priority ties are diagnosed.
pub fn select_active_camera(candidates: impl IntoIterator<Item = ActiveCameraCandidate>) -> ActiveCameraSelection {
    let mut enabled: Vec<_> = candidates.into_iter().filter(|c| c.enabled).collect();
    enabled.sort_by_key(|c| (std::cmp::Reverse(c.priority), c.entity_key));
    let Some(first) = enabled.first().copied() else { return ActiveCameraSelection::None; };
    if enabled.get(1).is_some_and(|next| next.priority == first.priority) { ActiveCameraSelection::Ambiguous { priority: first.priority } } else { ActiveCameraSelection::Selected(first) }
}

/// Extracted SpriteRenderer2D instance. `entity_key` is the deterministic equal-order tie-break.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteInstance2d {
    /// Stable deterministic runtime entity key.
    pub entity_key: u64,
    /// Stable logical sprite reference.
    pub sprite: SpriteRef,
    /// Stable logical sorting layer.
    pub sorting_layer: SortingLayerId,
    /// Resolved project ordering rank for the sorting layer.
    pub layer_rank: u32,
    /// Signed authored order within the logical layer.
    pub order_in_layer: i32,
    /// Runtime texture page selected by asset compilation.
    pub texture_page: u32,
    /// Runtime material batching key.
    pub material_key: u64,
    /// Runtime sampler batching key.
    pub sampler_key: u64,
    /// World XY sprite origin.
    pub position: Vec2,
    /// World XY sprite size.
    pub size: Vec2,
    /// Normalized pivot used to place the quad around the origin.
    pub pivot: Vec2,
    /// Linear RGBA tint multiplier.
    pub tint: [f32; 4],
    /// Whether texture coordinates are mirrored horizontally.
    pub flip_x: bool,
    /// Whether texture coordinates are mirrored vertically.
    pub flip_y: bool,
    /// Whether this instance contributes a draw.
    pub visible: bool,
}

/// One deterministic contiguous sprite batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteBatch2d {
    /// Runtime texture page shared by every instance in the batch.
    pub texture_page: u32,
    /// Runtime material key shared by every instance in the batch.
    pub material_key: u64,
    /// Runtime sampler key shared by every instance in the batch.
    pub sampler_key: u64,
    /// First sorted instance index included in the batch.
    pub first: usize,
    /// Number of contiguous sorted instances included in the batch.
    pub count: usize,
}

/// Sorts instances by authored logical order and derives compatible contiguous batches.
pub fn sort_and_batch_sprites(instances: &mut [SpriteInstance2d]) -> Vec<SpriteBatch2d> {
    instances.sort_by_key(|s| (s.layer_rank, s.order_in_layer, s.entity_key));
    let mut batches = Vec::new();
    for (index, sprite) in instances.iter().enumerate() {
        if !sprite.visible { continue; }
        let same = batches.last().is_some_and(|b: &SpriteBatch2d| b.first + b.count == index && b.texture_page == sprite.texture_page && b.material_key == sprite.material_key && b.sampler_key == sprite.sampler_key);
        if same { batches.last_mut().expect("checked").count += 1; } else { batches.push(SpriteBatch2d { texture_page: sprite.texture_page, material_key: sprite.material_key, sampler_key: sprite.sampler_key, first: index, count: 1 }); }
    }
    batches
}

/// Camera-visible XY rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRect2d {
    /// Inclusive minimum world XY corner.
    pub min: Vec2,
    /// Inclusive maximum world XY corner.
    pub max: Vec2,
}

impl ViewRect2d {
    /// Returns whether an XY bounds pair intersects this visible rectangle.
    pub fn intersects(self, min: Vec2, max: Vec2) -> bool {
        self.min.x <= max.x
            && self.max.x >= min.x
            && self.min.y <= max.y
            && self.max.y >= min.y
    }
}

/// Returns visible Tile Map chunks only; unrelated chunks remain untouched.
pub fn cull_tile_chunks(chunks: impl IntoIterator<Item = (TileChunkCoord, Vec2, Vec2)>, view: ViewRect2d) -> Vec<TileChunkCoord> {
    chunks.into_iter().filter_map(|(id, min, max)| view.intersects(min, max).then_some(id)).collect()
}

#[cfg(test)] mod tests { use super::*;
    #[test] fn equal_order_has_stable_entity_tie_break() {
        let layer = SortingLayerId::parse("sorting_layer_00000000000000000000000000").unwrap();
        let atlas = engine_authoring::AssetId::generate(); let sprite = engine_authoring::SpriteId::generate();
        let make = |entity_key| SpriteInstance2d { entity_key, sprite: SpriteRef { atlas: atlas.clone(), sprite: sprite.clone() }, sorting_layer: layer.clone(), layer_rank: 0, order_in_layer: 0, texture_page: 0, material_key: 1, sampler_key: 1, position: Vec2::ZERO, size: Vec2::ONE, pivot: Vec2::splat(0.5), tint: [1.0;4], flip_x:false, flip_y:false, visible:true };
        let mut sprites = vec![make(9), make(2)]; let batches = sort_and_batch_sprites(&mut sprites);
        assert_eq!(sprites[0].entity_key, 2); assert_eq!(batches[0].count, 2);
    }
}
