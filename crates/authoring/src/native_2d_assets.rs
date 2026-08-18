//! Native 2D sprite, animation, and tile authoring contracts (ADR 0127).
//!
//! Persisted content stores stable logical IDs and authoring values only.
//! Runtime handles, packed UVs, solver handles, and GPU page identities are
//! resolved after this boundary.

use crate::id::AssetId;
use crate::native_2d::{SortingLayerId, SpriteFiltering};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Sprite Atlas document schema version.
pub const SPRITE_ATLAS_SCHEMA_VERSION: u32 = 1;
/// Sprite Animation document schema version.
pub const SPRITE_ANIMATION_SCHEMA_VERSION: u32 = 1;
/// Tile Set document schema version.
pub const TILE_SET_SCHEMA_VERSION: u32 = 1;
/// Tile Map document schema version.
pub const TILE_MAP_SCHEMA_VERSION: u32 = 1;

macro_rules! stable_2d_id {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("Stable persisted identifier with prefix `", $prefix, "_`.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generates a new opaque stable identifier.
            pub fn generate() -> Self {
                Self(format!(concat!($prefix, "_{}"), ulid::Ulid::new()))
            }

            /// Parses and validates a persisted identifier.
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

            /// Returns the persisted opaque identifier.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

/// Stable Native 2D identifier validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Native2dIdError {
    /// The identifier did not use the expected typed prefix.
    WrongPrefix(String),
    /// The suffix after the typed prefix was not a valid ULID.
    InvalidUlid(String),
}

impl fmt::Display for Native2dIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPrefix(value) => write!(formatter, "Native 2D ID `{value}` has the wrong prefix"),
            Self::InvalidUlid(value) => write!(formatter, "Native 2D ID `{value}` has an invalid ULID suffix"),
        }
    }
}

impl std::error::Error for Native2dIdError {}

stable_2d_id!(SpriteId, "sprite");
stable_2d_id!(TileId, "tile");
stable_2d_id!(TileLayerId, "tile_layer");

/// Integer source-texture pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PixelRect {
    /// Left edge in source pixels.
    pub x: u32,
    /// Top edge in source pixels.
    pub y: u32,
    /// Width in source pixels.
    pub width: u32,
    /// Height in source pixels.
    pub height: u32,
}

/// Stable logical reference to one Sprite Atlas region.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteRef {
    /// Sprite Atlas asset containing the logical region.
    pub atlas: AssetId,
    /// Stable region identity inside the atlas.
    pub sprite: SpriteId,
}

/// Pixels-per-unit selection for one sprite region.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelsPerUnit {
    /// Use the project-wide Native 2D default.
    ProjectDefault,
    /// Use the region-specific positive value.
    Override(f32),
}

/// Blend policy for one SpriteRenderer2D.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpriteBlendMode {
    /// Straight-alpha source over destination.
    Alpha,
    /// Premultiplied-alpha source over destination.
    PremultipliedAlpha,
    /// Add source color to the destination.
    Additive,
}

impl Default for SpriteBlendMode {
    fn default() -> Self {
        Self::Alpha
    }
}

/// One stable sprite region in a Sprite Atlas document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteRegion {
    /// Stable logical identity.
    pub id: SpriteId,
    /// Human-readable name; rename does not alter [`Self::id`].
    pub name: String,
    /// Source texture asset.
    pub source_texture: AssetId,
    /// Integer source pixel bounds.
    pub rect: PixelRect,
    /// Normalized pivot in `[0, 1]` on both axes.
    pub pivot: [f32; 2],
    /// World-unit scale policy.
    pub pixels_per_unit: PixelsPerUnit,
    /// Optional sampler override; `None` uses project defaults.
    pub filtering: Option<SpriteFiltering>,
    /// Edge pixels reserved for packed-atlas bleed prevention.
    pub extrusion_pixels: u8,
}

/// Versioned `*.spriteatlas.json` authoring document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteAtlasDocument {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Stable logical sprite regions.
    pub regions: Vec<SpriteRegion>,
}

