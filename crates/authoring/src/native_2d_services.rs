//! GUI-free Native 2D authoring services shared by Editor/CLI/MCP (ADR 0127).

use crate::native_2d::{SpriteAtlasDocument, SpriteId, TileCell, TileChunkCoord, TileId, TileLayerId, TileMapDocument, TileMapStroke};

/// Atomic sprite-atlas mutations. Callers persist the returned document through the normal typed-document transaction path.
#[derive(Debug, Clone)]
pub struct SpriteAtlasAuthoringService { document: SpriteAtlasDocument, undo: Vec<SpriteAtlasDocument> }
impl SpriteAtlasAuthoringService {
    /// Starts from one validated source document.
    pub fn new(document: SpriteAtlasDocument) -> Self { Self { document, undo: Vec::new() } }
    /// Current immutable authoring snapshot.
    pub fn document(&self) -> &SpriteAtlasDocument { &self.document }
    /// Renames a region without changing stable SpriteId.
    pub fn rename(&mut self, id: &SpriteId, name: String) -> Result<(), &'static str> { let Some(index)=self.document.regions.iter().position(|r| &r.id==id) else{return Err("sprite not found")}; let before=self.document.clone(); self.document.regions[index].name=name; self.undo.push(before); Ok(()) }
    /// Changes a normalized pivot as one semantic transaction.
    pub fn set_pivot(&mut self, id: &SpriteId, pivot: [f32;2]) -> Result<(), &'static str> { if !(0.0..=1.0).contains(&pivot[0]) || !(0.0..=1.0).contains(&pivot[1]) { return Err("pivot must be normalized"); } let Some(index)=self.document.regions.iter().position(|r| &r.id==id) else{return Err("sprite not found")}; let before=self.document.clone(); self.document.regions[index].pivot=pivot; self.undo.push(before); Ok(()) }
    /// Reverts the latest semantic mutation.
    pub fn undo(&mut self) -> bool { let Some(previous)=self.undo.pop() else{return false}; self.document=previous; true }
}

/// Result of committing one tile painting gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileMapGestureCommit { /// Chunks invalidated by this gesture only.
    pub affected_chunks: Vec<TileChunkCoord>, /// Number of semantic undo entries produced; always one.
    pub undo_entries: u32 }

/// Transactional Tile Map service. One pointer gesture maps to exactly one undo entry.
#[derive(Debug, Clone)]
pub struct TileMapAuthoringService { document: TileMapDocument, active: Option<TileMapStroke>, affected: Vec<TileChunkCoord>, undo: Vec<TileMapDocument> }
impl TileMapAuthoringService {
    /// Creates a service for one sparse map document.
    pub fn new(document: TileMapDocument) -> Self { Self { document, active: None, affected: Vec::new(), undo: Vec::new() } }
    /// Current committed document.
    pub fn document(&self) -> &TileMapDocument { &self.document }
    /// Begins one pointer gesture. Nested gestures are rejected.
    pub fn begin_gesture(&mut self) -> Result<(), &'static str> { if self.active.is_some(){return Err("tile gesture already active")}; self.active=Some(TileMapStroke::begin(&self.document)); self.affected.clear(); Ok(()) }
    /// Paints/erases one cell in the transient gesture preview.
    pub fn paint(&mut self, layer:&TileLayerId, cell:TileCell, tile:Option<TileId>) -> Result<TileChunkCoord,&'static str> { let Some(stroke)=self.active.as_mut() else{return Err("tile gesture not active")}; let chunk=stroke.paint(layer,cell,tile)?; if !self.affected.contains(&chunk){self.affected.push(chunk);} Ok(chunk) }
    /// Returns transient preview content while a gesture is active.
    pub fn preview(&self) -> &TileMapDocument { self.active.as_ref().map_or(&self.document, TileMapStroke::document) }
    /// Cancels a gesture and restores the exact pre-gesture snapshot.
    pub fn cancel_gesture(&mut self) -> Result<(), &'static str> { let Some(stroke)=self.active.take() else{return Err("tile gesture not active")}; self.document=stroke.cancel(); self.affected.clear(); Ok(()) }
    /// Commits the whole gesture as exactly one semantic undo entry.
    pub fn commit_gesture(&mut self) -> Result<TileMapGestureCommit,&'static str> { let Some(stroke)=self.active.take() else{return Err("tile gesture not active")}; let before=self.document.clone(); self.document=stroke.commit(); self.undo.push(before); self.affected.sort_by_key(|c|(c.y,c.x)); let affected_chunks=std::mem::take(&mut self.affected); Ok(TileMapGestureCommit{affected_chunks,undo_entries:1}) }
    /// Reverts the latest completed gesture.
    pub fn undo(&mut self)->bool{if self.active.is_some(){return false;}let Some(previous)=self.undo.pop()else{return false};self.document=previous;true}
}

#[cfg(test)] mod tests { use super::*; use crate::native_2d::*; use crate::AssetId;
    fn map()->TileMapDocument{let layer=TileLayerId::generate();TileMapDocument{schema_version:TILE_MAP_SCHEMA_VERSION,tile_set:AssetId::generate(),chunk_size:32,layers:vec![TileMapLayer{id:layer,name:"World".into(),enabled:true,locked:false,sorting_layer:SortingLayerId::generate(),order_in_layer:0,chunks:vec![]}]}}
    #[test] fn one_stroke_is_one_undo_and_cancel_is_exact(){let original=map();let layer=original.layers[0].id.clone();let mut service=TileMapAuthoringService::new(original.clone());service.begin_gesture().unwrap();service.paint(&layer,TileCell{x:2,y:3},Some(TileId::generate())).unwrap();service.cancel_gesture().unwrap();assert_eq!(service.document(),&original);service.begin_gesture().unwrap();service.paint(&layer,TileCell{x:33,y:3},Some(TileId::generate())).unwrap();let commit=service.commit_gesture().unwrap();assert_eq!(commit.undo_entries,1);assert_eq!(commit.affected_chunks,vec![TileChunkCoord{x:1,y:0}]);assert!(service.undo());assert_eq!(service.document(),&original);}
}
