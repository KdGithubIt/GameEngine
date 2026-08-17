//! Backend-neutral compiled sprite and tile data (ADR 0127).

use engine_authoring::{
    SpriteAtlasDocument, SpriteId, SpriteRef, TileChunkCoord, TileId, TileMapDocument,
    TileSetDocument,
};
use std::collections::BTreeMap;

/// Resolved sprite region independent of GPU page packing.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledSpriteRegion {
    /// Stable logical sprite identity.
    pub id: SpriteId,
    /// Runtime ID of the resolved source texture.
    pub source_texture: crate::asset::RuntimeAssetId,
    /// Source pixel rectangle `[x, y, width, height]`.
    pub rect: [u32; 4],
    /// Normalized pivot inside the region.
    pub pivot: [f32; 2],
    /// Resolved pixels-per-world-unit value.
    pub pixels_per_unit: f32,
}

/// Runtime sprite atlas keyed by stable logical identity.
#[derive(Debug, Clone, Default)]
pub struct CompiledSpriteAtlas {
    /// Resolved regions keyed by stable `SpriteId`.
    pub regions: BTreeMap<SpriteId, CompiledSpriteRegion>,
}

impl CompiledSpriteAtlas {
    /// Resolves one compiled sprite region by stable identity.
    pub fn region(&self, id: &SpriteId) -> Option<&CompiledSpriteRegion> {
        self.regions.get(id)
    }
}

/// Backend-neutral tile definition resolved to stable SpriteRef and collision metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTile {
    /// Stable logical tile identity.
    pub id: TileId,
    /// Stable sprite reference used to render the tile.
    pub sprite: SpriteRef,
    /// Whether the shared one-way collision policy applies.
    pub one_way: bool,
}

/// One compiled sparse Tile Map chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTileChunk {
    /// Index of the authoring layer containing this chunk.
    pub layer_index: usize,
    /// Stable spatial chunk coordinate.
    pub coord: TileChunkCoord,
    /// Deterministically ordered local `(x, y, TileId)` cells.
    pub cells: Vec<(i32, i32, TileId)>,
}

/// Compiled map retaining chunk granularity for independent render/physics consumers.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledTileMap {
    /// Width and height of each chunk in cells.
    pub chunk_size: u16,
    /// Deterministically ordered sparse chunks.
    pub chunks: Vec<CompiledTileChunk>,
}

impl CompiledTileMap {
    /// Returns one chunk by layer index and spatial coordinate.
    pub fn chunk(&self, layer: usize, coord: TileChunkCoord) -> Option<&CompiledTileChunk> {
        self.chunks
            .iter()
            .find(|chunk| chunk.layer_index == layer && chunk.coord == coord)
    }
}

/// Compiles sparse authored chunks without deriving identity from ordering.
pub fn compile_tile_map(document: &TileMapDocument) -> CompiledTileMap {
    let mut chunks = Vec::new();
    for (layer_index, layer) in document.layers.iter().enumerate() {
        for chunk in &layer.chunks {
            let mut cells = Vec::new();
            for (key, tile) in &chunk.cells {
                if let Some((x, y)) = key.split_once(',').and_then(|(x, y)| {
                    Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?))
                }) {
                    cells.push((x, y, tile.clone()));
                }
            }
            cells.sort_by_key(|(x, y, tile)| (*y, *x, tile.as_str().to_owned()));
            chunks.push(CompiledTileChunk {
                layer_index,
                coord: chunk.coord,
                cells,
            });
        }
    }
    chunks.sort_by_key(|chunk| (chunk.layer_index, chunk.coord.y, chunk.coord.x));
    CompiledTileMap {
        chunk_size: document.chunk_size,
        chunks,
    }
}

/// Validates that referenced stable identities resolve before runtime conversion.
pub fn validate_2d_asset_references(
    atlas: &SpriteAtlasDocument,
    tiles: &TileSetDocument,
) -> Vec<String> {
    let mut errors = atlas.validate();
    for tile in &tiles.tiles {
        if atlas.region(&tile.sprite.sprite).is_none() {
            errors.push(format!(
                "tile {} references missing sprite {}",
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

    #[test]
    fn empty_map_compiles_without_phantom_chunks() {
        let map = TileMapDocument {
            schema_version: 1,
            tile_set: engine_authoring::AssetId::generate(),
            chunk_size: 32,
            layers: vec![],
        };
        assert!(compile_tile_map(&map).chunks.is_empty());
    }
}
