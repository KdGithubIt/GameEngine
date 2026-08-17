//! Native 2D authoring contracts (ADR 0127).
//!
//! Persisted 2D identity lives here; runtime/GPU handles are deliberately absent.

use crate::id::AssetId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Sprite atlas document schema version.
pub const SPRITE_ATLAS_SCHEMA_VERSION: u32 = 1;
/// Sprite animation document schema version.
pub const SPRITE_ANIMATION_SCHEMA_VERSION: u32 = 1;
/// Tile-set document schema version.
pub const TILE_SET_SCHEMA_VERSION: u32 = 1;
/// Tile-map document schema version.
pub const TILE_MAP_SCHEMA_VERSION: u32 = 1;

macro_rules! stable_2d_id {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("Stable persisted identifier with prefix `", $prefix, "_`.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generates a stable opaque identifier.
            pub fn generate() -> Self {
                Self(format!(concat!($prefix, "_{}"), ulid::Ulid::new()))
            }

            /// Creates an identifier after validating its persisted prefix and ULID suffix.
            pub fn parse(value: impl Into<String>) -> Result<Self, Native2dIdError> {
                let value = value.into();
                let Some(suffix) = value.strip_prefix(concat!($prefix, "_")) else {
                    return Err(Native2dIdError::WrongPrefix(value));
                };
                if ulid::Ulid::from_string(suffix).is_err() {
                    return Err(Native2dIdError::InvalidUlid(value));
                }
                Ok(Self(value))
            }

            /// Returns the opaque persisted string.
            pub fn as_str(&self) -> &str { &self.0 }
        }
    };
}

/// Stable 2D identifier validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Native2dIdError {
    /// Prefix did not match the typed domain.
    WrongPrefix(String),
    /// Suffix was not a valid ULID.
    InvalidUlid(String),
}

stable_2d_id!(SortingLayerId, "sorting_layer");
stable_2d_id!(SpriteId, "sprite");
stable_2d_id!(TileId, "tile");
stable_2d_id!(TileLayerId, "tile_layer");

/// Project-wide default texture filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpriteFiltering {
    /// Preserve hard texel boundaries for pixel-art assets.
    Nearest,
    /// Interpolate neighboring texels for smoothly filtered sprites.
    Linear,
}

/// Pixel-preview policy for 2D authoring and Camera2D.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelPreviewPolicy {
    /// Do not request pixel-grid alignment behavior.
    Off,
    /// Show pixel-alignment guidance without enforcing a pixel-perfect camera.
    Advisory,
    /// Request the deterministic pixel-perfect camera projection policy.
    PixelPerfect,
}

/// Stable named logical draw layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortingLayer {
    /// Stable identity persisted by sprites and Tile Map layers.
    pub id: SortingLayerId,
    /// Human-readable layer name; renaming does not change [`Self::id`].
    pub name: String,
}

/// Typed project 2D defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project2dSettings {
    /// Default number of source texture pixels represented by one world unit.
    pub default_pixels_per_unit: f32,
    /// Default texture sampling policy for sprite regions without an override.
    pub default_filtering: SpriteFiltering,
    /// Fixed-step gravity vector in the world XY gameplay plane.
    pub gravity: [f32; 2],
    /// Project-wide default policy for pixel-aware previews and cameras.
    pub pixel_preview: PixelPreviewPolicy,
    /// Ordered logical draw layers addressed by stable identity.
    pub sorting_layers: Vec<SortingLayer>,
}

impl Default for Project2dSettings {
    fn default() -> Self {
        Self {
            default_pixels_per_unit: 100.0,
            default_filtering: SpriteFiltering::Nearest,
            gravity: [0.0, -9.81],
            pixel_preview: PixelPreviewPolicy::Advisory,
            sorting_layers: vec![SortingLayer {
                id: SortingLayerId::parse("sorting_layer_00000000000000000000000000").expect("constant id"),
                name: "Default".into(),
            }],
        }
    }
}

/// Integer source-texture pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect {
    /// Left edge in source-texture pixels.
    pub x: u32,
    /// Top edge in source-texture pixels.
    pub y: u32,
    /// Region width in source-texture pixels.
    pub width: u32,
    /// Region height in source-texture pixels.
    pub height: u32,
}

/// Stable logical sprite reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteRef {
    /// Sprite Atlas asset containing the referenced region.
    pub atlas: AssetId,
    /// Stable region identity within [`Self::atlas`].
    pub sprite: SpriteId,
}