impl SpriteAtlasDocument {
    /// Parses a Sprite Atlas from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serializes canonical human-readable JSON with a trailing newline.
    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }

    /// Resolves one region by stable logical identity.
    pub fn region(&self, id: &SpriteId) -> Option<&SpriteRegion> {
        self.regions.iter().find(|region| &region.id == id)
    }

    /// Validates first-release Sprite Atlas invariants.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != SPRITE_ATLAS_SCHEMA_VERSION {
            errors.push(format!("unsupported Sprite Atlas schema version {}", self.schema_version));
        }
        let mut ids = BTreeSet::new();
        for region in &self.regions {
            if !ids.insert(region.id.clone()) {
                errors.push(format!("duplicate SpriteId `{}`", region.id.as_str()));
            }
            if region.name.trim().is_empty() {
                errors.push(format!("sprite `{}` has an empty name", region.id.as_str()));
            }
            if region.rect.width == 0 || region.rect.height == 0 {
                errors.push(format!("sprite `{}` has an empty pixel rect", region.id.as_str()));
            }
            if region.pivot.iter().any(|value| !value.is_finite() || !(0.0..=1.0).contains(value)) {
                errors.push(format!("sprite `{}` pivot must be finite and normalized", region.id.as_str()));
            }
            if let PixelsPerUnit::Override(value) = region.pixels_per_unit
                && (!value.is_finite() || value <= 0.0)
            {
                errors.push(format!("sprite `{}` pixels_per_unit must be finite and positive", region.id.as_str()));
            }
        }
        errors
    }
}

/// Stable persisted SpriteRenderer2D authoring data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteRenderer2d {
    /// Logical Sprite Atlas region.
    pub sprite: SpriteRef,
    /// Linear RGBA tint multiplier.
    pub tint: [f32; 4],
    /// Mirror texture coordinates horizontally.
    pub flip_x: bool,
    /// Mirror texture coordinates vertically.
    pub flip_y: bool,
    /// Stable logical project sorting layer.
    pub sorting_layer: SortingLayerId,
    /// Signed authored order inside the sorting layer.
    pub order_in_layer: i32,
    /// Whether the sprite contributes a draw.
    pub visible: bool,
    /// Author-visible blend policy.
    #[serde(default)]
    pub blend: SpriteBlendMode,
    /// Optional material asset override. Runtime handles are never persisted.
    #[serde(default)]
    pub material_override: Option<AssetId>,
}

/// One exact-duration Sprite Animation frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteAnimationFrame {
    /// Sprite displayed by this frame.
    pub sprite: SpriteRef,
    /// Positive duration in the clip integer tick domain.
    pub duration_ticks: u32,
    /// Optional named event emitted when playback enters the frame.
    #[serde(default)]
    pub event: Option<String>,
}

/// Versioned immutable `*.spriteanim.json` clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteAnimationDocument {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Integer timebase used by every frame duration.
    pub ticks_per_second: u32,
    /// Default looping behavior.
    pub looping: bool,
    /// Default playback-speed multiplier.
    #[serde(default = "default_sprite_animation_speed")]
    pub default_speed: f32,
    /// Ordered immutable frames.
    pub frames: Vec<SpriteAnimationFrame>,
}

fn default_sprite_animation_speed() -> f32 {
    1.0
}

