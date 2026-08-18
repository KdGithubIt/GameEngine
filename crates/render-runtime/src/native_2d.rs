//! Native 2D camera, sprite ordering/batching, and tile visibility contracts (ADR 0127).

use crate::material::DecodedTexture;
use crate::transform::Transform;
use engine_authoring::{
    AssetId, PixelsPerUnit, SortingLayerId, SpriteFiltering, SpriteRef, TileCell, TileChunkCoord,
    TileId, TileLayerId, TileMapDocument, TileSetDocument,
};
use glam::{Mat4, Vec2, Vec3};
use std::collections::BTreeMap;
use std::sync::Arc;

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

/// Runtime Camera2D contract. Pose remains owned by the normal Transform hierarchy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera2d {
    /// Whether this camera participates in Game View arbitration.
    pub enabled: bool,
    /// Selection priority shared with Camera3D. Higher values win.
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
    fn default() -> Self {
        Self {
            enabled: true,
            priority: 0,
            orthographic_height: 10.0,
            zoom: 1.0,
            near: -1000.0,
            far: 1000.0,
            pixel_perfect: false,
            reference_pixels_per_unit: 100.0,
            reference_resolution: [320, 180],
            fit: ViewportFit2d::Fit,
        }
    }
}

/// Why Camera2D cannot safely produce the required XY orthographic view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Camera2dDiagnostic {
    /// Transform or projection contains non-finite values.
    NonFinitePose,
    /// Camera rotation tilts away from the world XY plane.
    NonPlanarTilt,
    /// Camera scale would make projection semantics ambiguous.
    IncompatibleScale,
    /// Projection settings cannot form a finite orthographic matrix.
    InvalidProjection,
}

impl Camera2d {
    /// Builds the orthographic projection without rewriting authored transforms.
    pub fn projection(self, viewport: [u32; 2]) -> Result<Mat4, Camera2dDiagnostic> {
        if !self.orthographic_height.is_finite()
            || self.orthographic_height <= 0.0
            || !self.zoom.is_finite()
            || self.zoom <= 0.0
            || !self.near.is_finite()
            || !self.far.is_finite()
            || self.far <= self.near
            || !self.reference_pixels_per_unit.is_finite()
            || self.reference_pixels_per_unit <= 0.0
        {
            return Err(Camera2dDiagnostic::InvalidProjection);
        }
        let width = viewport[0].max(1) as f32;
        let height = viewport[1].max(1) as f32;
        let reference_width = self.reference_resolution[0].max(1) as f32;
        let reference_height = self.reference_resolution[1].max(1) as f32;
        let viewport_aspect = width / height;
        let reference_aspect = reference_width / reference_height;

        let mut world_height = self.orthographic_height / self.zoom;
        let mut world_width = world_height * viewport_aspect;
        if self.pixel_perfect {
            let integer_scale = (width / reference_width)
                .min(height / reference_height)
                .floor()
                .max(1.0);
            let pixel_world = 1.0 / (self.reference_pixels_per_unit * integer_scale);
            let reference_world_width = reference_width * pixel_world;
            let reference_world_height = reference_height * pixel_world;
            match self.fit {
                ViewportFit2d::Fit => {
                    if viewport_aspect >= reference_aspect {
                        world_height = reference_world_height;
                        world_width = world_height * viewport_aspect;
                    } else {
                        world_width = reference_world_width;
                        world_height = world_width / viewport_aspect;
                    }
                }
                ViewportFit2d::Fill => {
                    if viewport_aspect >= reference_aspect {
                        world_width = reference_world_width;
                        world_height = world_width / viewport_aspect;
                    } else {
                        world_height = reference_world_height;
                        world_width = world_height * viewport_aspect;
                    }
                }
                ViewportFit2d::Stretch => {
                    world_width = reference_world_width;
                    world_height = reference_world_height;
                }
            }
        }
        Ok(Mat4::orthographic_rh(
            -world_width * 0.5,
            world_width * 0.5,
            -world_height * 0.5,
            world_height * 0.5,
            self.near,
            self.far,
        ))
    }

    /// Builds a view-projection matrix from the normal Transform authority.
    pub fn view_projection_matrix(
        self,
        transform: &Transform,
        viewport: [u32; 2],
    ) -> Result<Mat4, Camera2dDiagnostic> {
        validate_camera_transform(transform)?;
        Ok(self.projection(viewport)? * transform.to_matrix().inverse())
    }
}

/// Validates planar Camera2D Transform usage without mutating authored values.
pub fn validate_camera_transform(transform: &Transform) -> Result<(), Camera2dDiagnostic> {
    if !transform.translation.is_finite()
        || !transform.rotation.is_finite()
        || !transform.scale.is_finite()
    {
        return Err(Camera2dDiagnostic::NonFinitePose);
    }
    if (transform.scale.x - 1.0).abs() > 1.0e-4
        || (transform.scale.y - 1.0).abs() > 1.0e-4
        || (transform.scale.z - 1.0).abs() > 1.0e-4
    {
        return Err(Camera2dDiagnostic::IncompatibleScale);
    }
    let forward = transform.rotation * Vec3::NEG_Z;
    let up = transform.rotation * Vec3::Y;
    if forward.normalize_or_zero().dot(Vec3::NEG_Z).abs() < 0.999
        || up.normalize_or_zero().dot(Vec3::Y).abs() < 0.999
    {
        return Err(Camera2dDiagnostic::NonPlanarTilt);
    }
    Ok(())
}