/// Pixels-per-unit selection for one sprite region.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelsPerUnit {
    /// Use [`Project2dSettings::default_pixels_per_unit`].
    ProjectDefault,
    /// Use this region-specific pixels-per-unit value.
    Override(f32),
}

/// One stable sprite region in an atlas source document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteRegion {
    /// Stable logical identity of this region.
    pub id: SpriteId,
    /// Human-readable region name.
    pub name: String,
    /// Source texture asset from which pixels are sampled.
    pub source_texture: AssetId,
    /// Integer pixel bounds within the source texture.
    pub rect: PixelRect,
    /// Normalized pivot measured from the region's top-left bounds.
    pub pivot: [f32; 2],
    /// World-unit scale policy for this region.
    pub pixels_per_unit: PixelsPerUnit,
    /// Optional sampling override; `None` uses the project default.
    pub filtering: Option<SpriteFiltering>,
    /// Number of edge pixels reserved to prevent packed-atlas bleeding.
    pub extrusion_pixels: u8,
}

/// Versioned Sprite Atlas authoring asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteAtlasDocument {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Stable sprite regions contained in the atlas.
    pub regions: Vec<SpriteRegion>,
}

impl SpriteAtlasDocument {
    /// Finds a region by stable identity; rename/reorder cannot affect resolution.
    pub fn region(&self, id: &SpriteId) -> Option<&SpriteRegion> { self.regions.iter().find(|r| &r.id == id) }
    /// Validates first-release region invariants.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != SPRITE_ATLAS_SCHEMA_VERSION { errors.push("unsupported sprite atlas schema".into()); }
        let mut ids = BTreeMap::new();
        for region in &self.regions {
            if region.rect.width == 0 || region.rect.height == 0 { errors.push(format!("sprite {} has an empty rect", region.id.as_str())); }
            if !(0.0..=1.0).contains(&region.pivot[0]) || !(0.0..=1.0).contains(&region.pivot[1]) { errors.push(format!("sprite {} pivot is outside [0,1]", region.id.as_str())); }
            if ids.insert(region.id.as_str(), ()).is_some() { errors.push(format!("duplicate SpriteId {}", region.id.as_str())); }
        }
        errors
    }
}

/// Stable SpriteRenderer2D authoring data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteRenderer2d {
    /// Logical sprite region rendered by this component.
    pub sprite: SpriteRef,
    /// Linear RGBA tint multiplier.
    pub tint: [f32; 4],
    /// Whether sampling is mirrored horizontally.
    pub flip_x: bool,
    /// Whether sampling is mirrored vertically.
    pub flip_y: bool,
    /// Stable logical sorting layer.
    pub sorting_layer: SortingLayerId,
    /// Signed authored order within the sorting layer.
    pub order_in_layer: i32,
    /// Whether the sprite contributes a render instance.
    pub visible: bool,
}

/// One exact-duration sprite-animation frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteAnimationFrame {
    /// Sprite displayed for this frame.
    pub sprite: SpriteRef,
    /// Positive frame duration in the clip's integer tick domain.
    pub duration_ticks: u32,
    /// Optional event emitted when playback enters this frame.
    pub event: Option<String>,
}

/// Versioned immutable sprite-animation clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteAnimationDocument {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Integer timebase used by every frame duration.
    pub ticks_per_second: u32,
    /// Default looping policy for playback instances.
    pub looping: bool,
    /// Ordered immutable frame sequence.
    pub frames: Vec<SpriteAnimationFrame>,
}

/// Persisted SpriteAnimator2D component settings; live frame/time are runtime state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteAnimator2d {
    /// Sprite Animation asset selected for playback.
    pub clip: AssetId,
    /// Whether playback starts when the runtime component becomes active.
    pub autoplay: bool,
    /// Non-negative playback speed multiplier.
    pub speed: f32,
    /// Optional per-instance looping override.
    pub looping_override: Option<bool>,
}

/// Collision material stored without backend-specific types.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PhysicsMaterial2d {
    /// Tangential friction coefficient.
    pub friction: f32,
    /// Normal restitution coefficient.
    pub restitution: f32,
}

