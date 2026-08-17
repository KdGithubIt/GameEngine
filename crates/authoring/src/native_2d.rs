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
pub enum SpriteFiltering { Nearest, Linear }

/// Pixel-preview policy for 2D authoring and Camera2D.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelPreviewPolicy { Off, Advisory, PixelPerfect }

/// Stable named logical draw layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortingLayer { pub id: SortingLayerId, pub name: String }

/// Typed project 2D defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project2dSettings {
    pub default_pixels_per_unit: f32,
    pub default_filtering: SpriteFiltering,
    pub gravity: [f32; 2],
    pub pixel_preview: PixelPreviewPolicy,
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
pub struct PixelRect { pub x: u32, pub y: u32, pub width: u32, pub height: u32 }

/// Stable logical sprite reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteRef { pub atlas: AssetId, pub sprite: SpriteId }

/// Pixels-per-unit selection for one sprite region.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelsPerUnit { ProjectDefault, Override(f32) }

/// One stable sprite region in an atlas source document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteRegion {
    pub id: SpriteId,
    pub name: String,
    pub source_texture: AssetId,
    pub rect: PixelRect,
    pub pivot: [f32; 2],
    pub pixels_per_unit: PixelsPerUnit,
    pub filtering: Option<SpriteFiltering>,
    pub extrusion_pixels: u8,
}

/// Versioned Sprite Atlas authoring asset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteAtlasDocument { pub schema_version: u32, pub regions: Vec<SpriteRegion> }

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
    pub sprite: SpriteRef,
    pub tint: [f32; 4],
    pub flip_x: bool,
    pub flip_y: bool,
    pub sorting_layer: SortingLayerId,
    pub order_in_layer: i32,
    pub visible: bool,
}

/// One exact-duration sprite-animation frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteAnimationFrame { pub sprite: SpriteRef, pub duration_ticks: u32, pub event: Option<String> }

/// Versioned immutable sprite-animation clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteAnimationDocument {
    pub schema_version: u32,
    pub ticks_per_second: u32,
    pub looping: bool,
    pub frames: Vec<SpriteAnimationFrame>,
}

/// Persisted SpriteAnimator2D component settings; live frame/time are runtime state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteAnimator2d {
    pub clip: AssetId,
    pub autoplay: bool,
    pub speed: f32,
    pub looping_override: Option<bool>,
}

/// Collision material stored without backend-specific types.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PhysicsMaterial2d { pub friction: f32, pub restitution: f32 }

/// Backend-neutral tile collision shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TileCollisionShape { Box { half_extents: [f32; 2] }, Circle { radius: f32 }, Polygon { points: Vec<[f32; 2]> } }

/// One stable tile definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileDefinition {
    pub id: TileId,
    pub name: String,
    pub sprite: SpriteRef,
    pub collision: Vec<TileCollisionShape>,
    pub one_way: bool,
    pub tags: Vec<String>,
    pub custom_values: BTreeMap<String, serde_json::Value>,
}

/// Versioned Tile Set document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileSetDocument { pub schema_version: u32, pub tiles: Vec<TileDefinition> }
impl TileSetDocument {
    /// Resolves one tile by stable identity.
    pub fn tile(&self, id: &TileId) -> Option<&TileDefinition> { self.tiles.iter().find(|tile| &tile.id == id) }
    /// Validates first-release stable identity and collision-shape invariants.
    pub fn validate(&self) -> Vec<String> {
        let mut errors=Vec::new(); if self.schema_version!=TILE_SET_SCHEMA_VERSION{errors.push("unsupported tile set schema".into());}
        let mut ids=BTreeMap::new(); for tile in &self.tiles { if ids.insert(tile.id.as_str(),()).is_some(){errors.push(format!("duplicate TileId {}",tile.id.as_str()));} for shape in &tile.collision { match shape { TileCollisionShape::Box{half_extents} if half_extents.iter().any(|v| !v.is_finite() || *v<=0.0)=>errors.push(format!("tile {} has invalid box collision",tile.id.as_str())), TileCollisionShape::Circle{radius} if !radius.is_finite() || *radius<=0.0=>errors.push(format!("tile {} has invalid circle collision",tile.id.as_str())), TileCollisionShape::Polygon{points} if points.len()<3 || points.iter().flatten().any(|v| !v.is_finite())=>errors.push(format!("tile {} has invalid polygon collision",tile.id.as_str())), _=>{} } } } errors
    }
}

impl SpriteAnimationDocument {
    /// Validates exact-duration deterministic playback data.
    pub fn validate(&self)->Vec<String>{let mut errors=Vec::new();if self.schema_version!=SPRITE_ANIMATION_SCHEMA_VERSION{errors.push("unsupported sprite animation schema".into());}if self.ticks_per_second==0{errors.push("ticks_per_second must be positive".into());}for(index,frame)in self.frames.iter().enumerate(){if frame.duration_ticks==0{errors.push(format!("frame {index} duration_ticks must be positive"));}}errors}
}

/// Integer tile cell coordinate inside a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TileCell { pub x: i32, pub y: i32 }
/// Integer sparse chunk coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TileChunkCoord { pub x: i32, pub y: i32 }
/// One sparse chunk. Empty cells are omitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileChunk { pub coord: TileChunkCoord, pub cells: BTreeMap<String, TileId> }

impl TileChunk {
    fn key(cell: TileCell) -> String { format!("{},{}", cell.x, cell.y) }
    pub fn get(&self, cell: TileCell) -> Option<&TileId> { self.cells.get(&Self::key(cell)) }
    pub fn set(&mut self, cell: TileCell, tile: Option<TileId>) {
        let key = Self::key(cell);
        if let Some(tile) = tile { self.cells.insert(key, tile); } else { self.cells.remove(&key); }
    }
}

/// Stable Tile Map layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileMapLayer {
    pub id: TileLayerId,
    pub name: String,
    pub enabled: bool,
    pub locked: bool,
    pub sorting_layer: SortingLayerId,
    pub order_in_layer: i32,
    pub chunks: Vec<TileChunk>,
}

/// Versioned sparse chunked Tile Map document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileMapDocument {
    pub schema_version: u32,
    pub tile_set: AssetId,
    pub chunk_size: u16,
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
pub struct TileMapStroke { before: TileMapDocument, working: TileMapDocument }
impl TileMapStroke {
    pub fn begin(document: &TileMapDocument) -> Self { Self { before: document.clone(), working: document.clone() } }
    pub fn document(&self) -> &TileMapDocument { &self.working }
    pub fn paint(&mut self, layer: &TileLayerId, cell: TileCell, tile: Option<TileId>) -> Result<TileChunkCoord, &'static str> {
        let size = i32::from(self.working.chunk_size.max(1));
        let coord = TileChunkCoord { x: cell.x.div_euclid(size), y: cell.y.div_euclid(size) };
        let local = TileCell { x: cell.x.rem_euclid(size), y: cell.y.rem_euclid(size) };
        let layer = self.working.layers.iter_mut().find(|candidate| &candidate.id == layer).ok_or("tile layer not found")?;
        if layer.locked { return Err("tile layer is locked"); }
        let chunk = if let Some(index) = layer.chunks.iter().position(|chunk| chunk.coord == coord) { &mut layer.chunks[index] } else {
            layer.chunks.push(TileChunk { coord, cells: BTreeMap::new() });
            layer.chunks.last_mut().expect("just pushed")
        };
        chunk.set(local, tile);
        Ok(coord)
    }
    pub fn cancel(self) -> TileMapDocument { self.before }
    pub fn commit(self) -> TileMapDocument { self.working }
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
