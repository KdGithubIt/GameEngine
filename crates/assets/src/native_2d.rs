//! Backend-neutral compiled sprite and tile data (ADR 0127).

use engine_authoring::{SpriteAtlasDocument, SpriteId, SpriteRef, TileChunkCoord, TileId, TileMapDocument, TileSetDocument};
use std::collections::BTreeMap;

/// Resolved sprite region independent of GPU page packing.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledSpriteRegion { pub id: SpriteId, pub source_texture: crate::asset::RuntimeAssetId, pub rect:[u32;4], pub pivot:[f32;2], pub pixels_per_unit:f32 }
/// Runtime sprite atlas keyed by stable logical identity.
#[derive(Debug, Clone, Default)] pub struct CompiledSpriteAtlas { pub regions:BTreeMap<SpriteId, CompiledSpriteRegion> }
impl CompiledSpriteAtlas { pub fn region(&self, id:&SpriteId)->Option<&CompiledSpriteRegion>{self.regions.get(id)} }

/// Backend-neutral tile definition resolved to stable SpriteRef and collision metadata.
#[derive(Debug, Clone, PartialEq)] pub struct CompiledTile { pub id:TileId, pub sprite:SpriteRef, pub one_way:bool }
/// One compiled sparse Tile Map chunk.
#[derive(Debug, Clone, PartialEq)] pub struct CompiledTileChunk { pub layer_index:usize, pub coord:TileChunkCoord, pub cells:Vec<(i32,i32,TileId)> }
/// Compiled map retaining chunk granularity for independent render/physics consumers.
#[derive(Debug, Clone, PartialEq)] pub struct CompiledTileMap { pub chunk_size:u16, pub chunks:Vec<CompiledTileChunk> }
impl CompiledTileMap { pub fn chunk(&self, layer:usize, coord:TileChunkCoord)->Option<&CompiledTileChunk>{self.chunks.iter().find(|c|c.layer_index==layer&&c.coord==coord)} }

/// Compiles sparse authored chunks without deriving identity from ordering.
pub fn compile_tile_map(document:&TileMapDocument)->CompiledTileMap {
    let mut chunks=Vec::new();
    for (layer_index,layer) in document.layers.iter().enumerate(){for chunk in &layer.chunks{let mut cells=Vec::new();for(key,tile)in &chunk.cells{if let Some((x,y))=key.split_once(',').and_then(|(x,y)|Some((x.parse::<i32>().ok()?,y.parse::<i32>().ok()?))){cells.push((x,y,tile.clone()));}}cells.sort_by_key(|(x,y,t)|(*y,*x,t.as_str().to_owned()));chunks.push(CompiledTileChunk{layer_index,coord:chunk.coord,cells});}}
    chunks.sort_by_key(|c|(c.layer_index,c.coord.y,c.coord.x)); CompiledTileMap{chunk_size:document.chunk_size,chunks}
}

/// Validates that referenced stable identities resolve before runtime conversion.
pub fn validate_2d_asset_references(atlas:&SpriteAtlasDocument, tiles:&TileSetDocument)->Vec<String>{let mut errors=atlas.validate();for tile in &tiles.tiles{if atlas.region(&tile.sprite.sprite).is_none(){errors.push(format!("tile {} references missing sprite {}",tile.id.as_str(),tile.sprite.sprite.as_str()));}}errors}

#[cfg(test)]mod tests{use super::*;#[test]fn empty_map_compiles_without_phantom_chunks(){let map=TileMapDocument{schema_version:1,tile_set:engine_authoring::AssetId::generate(),chunk_size:32,layers:vec![]};assert!(compile_tile_map(&map).chunks.is_empty());}}