/// Backend-neutral tile collision shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TileCollisionShape {
    /// Axis-aligned box centered on the tile origin.
    Box {
        /// Positive X/Y half extents in tile-local units.
        half_extents: [f32; 2],
    },
    /// Circle centered on the tile origin.
    Circle {
        /// Positive radius in tile-local units.
        radius: f32,
    },
    /// Simple polygon described by tile-local points.
    Polygon {
        /// Ordered finite polygon vertices.
        points: Vec<[f32; 2]>,
    },
}

/// One stable tile definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileDefinition {
    /// Stable logical identity of this tile.
    pub id: TileId,
    /// Human-readable tile name.
    pub name: String,
    /// Sprite used to render this tile.
    pub sprite: SpriteRef,
    /// Backend-neutral collision geometry contributed by the tile.
    pub collision: Vec<TileCollisionShape>,
    /// Whether collision uses the shared one-way platform policy.
    pub one_way: bool,
    /// Author-defined classification tags.
    pub tags: Vec<String>,
    /// Extensible author-defined values that do not alter stable identity.
    pub custom_values: BTreeMap<String, serde_json::Value>,
}

/// Versioned Tile Set document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileSetDocument {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Stable tile definitions contained in the set.
    pub tiles: Vec<TileDefinition>,
}

impl TileSetDocument {
    /// Resolves one tile by stable identity.
    pub fn tile(&self, id: &TileId) -> Option<&TileDefinition> {
        self.tiles.iter().find(|tile| &tile.id == id)
    }

    /// Validates first-release stable identity and collision-shape invariants.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != TILE_SET_SCHEMA_VERSION {
            errors.push("unsupported tile set schema".into());
        }
        let mut ids = BTreeMap::new();
        for tile in &self.tiles {
            if ids.insert(tile.id.as_str(), ()).is_some() {
                errors.push(format!("duplicate TileId {}", tile.id.as_str()));
            }
            for shape in &tile.collision {
                match shape {
                    TileCollisionShape::Box { half_extents }
                        if half_extents.iter().any(|value| !value.is_finite() || *value <= 0.0) =>
                    {
                        errors.push(format!("tile {} has invalid box collision", tile.id.as_str()));
                    }
                    TileCollisionShape::Circle { radius } if !radius.is_finite() || *radius <= 0.0 => {
                        errors.push(format!("tile {} has invalid circle collision", tile.id.as_str()));
                    }
                    TileCollisionShape::Polygon { points }
                        if points.len() < 3 || points.iter().flatten().any(|value| !value.is_finite()) =>
                    {
                        errors.push(format!("tile {} has invalid polygon collision", tile.id.as_str()));
                    }
                    _ => {}
                }
            }
        }
        errors
    }
}

impl SpriteAnimationDocument {
    /// Validates exact-duration deterministic playback data.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != SPRITE_ANIMATION_SCHEMA_VERSION {
            errors.push("unsupported sprite animation schema".into());
        }
        if self.ticks_per_second == 0 {
            errors.push("ticks_per_second must be positive".into());
        }
        for (index, frame) in self.frames.iter().enumerate() {
            if frame.duration_ticks == 0 {
                errors.push(format!("frame {index} duration_ticks must be positive"));
            }
        }
        errors
    }
}

/// Integer tile cell coordinate inside a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TileCell {
    /// Horizontal cell coordinate.
    pub x: i32,
    /// Vertical cell coordinate.
    pub y: i32,
}

/// Integer sparse chunk coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TileChunkCoord {
    /// Horizontal chunk coordinate.
    pub x: i32,
    /// Vertical chunk coordinate.
    pub y: i32,
}

/// One sparse chunk. Empty cells are omitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileChunk {
    /// Stable spatial coordinate of this chunk.
    pub coord: TileChunkCoord,
    /// Sparse local-cell map keyed by `x,y` and storing stable TileIds.
    pub cells: BTreeMap<String, TileId>,
}

impl TileChunk {
    fn key(cell: TileCell) -> String {
        format!("{},{}", cell.x, cell.y)
    }

    /// Returns the tile assigned to one local cell, if any.
    pub fn get(&self, cell: TileCell) -> Option<&TileId> {
        self.cells.get(&Self::key(cell))
    }

    /// Assigns or clears one local cell.
    pub fn set(&mut self, cell: TileCell, tile: Option<TileId>) {
        let key = Self::key(cell);
        if let Some(tile) = tile {
            self.cells.insert(key, tile);
        } else {
            self.cells.remove(&key);
        }
    }
}