/// Runtime-only resolved Sprite Atlas region consumed by the GPU renderer.
///
/// Stable SpriteRef remains the logical identity on SpriteRenderer2D. This component
/// carries only immutable derived source data; GPU handles and packed UV identities
/// are deliberately excluded.
#[derive(Debug, Clone)]
pub struct ResolvedSpriteRegion2d {
    /// Stable source texture asset referenced by the Sprite Atlas region.
    pub source_texture: AssetId,
    /// CPU-decoded immutable source texture shared until GPU upload.
    pub texture: Arc<DecodedTexture>,
    /// Source pixel rectangle `[x, y, width, height]`.
    pub rect: [u32; 4],
    /// Normalized region pivot.
    pub pivot: [f32; 2],
    /// Authored region PPU policy; project defaults are resolved at render time.
    pub pixels_per_unit: PixelsPerUnit,
    /// Optional region sampler override; `None` uses project defaults.
    pub filtering: Option<SpriteFiltering>,
    /// Source edge extrusion retained for derived atlas packing/bleed handling.
    pub extrusion_pixels: u8,
}

/// Runtime Tile Map scene component. The referenced asset owns stable layers/chunks/cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileMap2d {
    /// Registered `*.tilemap.json` asset.
    pub tile_map: AssetId,
    /// Whether this Tile Map contributes runtime render output.
    pub visible: bool,
}

/// One resolved sparse Tile Map cell retained inside its owning runtime chunk.
#[derive(Debug, Clone)]
pub struct ResolvedTileCell2d {
    /// Chunk-local logical cell coordinate.
    pub cell: TileCell,
    /// Stable SpriteRef selected by the TileId.
    pub sprite: SpriteRef,
    /// Immutable resolved sprite source data.
    pub region: ResolvedSpriteRegion2d,
}

/// One stable resolved Tile Map render chunk.
#[derive(Debug, Clone)]
pub struct ResolvedTileChunkRender2d {
    /// Stable layer identity.
    pub layer: TileLayerId,
    /// Project sorting layer copied from the authored Tile Map layer.
    pub sorting_layer: SortingLayerId,
    /// Signed logical order copied from the authored Tile Map layer.
    pub order_in_layer: i32,
    /// Sparse chunk coordinate.
    pub coord: TileChunkCoord,
    /// Deterministically ordered non-empty cells in this chunk.
    pub cells: Vec<ResolvedTileCell2d>,
}

/// Runtime-only immutable Tile Map data resolved during scene conversion.
///
/// One ECS entity owns the whole map. Cells remain sparse/chunked; resolved sprite
/// source data is shared by TileId and contains no device-specific GPU handles.
#[derive(Debug, Clone)]
pub struct ResolvedTileMap2d {
    /// Validated Tile Map document.
    pub document: Arc<TileMapDocument>,
    /// Validated Tile Set referenced by [`Self::document`].
    pub tile_set: Arc<TileSetDocument>,
    /// Stable TileId to resolved Sprite Atlas region.
    pub sprites: BTreeMap<TileId, ResolvedSpriteRegion2d>,
    /// Stable sparse render chunks prepared once during scene conversion.
    pub chunks: Vec<ResolvedTileChunkRender2d>,
}

/// Extracted SpriteRenderer2D draw instance.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteInstance2d {
    /// Stable deterministic runtime entity tie-break key.
    pub entity_key: u64,
    /// Stable logical SpriteRef consumed from runtime component state.
    pub sprite: SpriteRef,
    /// Stable logical sorting layer.
    pub sorting_layer: SortingLayerId,
    /// Resolved project rank for the sorting layer.
    pub layer_rank: u32,
    /// Signed authored order within the layer.
    pub order_in_layer: i32,
    /// Runtime texture page or source texture key used for batching only.
    pub texture_key: u64,
    /// Runtime material key used for batching only.
    pub material_key: u64,
    /// Runtime sampler key used for batching only.
    pub sampler_key: u64,
    /// World XY sprite origin.
    pub position: Vec2,
    /// World XY dimensions.
    pub size: Vec2,
    /// Normalized pivot.
    pub pivot: Vec2,
    /// World Z-axis rotation in radians.
    pub rotation_radians: f32,
    /// Runtime normalized UV rectangle `[u0, v0, u1, v1]`.
    pub uv_rect: [f32; 4],
    /// Linear RGBA tint multiplier.
    pub tint: [f32; 4],
    /// Mirror texture coordinates horizontally.
    pub flip_x: bool,
    /// Mirror texture coordinates vertically.
    pub flip_y: bool,
    /// Whether this instance contributes a draw.
    pub visible: bool,
}

