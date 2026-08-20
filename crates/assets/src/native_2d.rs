//! Backend-neutral compiled Native 2D sprite and tile data (ADR 0127).

use crate::asset::RuntimeAssetId;
use engine_authoring::{
    AssetId, PixelsPerUnit, SortingLayerId, SpriteAtlasDocument, SpriteFiltering, SpriteId,
    SpriteRef, TileChunk, TileChunkCoord, TileCollisionMaterial, TileCollisionShape, TileId,
    TileLayerId, TileMapDocument, TileSetDocument,
};
use std::collections::{BTreeMap, BTreeSet};
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
        self.chunks
            .iter()
            .find(|chunk| &chunk.layer == layer && chunk.coord == coord)
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
            Self::InvalidDocument(errors) => write!(
                formatter,
                "invalid Native 2D document: {}",
                errors.join("; ")
            ),
            Self::MissingSourceTexture(asset) => write!(
                formatter,
                "Sprite Atlas source texture `{}` is unresolved",
                asset.as_str()
            ),
            Self::MissingTile(tile) => write!(
                formatter,
                "Tile Map references missing TileId `{}`",
                tile.as_str()
            ),
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
        let source_texture = resolve_texture(&region.source_texture).ok_or_else(|| {
            Native2dCompileError::MissingSourceTexture(region.source_texture.clone())
        })?;
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
                rect: [
                    region.rect.x,
                    region.rect.y,
                    region.rect.width,
                    region.rect.height,
                ],
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
pub fn compile_tile_set(
    document: &TileSetDocument,
) -> Result<CompiledTileSet, Native2dCompileError> {
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
            chunks.push(compile_tile_chunk(&layer.id, chunk, tile_set)?);
        }
    }
    chunks.sort_by_key(sorted_chunk_key);
    Ok(CompiledTileMap {
        chunk_size: document.chunk_size,
        layers,
        chunks,
    })
}

/// Compiles one authored chunk against the Tile Set it references.
fn compile_tile_chunk(
    layer: &TileLayerId,
    chunk: &TileChunk,
    tile_set: &CompiledTileSet,
) -> Result<CompiledTileChunk, Native2dCompileError> {
    let mut cells = Vec::with_capacity(chunk.cells.len());
    for entry in &chunk.cells {
        if tile_set.tile(&entry.tile).is_none() {
            return Err(Native2dCompileError::MissingTile(entry.tile.clone()));
        }
        cells.push((entry.cell.x, entry.cell.y, entry.tile.clone()));
    }
    cells.sort_by_key(|(x, y, tile)| (*y, *x, tile.as_str().to_owned()));
    Ok(CompiledTileChunk {
        layer: layer.clone(),
        coord: chunk.coord,
        cells,
    })
}

/// Deterministic compiled-chunk ordering shared by full and incremental compiles.
fn sorted_chunk_key(chunk: &CompiledTileChunk) -> (String, i32, i32) {
    (
        chunk.layer.as_str().to_owned(),
        chunk.coord.y,
        chunk.coord.x,
    )
}

/// Result of updating a compiled Tile Map from a bounded set of changed chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileMapChunkRecompile {
    /// Updated compiled Tile Map.
    pub map: CompiledTileMap,
    /// Chunks rebuilt from the authored document during this update.
    pub recompiled_chunks: Vec<(TileLayerId, TileChunkCoord)>,
    /// Chunks carried over from the previous compile without rebuilding.
    pub reused_chunks: usize,
}