/// Stable Tile Map layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileMapLayer {
    /// Stable layer identity independent of name or ordering.
    pub id: TileLayerId,
    /// Human-readable layer name.
    pub name: String,
    /// Whether this layer participates in preview and runtime output.
    pub enabled: bool,
    /// Whether authoring tools reject painting into this layer.
    pub locked: bool,
    /// Stable logical sorting layer used by rendered chunks.
    pub sorting_layer: SortingLayerId,
    /// Signed authored order within the sorting layer.
    pub order_in_layer: i32,
    /// Sparse chunks owned by this layer.
    pub chunks: Vec<TileChunk>,
}

/// Versioned sparse chunked Tile Map document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileMapDocument {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Tile Set asset that resolves cell TileIds.
    pub tile_set: AssetId,
    /// Width and height of every sparse chunk in cells.
    pub chunk_size: u16,
    /// Ordered authoring layers with stable identities.
    pub layers: Vec<TileMapLayer>,
}

impl TileMapDocument {
    /// Returns only chunks touched by a bounded edit.
    pub fn affected_chunks(&self, cells: impl IntoIterator<Item = TileCell>) -> Vec<TileChunkCoord> {
        let size = i32::from(self.chunk_size.max(1));
        let mut out = BTreeMap::new();
        for cell in cells { out.insert(TileChunkCoord { x: cell.x.div_euclid(size), y: cell.y.div_euclid(size) }, ()); }
        out.into_keys().collect()
    }
}

/// One gesture-level Tile Map edit with exact cancel/undo snapshot.
#[derive(Debug, Clone)]
pub struct TileMapStroke {
    before: TileMapDocument,
    working: TileMapDocument,
}

impl TileMapStroke {
    /// Begins one semantic stroke from an exact pre-gesture snapshot.
    pub fn begin(document: &TileMapDocument) -> Self {
        Self {
            before: document.clone(),
            working: document.clone(),
        }
    }

    /// Returns the transient document represented by the current gesture.
    pub fn document(&self) -> &TileMapDocument {
        &self.working
    }

    /// Assigns or clears one world-space cell and returns its affected chunk.
    pub fn paint(
        &mut self,
        layer: &TileLayerId,
        cell: TileCell,
        tile: Option<TileId>,
    ) -> Result<TileChunkCoord, &'static str> {
        let size = i32::from(self.working.chunk_size.max(1));
        let coord = TileChunkCoord {
            x: cell.x.div_euclid(size),
            y: cell.y.div_euclid(size),
        };
        let local = TileCell {
            x: cell.x.rem_euclid(size),
            y: cell.y.rem_euclid(size),
        };
        let layer = self
            .working
            .layers
            .iter_mut()
            .find(|candidate| &candidate.id == layer)
            .ok_or("tile layer not found")?;
        if layer.locked {
            return Err("tile layer is locked");
        }
        let chunk = if let Some(index) = layer
            .chunks
            .iter()
            .position(|chunk| chunk.coord == coord)
        {
            &mut layer.chunks[index]
        } else {
            layer.chunks.push(TileChunk {
                coord,
                cells: BTreeMap::new(),
            });
            layer.chunks.last_mut().expect("just pushed")
        };
        chunk.set(local, tile);
        Ok(coord)
    }

    /// Cancels the gesture and restores the exact document from before it began.
    pub fn cancel(self) -> TileMapDocument {
        self.before
    }

    /// Commits the gesture as one complete semantic document result.
    pub fn commit(self) -> TileMapDocument {
        self.working
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sprite_id_survives_rename_and_reorder() {
        let id = SpriteId::generate();
        let other = SpriteId::generate();
        let texture = AssetId::generate();
        let make = |id: SpriteId, name: &str| SpriteRegion { id, name: name.into(), source_texture: texture.clone(), rect: PixelRect { x: 0, y: 0, width: 16, height: 16 }, pivot: [0.5, 0.5], pixels_per_unit: PixelsPerUnit::ProjectDefault, filtering: None, extrusion_pixels: 1 };
        let mut atlas = SpriteAtlasDocument { schema_version: 1, regions: vec![make(id.clone(), "hero"), make(other, "other")] };
        atlas.regions[0].name = "renamed".into(); atlas.regions.swap(0, 1);
        assert_eq!(atlas.region(&id).unwrap().name, "renamed");
    }
}