/// One deterministic contiguous compatible sprite batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteBatch2d {
    /// Runtime texture compatibility key.
    pub texture_key: u64,
    /// Runtime material compatibility key.
    pub material_key: u64,
    /// Runtime sampler compatibility key.
    pub sampler_key: u64,
    /// First sorted instance index in the batch.
    pub first: usize,
    /// Number of contiguous sorted instances.
    pub count: usize,
}

/// Sorts by logical layer/order/entity and derives compatible contiguous batches.
pub fn sort_and_batch_sprites(instances: &mut [SpriteInstance2d]) -> Vec<SpriteBatch2d> {
    instances.sort_by_key(|sprite| (sprite.layer_rank, sprite.order_in_layer, sprite.entity_key));
    let mut batches = Vec::new();
    for (index, sprite) in instances.iter().enumerate() {
        if !sprite.visible {
            continue;
        }
        let compatible = batches.last().is_some_and(|batch: &SpriteBatch2d| {
            batch.first + batch.count == index
                && batch.texture_key == sprite.texture_key
                && batch.material_key == sprite.material_key
                && batch.sampler_key == sprite.sampler_key
        });
        if compatible {
            batches.last_mut().expect("compatible batch exists").count += 1;
        } else {
            batches.push(SpriteBatch2d {
                texture_key: sprite.texture_key,
                material_key: sprite.material_key,
                sampler_key: sprite.sampler_key,
                first: index,
                count: 1,
            });
        }
    }
    batches
}

/// Camera-visible world XY rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewRect2d {
    /// Inclusive minimum corner.
    pub min: Vec2,
    /// Inclusive maximum corner.
    pub max: Vec2,
}

impl ViewRect2d {
    /// Returns whether one XY bounds pair intersects the view.
    pub fn intersects(self, min: Vec2, max: Vec2) -> bool {
        self.min.x <= max.x
            && self.max.x >= min.x
            && self.min.y <= max.y
            && self.max.y >= min.y
    }
}

/// World bounds for one stable Tile Map layer/chunk.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileChunkBounds2d<'a> {
    /// Stable Tile Map layer identity.
    pub layer: &'a TileLayerId,
    /// Spatial chunk coordinate.
    pub coord: TileChunkCoord,
    /// Inclusive minimum world XY bounds.
    pub min: Vec2,
    /// Inclusive maximum world XY bounds.
    pub max: Vec2,
}

/// Stable identity of one camera-visible Tile Map chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleTileChunk2d {
    /// Stable Tile Map layer identity.
    pub layer: TileLayerId,
    /// Spatial chunk coordinate.
    pub coord: TileChunkCoord,
}

/// Culls sparse Tile Map chunks without rebuilding unrelated chunks.
pub fn cull_tile_chunks<'a>(
    chunks: impl IntoIterator<Item = TileChunkBounds2d<'a>>,
    view: ViewRect2d,
) -> Vec<VisibleTileChunk2d> {
    chunks
        .into_iter()
        .filter(|chunk| view.intersects(chunk.min, chunk.max))
        .map(|chunk| VisibleTileChunk2d { layer: chunk.layer.clone(), coord: chunk.coord })
        .collect()
}

/// Per-frame Native 2D render counters used by proving-project budgets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Native2dRenderMetrics {
    /// Visible SpriteRenderer2D instance count.
    pub sprite_count: usize,
    /// Compatible sprite batch count after deterministic sorting.
    pub sprite_batches: usize,
    /// Camera-visible Tile Map chunk count.
    pub visible_tile_chunks: usize,
    /// Tile chunks rebuilt because source data changed this frame.
    pub rebuilt_tile_chunks: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{AssetId, SpriteId};

    #[test]
    fn equal_sprite_order_has_stable_entity_tie_break() {
        let layer = SortingLayerId::parse("sorting_layer_00000000000000000000000000").unwrap();
        let atlas = AssetId::generate();
        let sprite = SpriteId::generate();
        let make = |entity_key| SpriteInstance2d {
            entity_key,
            sprite: SpriteRef { atlas: atlas.clone(), sprite: sprite.clone() },
            sorting_layer: layer.clone(),
            layer_rank: 0,
            order_in_layer: 0,
            texture_key: 1,
            material_key: 2,
            sampler_key: 3,
            position: Vec2::ZERO,
            size: Vec2::ONE,
            pivot: Vec2::splat(0.5),
            rotation_radians: 0.0,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            tint: [1.0; 4],
            flip_x: false,
            flip_y: false,
            visible: true,
        };
        let mut sprites = vec![make(9), make(2)];
        let batches = sort_and_batch_sprites(&mut sprites);
        assert_eq!(sprites[0].entity_key, 2);
        assert_eq!(batches[0].count, 2);
    }

    #[test]
    fn camera_validation_rejects_scale_without_rewriting_transform() {
        let mut transform = Transform::default();
        transform.scale = Vec3::splat(2.0);
        assert_eq!(validate_camera_transform(&transform), Err(Camera2dDiagnostic::IncompatibleScale));
        assert_eq!(transform.scale, Vec3::splat(2.0));
    }
}
