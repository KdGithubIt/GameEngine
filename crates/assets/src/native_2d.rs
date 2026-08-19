//! Backend-neutral compiled Native 2D sprite and tile data (ADR 0127).

use crate::asset::RuntimeAssetId;
use engine_authoring::{
    AssetId, PixelsPerUnit, SortingLayerId, SpriteAtlasDocument, SpriteFiltering, SpriteId,
    SpriteRef, TileChunkCoord, TileCollisionMaterial, TileCollisionShape, TileId, TileLayerId,
    TileMapDocument, TileSetDocument,
};
use std::collections::BTreeMap;
use std::fmt;

/// Resolved logical sprite region independent of GPU atlas packing.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledSpriteRegion {
    /// Stable logical SpriteId.
    pub id: SpriteId,
    /// Resolved source texture runtime asset identity.
    pub source_texture: RuntimeAssetId,
    /// Source pixel rectangle `[x, y, width, height]`.
    pub rect: [u32; 4],
    /// Normalized pivot.
    pub pivot: [f32; 2],
    /// Resolved positive pixels per world unit.
    pub pixels_per_unit: f32,
    /// Resolved sampler policy.
    pub filtering: SpriteFiltering,
    /// Source edge-extrusion pixels retained for a later packer.
    pub extrusion_pixels: u8,
}

/// Compiled Sprite Atlas keyed by stable logical identity.
#[derive(Debug, Clone, Default)]
pub struct CompiledSpriteAtlas {
    /// Resolved regions keyed by stable SpriteId.
    pub regions: BTreeMap<SpriteId, CompiledSpriteRegion>,
}

impl CompiledSpriteAtlas {
    /// Resolves a compiled region by stable logical identity.
    pub fn region(&self, id: &SpriteId) -> Option<&CompiledSpriteRegion> {
        self.regions.get(id)
    }
}

/// Backend-neutral compiled Tile Set entry.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTile {
    /// Stable logical TileId.
    pub id: TileId,
    /// Stable SpriteRef retained until render asset resolution.
    pub sprite: SpriteRef,
    /// Backend-neutral collision geometry.
    pub collision: Vec<TileCollisionShape>,
    /// Optional neutral collision material values.
    pub collision_material: Option<TileCollisionMaterial>,
    /// Shared one-way platform policy.
    pub one_way: bool,
    /// Author-defined classification tags.
    pub tags: Vec<String>,
    /// Extensible custom metadata retained without interpretation.
    pub custom_values: BTreeMap<String, serde_json::Value>,
}

/// Compiled Tile Set keyed by stable logical identity.
#[derive(Debug, Clone, Default)]
pub struct CompiledTileSet {
    /// Resolved tile definitions keyed by stable TileId.
    pub tiles: BTreeMap<TileId, CompiledTile>,
}

impl CompiledTileSet {
    /// Resolves one compiled tile by stable identity.
    pub fn tile(&self, id: &TileId) -> Option<&CompiledTile> {
        self.tiles.get(id)
    }
}

/// One compiled sparse Tile Map chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledTileChunk {
    /// Stable layer identity; compile output never substitutes a vector index.
    pub layer: TileLayerId,
    /// Stable spatial chunk coordinate.
    pub coord: TileChunkCoord,
    /// Deterministically ordered `(local_x, local_y, TileId)` cells.
    pub cells: Vec<(i32, i32, TileId)>,
}

/// One compiled Tile Map layer retaining logical draw ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledTileLayer {
    /// Stable Tile Map layer identity.
    pub id: TileLayerId,
    /// Whether runtime output is enabled.
    pub enabled: bool,
    /// Stable project sorting layer.
    pub sorting_layer: SortingLayerId,
    /// Signed authored order inside the sorting layer.
    pub order_in_layer: i32,
}

/// Compiled sparse Tile Map retaining chunk update granularity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledTileMap {
    /// Width and height of each chunk in cells.
    pub chunk_size: u16,
    /// Stable compiled layer records in authored order.
    pub layers: Vec<CompiledTileLayer>,
    /// Deterministically ordered sparse chunks.
    pub chunks: Vec<CompiledTileChunk>,
}