impl SpriteAnimationDocument {
    /// Parses a Sprite Animation from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serializes canonical human-readable JSON with a trailing newline.
    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }

    /// Validates deterministic fixed-time playback data.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != SPRITE_ANIMATION_SCHEMA_VERSION {
            errors.push(format!("unsupported Sprite Animation schema version {}", self.schema_version));
        }
        if self.ticks_per_second == 0 {
            errors.push("Sprite Animation ticks_per_second must be positive".to_owned());
        }
        if !self.default_speed.is_finite() || self.default_speed < 0.0 {
            errors.push("Sprite Animation default_speed must be finite and non-negative".to_owned());
        }
        if self.frames.is_empty() {
            errors.push("Sprite Animation must contain at least one frame".to_owned());
        }
        for (index, frame) in self.frames.iter().enumerate() {
            if frame.duration_ticks == 0 {
                errors.push(format!("Sprite Animation frame {index} duration_ticks must be positive"));
            }
            if frame.event.as_ref().is_some_and(|event| event.trim().is_empty()) {
                errors.push(format!("Sprite Animation frame {index} event name is empty"));
            }
        }
        errors
    }
}

/// Persisted SpriteAnimator2D settings. Live frame/time state remains runtime-owned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteAnimator2d {
    /// Sprite Animation asset selected for playback.
    pub clip: AssetId,
    /// Whether playback starts when the runtime component becomes active.
    pub autoplay: bool,
    /// Non-negative per-instance speed multiplier applied to the clip default.
    pub speed: f32,
    /// Optional per-instance looping override.
    #[serde(default)]
    pub looping_override: Option<bool>,
    /// Initial frame selected when the component becomes active.
    #[serde(default)]
    pub initial_frame: usize,
}

/// Backend-neutral tile collision geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TileCollisionShape {
    /// Axis-aligned box centered on the tile origin.
    Box {
        /// Positive X/Y half extents in tile-local world units.
        half_extents: [f32; 2],
    },
    /// Circle centered on the tile origin.
    Circle {
        /// Positive radius in tile-local world units.
        radius: f32,
    },
    /// Simple polygon described by tile-local vertices.
    Polygon {
        /// Ordered finite polygon vertices.
        points: Vec<[f32; 2]>,
    },
}

/// Backend-neutral collision material metadata for one tile.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TileCollisionMaterial {
    /// Non-negative friction coefficient.
    pub friction: f32,
    /// Restitution in `[0, 1]`.
    pub restitution: f32,
}

/// One stable tile definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileDefinition {
    /// Stable logical tile identity.
    pub id: TileId,
    /// Human-readable name; rename does not alter [`Self::id`].
    pub name: String,
    /// Sprite used to render this tile.
    pub sprite: SpriteRef,
    /// Backend-neutral collision geometry.
    #[serde(default)]
    pub collision: Vec<TileCollisionShape>,
    /// Optional collision material metadata.
    #[serde(default)]
    pub collision_material: Option<TileCollisionMaterial>,
    /// Whether collision follows the shared one-way platform policy.
    #[serde(default)]
    pub one_way: bool,
    /// Author-defined classification tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Extensible custom authoring metadata.
    #[serde(default)]
    pub custom_values: BTreeMap<String, serde_json::Value>,
}

/// Versioned `*.tileset.json` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileSetDocument {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Stable logical tile definitions.
    pub tiles: Vec<TileDefinition>,
}

impl TileSetDocument {
    /// Parses a Tile Set from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serializes canonical human-readable JSON with a trailing newline.
    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }

    /// Resolves one tile by stable logical identity.
    pub fn tile(&self, id: &TileId) -> Option<&TileDefinition> {
        self.tiles.iter().find(|tile| &tile.id == id)
    }

    /// Validates first-release Tile Set invariants.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != TILE_SET_SCHEMA_VERSION {
            errors.push(format!("unsupported Tile Set schema version {}", self.schema_version));
        }
        let mut ids = BTreeSet::new();
        for tile in &self.tiles {
            if !ids.insert(tile.id.clone()) {
                errors.push(format!("duplicate TileId `{}`", tile.id.as_str()));
            }
            if tile.name.trim().is_empty() {
                errors.push(format!("tile `{}` has an empty name", tile.id.as_str()));
            }
            if let Some(material) = tile.collision_material
                && (!material.friction.is_finite()
                    || material.friction < 0.0
                    || !material.restitution.is_finite()
                    || !(0.0..=1.0).contains(&material.restitution))
            {
                errors.push(format!("tile `{}` has invalid collision material values", tile.id.as_str()));
            }
            for shape in &tile.collision {
                let valid = match shape {
                    TileCollisionShape::Box { half_extents } => half_extents.iter().all(|value| value.is_finite() && *value > 0.0),
                    TileCollisionShape::Circle { radius } => radius.is_finite() && *radius > 0.0,
                    TileCollisionShape::Polygon { points } => points.len() >= 3
                        && points.iter().all(|point| point.iter().all(|value| value.is_finite())),
                };
                if !valid {
                    errors.push(format!("tile `{}` has invalid collision geometry", tile.id.as_str()));
                }
            }
        }
        errors
    }
}