/// Rebuilds only the chunks a bounded edit changed.
///
/// ADR 0127 makes the chunk the update unit: one paint gesture reports the
/// layer/chunk pairs it touched, and only those are rebuilt. Every other chunk
/// is carried over from `previous` exactly as it was compiled.
///
/// A chunk present in the document but absent from `previous` is compiled, and
/// a chunk that the document no longer contains is dropped. Layer records are
/// always taken from the document because they are ordering metadata rather
/// than per-cell work.
///
/// The caller must recompile the whole map when the Tile Set changes: reused
/// chunks were validated against the Tile Set they were compiled with, and this
/// function does not revisit them.
pub fn recompile_tile_map_chunks(
    previous: &CompiledTileMap,
    document: &TileMapDocument,
    tile_set: &CompiledTileSet,
    changed: &[(TileLayerId, TileChunkCoord)],
) -> Result<TileMapChunkRecompile, Native2dCompileError> {
    let errors = document.validate();
    if !errors.is_empty() {
        return Err(Native2dCompileError::InvalidDocument(errors));
    }
    let changed = changed.iter().cloned().collect::<BTreeSet<_>>();
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
    let mut recompiled_chunks = Vec::new();
    let mut reused_chunks = 0;
    for layer in &document.layers {
        for chunk in &layer.chunks {
            let key = (layer.id.clone(), chunk.coord);
            if !changed.contains(&key)
                && let Some(existing) = previous.chunk(&layer.id, chunk.coord)
            {
                chunks.push(existing.clone());
                reused_chunks += 1;
                continue;
            }
            chunks.push(compile_tile_chunk(&layer.id, chunk, tile_set)?);
            recompiled_chunks.push(key);
        }
    }
    chunks.sort_by_key(sorted_chunk_key);
    Ok(TileMapChunkRecompile {
        map: CompiledTileMap {
            chunk_size: document.chunk_size,
            layers,
            chunks,
        },
        recompiled_chunks,
        reused_chunks,
    })
}