impl CompiledTileMap {
    /// Resolves one compiled chunk by stable layer and spatial coordinate.
    pub fn chunk(&self, layer: &TileLayerId, coord: TileChunkCoord) -> Option<&CompiledTileChunk> {
        self.chunks.iter().find(|chunk| &chunk.layer == layer && chunk.coord == coord)
    }
}

/// Native 2D compile failure before any backend-specific upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Native2dCompileError {
    /// The authored source document was structurally invalid.
    InvalidDocument(Vec<String>),
    /// A Sprite Atlas region referenced an unresolved source texture.
    MissingSourceTexture(AssetId),
    /// A Tile Map cell referenced a TileId absent from its Tile Set.
    MissingTile(TileId),
}

impl fmt::Display for Native2dCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDocument(errors) => write!(formatter, "invalid Native 2D document: {}", errors.join("; ")),
            Self::MissingSourceTexture(asset) => write!(formatter, "Sprite Atlas source texture `{}` is unresolved", asset.as_str()),
            Self::MissingTile(tile) => write!(formatter, "Tile Map references missing TileId `{}`", tile.as_str()),
        }
    }
}

impl std::error::Error for Native2dCompileError {}

/// Compiles one Sprite Atlas without deriving logical identity from ordering or UV packing.
pub fn compile_sprite_atlas(
    document: &SpriteAtlasDocument,
    default_pixels_per_unit: f32,
    default_filtering: SpriteFiltering,
    mut resolve_texture: impl FnMut(&AssetId) -> Option<RuntimeAssetId>,
) -> Result<CompiledSpriteAtlas, Native2dCompileError> {
    let errors = document.validate();
    if !errors.is_empty() {
        return Err(Native2dCompileError::InvalidDocument(errors));
    }
    let mut regions = BTreeMap::new();
    for region in &document.regions {
        let source_texture = resolve_texture(&region.source_texture)
            .ok_or_else(|| Native2dCompileError::MissingSourceTexture(region.source_texture.clone()))?;
        let pixels_per_unit = match region.pixels_per_unit {
            PixelsPerUnit::ProjectDefault => default_pixels_per_unit,
            PixelsPerUnit::Override(value) => value,
        };
        if !pixels_per_unit.is_finite() || pixels_per_unit <= 0.0 {
            return Err(Native2dCompileError::InvalidDocument(vec![
                "resolved Sprite Atlas pixels_per_unit must be finite and positive".to_owned(),
            ]));
        }
        regions.insert(
            region.id.clone(),
            CompiledSpriteRegion {
                id: region.id.clone(),
                source_texture,
                rect: [region.rect.x, region.rect.y, region.rect.width, region.rect.height],
                pivot: region.pivot,
                pixels_per_unit,
                filtering: region.filtering.unwrap_or(default_filtering),
                extrusion_pixels: region.extrusion_pixels,
            },
        );
    }
    Ok(CompiledSpriteAtlas { regions })
}

/// Compiles one Tile Set into stable backend-neutral entries.
pub fn compile_tile_set(document: &TileSetDocument) -> Result<CompiledTileSet, Native2dCompileError> {
    let errors = document.validate();
    if !errors.is_empty() {
        return Err(Native2dCompileError::InvalidDocument(errors));
    }
    Ok(CompiledTileSet {
        tiles: document
            .tiles
            .iter()
            .map(|tile| {
                (
                    tile.id.clone(),
                    CompiledTile {
                        id: tile.id.clone(),
                        sprite: tile.sprite.clone(),
                        collision: tile.collision.clone(),
                        collision_material: tile.collision_material,
                        one_way: tile.one_way,
                        tags: tile.tags.clone(),
                        custom_values: tile.custom_values.clone(),
                    },
                )
            })
            .collect(),
    })
}