/// Integer cell coordinate. World cells and chunk-local cells share this value type.
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

/// One sparse local cell assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileCellEntry {
    /// Chunk-local cell coordinate.
    pub cell: TileCell,
    /// Stable TileId assigned to the cell.
    pub tile: TileId,
}

/// One sparse Tile Map chunk. Empty cells are omitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileChunk {
    /// Stable spatial chunk coordinate.
    pub coord: TileChunkCoord,
    /// Deterministically ordered sparse cell assignments.
    pub cells: Vec<TileCellEntry>,
}

impl TileChunk {
    /// Returns the tile assigned to one local cell.
    pub fn get(&self, cell: TileCell) -> Option<&TileId> {
        self.cells.iter().find(|entry| entry.cell == cell).map(|entry| &entry.tile)
    }

    /// Assigns or clears one local cell while retaining deterministic ordering.
    pub fn set(&mut self, cell: TileCell, tile: Option<TileId>) {
        match (self.cells.iter().position(|entry| entry.cell == cell), tile) {
            (Some(index), Some(tile)) => self.cells[index].tile = tile,
            (Some(index), None) => { self.cells.remove(index); },
            (None, Some(tile)) => self.cells.push(TileCellEntry { cell, tile }),
            (None, None) => {}
        }
        self.cells.sort_by_key(|entry| (entry.cell.y, entry.cell.x, entry.tile.as_str().to_owned()));
    }
}

/// Stable Tile Map authoring layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileMapLayer {
    /// Stable identity independent of name or ordering.
    pub id: TileLayerId,
    /// Human-readable layer name.
    pub name: String,
    /// Whether the layer contributes preview/runtime output.
    pub enabled: bool,
    /// Whether painting tools reject writes to the layer.
    pub locked: bool,
    /// Stable project sorting layer.
    pub sorting_layer: SortingLayerId,
    /// Signed order inside the project sorting layer.
    pub order_in_layer: i32,
    /// Sparse fixed-size chunks.
    pub chunks: Vec<TileChunk>,
}

/// Versioned sparse `*.tilemap.json` document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileMapDocument {
    /// Serialized schema version.
    pub schema_version: u32,
    /// Tile Set asset that resolves cell TileIds.
    pub tile_set: AssetId,
    /// Width and height of every sparse chunk in cells.
    pub chunk_size: u16,
    /// Ordered layers with stable identities.
    pub layers: Vec<TileMapLayer>,
}