/// Validates logical SpriteRefs in a Tile Set through a caller-owned atlas resolver.
pub fn validate_tile_set_sprite_refs(
    document: &TileSetDocument,
    mut sprite_exists: impl FnMut(&SpriteRef) -> bool,
) -> Vec<String> {
    let mut errors = document.validate();
    for tile in &document.tiles {
        if !sprite_exists(&tile.sprite) {
            errors.push(format!(
                "tile `{}` references unresolved sprite `{}`",
                tile.id.as_str(),
                tile.sprite.sprite.as_str()
            ));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        PixelRect, SpriteRegion, TILE_MAP_SCHEMA_VERSION, TILE_SET_SCHEMA_VERSION, TileCell,
        TileCellEntry, TileChunk, TileMapLayer,
    };

    #[test]
    fn tile_compile_keeps_stable_layer_identity_instead_of_vector_index() {
        let tile = TileId::generate();
        let sprite = SpriteRef {
            atlas: AssetId::generate(),
            sprite: SpriteId::generate(),
        };
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
        })
        .unwrap();
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
                    cells: vec![TileCellEntry {
                        cell: TileCell { x: 3, y: 4 },
                        tile,
                    }],
                }],
            }],
        };
        let compiled = compile_tile_map(&map, &set).unwrap();
        assert_eq!(compiled.chunks[0].layer, layer_id);
    }

    fn tile_set_with(tiles: &[(TileId, &str)]) -> CompiledTileSet {
        compile_tile_set(&TileSetDocument {
            schema_version: TILE_SET_SCHEMA_VERSION,
            tiles: tiles
                .iter()
                .map(|(id, name)| engine_authoring::TileDefinition {
                    id: id.clone(),
                    name: (*name).to_owned(),
                    sprite: SpriteRef {
                        atlas: AssetId::generate(),
                        sprite: SpriteId::generate(),
                    },
                    collision: Vec::new(),
                    collision_material: None,
                    one_way: false,
                    tags: Vec::new(),
                    custom_values: BTreeMap::new(),
                })
                .collect(),
        })
        .expect("tile set")
    }

    fn map_with_chunks(
        layer: &TileLayerId,
        tile: &TileId,
        coords: &[(i32, i32)],
    ) -> TileMapDocument {
        TileMapDocument {
            schema_version: TILE_MAP_SCHEMA_VERSION,
            tile_set: AssetId::generate(),
            chunk_size: 32,
            layers: vec![TileMapLayer {
                id: layer.clone(),
                name: "World".to_owned(),
                enabled: true,
                locked: false,
                sorting_layer: SortingLayerId::generate(),
                order_in_layer: 0,
                chunks: coords
                    .iter()
                    .map(|(x, y)| TileChunk {
                        coord: TileChunkCoord { x: *x, y: *y },
                        cells: vec![TileCellEntry {
                            cell: TileCell { x: 1, y: 1 },
                            tile: tile.clone(),
                        }],
                    })
                    .collect(),
            }],
        }
    }

    #[test]
    fn a_changed_chunk_is_rebuilt_while_every_other_chunk_is_reused() {
        let ground = TileId::generate();
        let wall = TileId::generate();
        let set = tile_set_with(&[(ground.clone(), "ground"), (wall.clone(), "wall")]);
        let layer = TileLayerId::generate();
        let document = map_with_chunks(&layer, &ground, &[(0, 0), (1, 0), (0, 1)]);
        let compiled = compile_tile_map(&document, &set).expect("full compile");

        let mut edited = document.clone();
        edited.layers[0].chunks[1].cells[0].tile = wall.clone();
        let changed = [(layer.clone(), TileChunkCoord { x: 1, y: 0 })];
        let update =
            recompile_tile_map_chunks(&compiled, &edited, &set, &changed).expect("incremental");

        assert_eq!(update.recompiled_chunks, changed.to_vec());
        assert_eq!(update.reused_chunks, 2);
        assert_eq!(
            update.map,
            compile_tile_map(&edited, &set).expect("reference compile")
        );
        for coord in [TileChunkCoord { x: 0, y: 0 }, TileChunkCoord { x: 0, y: 1 }] {
            assert_eq!(
                update.map.chunk(&layer, coord),
                compiled.chunk(&layer, coord)
            );
        }
    }

    #[test]
    fn a_chunk_the_previous_compile_never_saw_is_built_even_when_unlisted() {
        let ground = TileId::generate();
        let set = tile_set_with(&[(ground.clone(), "ground")]);
        let layer = TileLayerId::generate();
        let document = map_with_chunks(&layer, &ground, &[(0, 0)]);
        let compiled = compile_tile_map(&document, &set).expect("full compile");
        let grown = map_with_chunks(&layer, &ground, &[(0, 0), (5, 5)]);

        let update = recompile_tile_map_chunks(&compiled, &grown, &set, &[]).expect("incremental");

        assert_eq!(
            update.recompiled_chunks,
            vec![(layer.clone(), TileChunkCoord { x: 5, y: 5 })]
        );
        assert_eq!(update.reused_chunks, 1);
    }

    #[test]
    fn a_removed_chunk_does_not_survive_in_the_updated_compile() {
        let ground = TileId::generate();
        let set = tile_set_with(&[(ground.clone(), "ground")]);
        let layer = TileLayerId::generate();
        let document = map_with_chunks(&layer, &ground, &[(0, 0), (1, 0)]);
        let compiled = compile_tile_map(&document, &set).expect("full compile");
        let shrunk = map_with_chunks(&layer, &ground, &[(0, 0)]);

        let update = recompile_tile_map_chunks(&compiled, &shrunk, &set, &[]).expect("incremental");

        assert_eq!(update.map.chunks.len(), 1);
        assert!(
            update
                .map
                .chunk(&layer, TileChunkCoord { x: 1, y: 0 })
                .is_none()
        );
    }

    #[test]
    fn tile_identity_survives_rename_and_palette_reorder() {
        let ground = TileId::generate();
        let wall = TileId::generate();
        let layer = TileLayerId::generate();
        let set = tile_set_with(&[(ground.clone(), "ground"), (wall.clone(), "wall")]);
        let document = map_with_chunks(&layer, &ground, &[(0, 0)]);
        let before = compile_tile_map(&document, &set).expect("compile");

        // The same tiles renamed and reordered in the palette.
        let renamed = tile_set_with(&[(wall.clone(), "brick"), (ground.clone(), "soil")]);
        let after = compile_tile_map(&document, &renamed).expect("compile");

        assert_eq!(before.chunks, after.chunks);
        assert_eq!(before.chunks[0].cells[0].2, ground);
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
                rect: PixelRect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 16,
                },
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
        })
        .unwrap();
        assert_eq!(compiled.region(&id).unwrap().source_texture, runtime);
    }
}