/// Compiles one sparse Tile Map while preserving stable layer/chunk identity.
pub fn compile_tile_map(
    document: &TileMapDocument,
    tile_set: &CompiledTileSet,
) -> Result<CompiledTileMap, Native2dCompileError> {
    let errors = document.validate();
    if !errors.is_empty() {
        return Err(Native2dCompileError::InvalidDocument(errors));
    }
    let layers = document
        .layers
        .iter()
        .map(|layer| CompiledTileLayer {
            id: layer.id.clone(),
            enabled: layer.enabled,
            sorting_layer: layer.sorting_layer.clone(),
            order_in_layer: layer.order_in_layer,
        })
        .collect::<Vec<_>>();
    let mut chunks = Vec::new();
    for layer in &document.layers {
        for chunk in &layer.chunks {
            let mut cells = Vec::with_capacity(chunk.cells.len());
            for entry in &chunk.cells {
                if tile_set.tile(&entry.tile).is_none() {
                    return Err(Native2dCompileError::MissingTile(entry.tile.clone()));
                }
                cells.push((entry.cell.x, entry.cell.y, entry.tile.clone()));
            }
            cells.sort_by_key(|(x, y, tile)| (*y, *x, tile.as_str().to_owned()));
            chunks.push(CompiledTileChunk {
                layer: layer.id.clone(),
                coord: chunk.coord,
                cells,
            });
        }
    }
    chunks.sort_by_key(|chunk| (chunk.layer.as_str().to_owned(), chunk.coord.y, chunk.coord.x));
    Ok(CompiledTileMap { chunk_size: document.chunk_size, layers, chunks })
}

/// Validates logical SpriteRefs in a Tile Set through a caller-owned atlas resolver.
pub fn validate_tile_set_sprite_refs(
    document: &TileSetDocument,
    mut sprite_exists: impl FnMut(&SpriteRef) -> bool,
) -> Vec<String> {
    let mut errors = document.validate();
    for tile in &document.tiles {
        if !sprite_exists(&tile.sprite) {
            errors.push(format!("tile `{}` references unresolved sprite `{}`", tile.id.as_str(), tile.sprite.sprite.as_str()));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        PixelRect, SpriteRegion, TileCell, TileCellEntry, TileChunk, TileMapLayer,
        TILE_MAP_SCHEMA_VERSION, TILE_SET_SCHEMA_VERSION,
    };

    #[test]
    fn tile_compile_keeps_stable_layer_identity_instead_of_vector_index() {
        let tile = TileId::generate();
        let sprite = SpriteRef { atlas: AssetId::generate(), sprite: SpriteId::generate() };
        let set = compile_tile_set(&TileSetDocument {
            schema_version: TILE_SET_SCHEMA_VERSION,
            tiles: vec![engine_authoring::TileDefinition {
                id: tile.clone(),
                name: "ground".to_owned(),
                sprite,
                collision: Vec::new(),
                collision_material: None,
                one_way: false,
                tags: Vec::new(),
                custom_values: BTreeMap::new(),
            }],
        }).unwrap();
        let layer_id = TileLayerId::generate();
        let map = TileMapDocument {
            schema_version: TILE_MAP_SCHEMA_VERSION,
            tile_set: AssetId::generate(),
            chunk_size: 32,
            layers: vec![TileMapLayer {
                id: layer_id.clone(),
                name: "World".to_owned(),
                enabled: true,
                locked: false,
                sorting_layer: SortingLayerId::generate(),
                order_in_layer: 0,
                chunks: vec![TileChunk {
                    coord: TileChunkCoord { x: 2, y: -1 },
                    cells: vec![TileCellEntry { cell: TileCell { x: 3, y: 4 }, tile }],
                }],
            }],
        };
        let compiled = compile_tile_map(&map, &set).unwrap();
        assert_eq!(compiled.chunks[0].layer, layer_id);
    }

    #[test]
    fn sprite_compile_resolves_source_texture_but_not_gpu_uvs() {
        let texture = AssetId::generate();
        let id = SpriteId::generate();
        let document = SpriteAtlasDocument {
            schema_version: engine_authoring::SPRITE_ATLAS_SCHEMA_VERSION,
            regions: vec![SpriteRegion {
                id: id.clone(),
                name: "hero".to_owned(),
                source_texture: texture.clone(),
                rect: PixelRect { x: 0, y: 0, width: 16, height: 16 },
                pivot: [0.5, 0.5],
                pixels_per_unit: PixelsPerUnit::ProjectDefault,
                filtering: None,
                extrusion_pixels: 1,
            }],
        };
        let mut store = crate::asset::Assets::new();
        let runtime = store.add(()).id();
        let compiled = compile_sprite_atlas(&document, 100.0, SpriteFiltering::Nearest, |asset| {
            (asset == &texture).then_some(runtime)
        }).unwrap();
        assert_eq!(compiled.region(&id).unwrap().source_texture, runtime);
    }
}