impl TileMapDocument {
    /// Parses a Tile Map from JSON.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serializes canonical human-readable JSON with a trailing newline.
    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        let mut copy = self.clone();
        for layer in &mut copy.layers {
            layer.chunks.sort_by_key(|chunk| (chunk.coord.y, chunk.coord.x));
            for chunk in &mut layer.chunks {
                chunk.cells.sort_by_key(|entry| (entry.cell.y, entry.cell.x, entry.tile.as_str().to_owned()));
            }
        }
        let mut json = serde_json::to_string_pretty(&copy)?;
        json.push('\n');
        Ok(json)
    }

    /// Converts one world cell to its owning chunk coordinate and local cell.
    pub fn split_cell(&self, cell: TileCell) -> (TileChunkCoord, TileCell) {
        let size = i32::from(self.chunk_size.max(1));
        (
            TileChunkCoord { x: cell.x.div_euclid(size), y: cell.y.div_euclid(size) },
            TileCell { x: cell.x.rem_euclid(size), y: cell.y.rem_euclid(size) },
        )
    }

    /// Returns the tile assigned to one world cell on one stable layer.
    pub fn tile_at(&self, layer_id: &TileLayerId, cell: TileCell) -> Option<&TileId> {
        let (coord, local) = self.split_cell(cell);
        self.layers
            .iter()
            .find(|layer| &layer.id == layer_id)?
            .chunks
            .iter()
            .find(|chunk| chunk.coord == coord)?
            .get(local)
    }

    /// Validates first-release sparse chunk and stable-layer invariants.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.schema_version != TILE_MAP_SCHEMA_VERSION {
            errors.push(format!("unsupported Tile Map schema version {}", self.schema_version));
        }
        if self.chunk_size == 0 {
            errors.push("Tile Map chunk_size must be positive".to_owned());
        }
        let size = i32::from(self.chunk_size.max(1));
        let mut layer_ids = BTreeSet::new();
        for layer in &self.layers {
            if !layer_ids.insert(layer.id.clone()) {
                errors.push(format!("duplicate TileLayerId `{}`", layer.id.as_str()));
            }
            if layer.name.trim().is_empty() {
                errors.push(format!("tile layer `{}` has an empty name", layer.id.as_str()));
            }
            let mut chunk_coords = BTreeSet::new();
            for chunk in &layer.chunks {
                if !chunk_coords.insert(chunk.coord) {
                    errors.push(format!("tile layer `{}` has duplicate chunk {},{}", layer.id.as_str(), chunk.coord.x, chunk.coord.y));
                }
                let mut cells = BTreeSet::new();
                for entry in &chunk.cells {
                    if entry.cell.x < 0 || entry.cell.y < 0 || entry.cell.x >= size || entry.cell.y >= size {
                        errors.push(format!("tile layer `{}` chunk {},{} contains out-of-range local cell {},{}", layer.id.as_str(), chunk.coord.x, chunk.coord.y, entry.cell.x, entry.cell.y));
                    }
                    if !cells.insert(entry.cell) {
                        errors.push(format!("tile layer `{}` chunk {},{} contains duplicate local cell {},{}", layer.id.as_str(), chunk.coord.x, chunk.coord.y, entry.cell.x, entry.cell.y));
                    }
                }
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_sprite_id_survives_rename_and_reorder() {
        let id = SpriteId::generate();
        let other = SpriteId::generate();
        let texture = AssetId::generate();
        let region = |id: SpriteId, name: &str| SpriteRegion {
            id,
            name: name.to_owned(),
            source_texture: texture.clone(),
            rect: PixelRect { x: 0, y: 0, width: 16, height: 16 },
            pivot: [0.5, 0.5],
            pixels_per_unit: PixelsPerUnit::ProjectDefault,
            filtering: None,
            extrusion_pixels: 1,
        };
        let mut atlas = SpriteAtlasDocument {
            schema_version: SPRITE_ATLAS_SCHEMA_VERSION,
            regions: vec![region(id.clone(), "hero"), region(other, "other")],
        };
        atlas.regions[0].name = "renamed".to_owned();
        atlas.regions.swap(0, 1);
        assert_eq!(atlas.region(&id).unwrap().name, "renamed");
    }

    #[test]
    fn negative_world_cells_use_euclidean_chunk_coordinates() {
        let document = TileMapDocument {
            schema_version: TILE_MAP_SCHEMA_VERSION,
            tile_set: AssetId::generate(),
            chunk_size: 32,
            layers: Vec::new(),
        };
        assert_eq!(
            document.split_cell(TileCell { x: -1, y: -33 }),
            (TileChunkCoord { x: -1, y: -2 }, TileCell { x: 31, y: 31 })
        );
    }
}
