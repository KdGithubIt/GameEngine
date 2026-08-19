//! GUI-free Native 2D authoring services shared by Editor, CLI, and MCP.

use crate::native_2d_assets::{
    SpriteAtlasDocument, SpriteId, TileCell, TileChunk, TileChunkCoord, TileId, TileLayerId,
    TileMapDocument,
};
use std::collections::{BTreeSet, VecDeque};
use std::fmt;

/// Stable identification of one Tile Map chunk invalidated by an edit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TileMapChunkKey {
    /// Stable Tile Map layer identity.
    pub layer: TileLayerId,
    /// Spatial sparse-chunk coordinate.
    pub chunk: TileChunkCoord,
}

/// Structured failure from shared Native 2D authoring operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Native2dAuthoringError {
    /// The requested stable SpriteId does not exist.
    SpriteNotFound(SpriteId),
    /// The requested stable TileLayerId does not exist.
    LayerNotFound(TileLayerId),
    /// The selected Tile Map layer is locked.
    LayerLocked(TileLayerId),
    /// A pointer gesture is already active.
    GestureAlreadyActive,
    /// A gesture-scoped operation was requested without an active gesture.
    GestureNotActive,
    /// A bounded operation exceeded its explicit work budget.
    WorkBudgetExceeded {
        /// Maximum number of cells the caller allowed.
        limit: usize,
    },
    /// A normalized sprite pivot was outside `[0, 1]` or non-finite.
    InvalidPivot,
}

impl fmt::Display for Native2dAuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpriteNotFound(id) => {
                write!(formatter, "SpriteId `{}` was not found", id.as_str())
            }
            Self::LayerNotFound(id) => {
                write!(formatter, "TileLayerId `{}` was not found", id.as_str())
            }
            Self::LayerLocked(id) => write!(formatter, "TileLayerId `{}` is locked", id.as_str()),
            Self::GestureAlreadyActive => {
                formatter.write_str("a Tile Map gesture is already active")
            }
            Self::GestureNotActive => formatter.write_str("no Tile Map gesture is active"),
            Self::WorkBudgetExceeded { limit } => write!(
                formatter,
                "Tile Map operation exceeded its {limit}-cell work budget"
            ),
            Self::InvalidPivot => formatter.write_str("sprite pivot must be finite and normalized"),
        }
    }
}

impl std::error::Error for Native2dAuthoringError {}

/// Atomic Sprite Atlas mutations independent of presentation or transport.
#[derive(Debug, Clone)]
pub struct SpriteAtlasAuthoringService {
    document: SpriteAtlasDocument,
    undo: Vec<SpriteAtlasDocument>,
}

impl SpriteAtlasAuthoringService {
    /// Creates the service from one typed Sprite Atlas document.
    pub fn new(document: SpriteAtlasDocument) -> Self {
        Self {
            document,
            undo: Vec::new(),
        }
    }

    /// Returns the current committed document.
    pub fn document(&self) -> &SpriteAtlasDocument {
        &self.document
    }

    /// Renames a region without changing its stable SpriteId.
    pub fn rename(&mut self, id: &SpriteId, name: String) -> Result<(), Native2dAuthoringError> {
        let Some(index) = self
            .document
            .regions
            .iter()
            .position(|region| &region.id == id)
        else {
            return Err(Native2dAuthoringError::SpriteNotFound(id.clone()));
        };
        let before = self.document.clone();
        self.document.regions[index].name = name;
        self.undo.push(before);
        Ok(())
    }

    /// Changes one normalized pivot as a single semantic mutation.
    pub fn set_pivot(
        &mut self,
        id: &SpriteId,
        pivot: [f32; 2],
    ) -> Result<(), Native2dAuthoringError> {
        if pivot
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
        {
            return Err(Native2dAuthoringError::InvalidPivot);
        }
        let Some(index) = self
            .document
            .regions
            .iter()
            .position(|region| &region.id == id)
        else {
            return Err(Native2dAuthoringError::SpriteNotFound(id.clone()));
        };
        let before = self.document.clone();
        self.document.regions[index].pivot = pivot;
        self.undo.push(before);
        Ok(())
    }

    /// Reverts the latest committed semantic mutation.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.document = previous;
        true
    }
}

/// Inclusive finite Tile Map rectangle used by rectangle/fill/stamp tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRect {
    /// Inclusive minimum world cell.
    pub min: TileCell,
    /// Inclusive maximum world cell.
    pub max: TileCell,
}

impl TileRect {
    /// Creates a normalized rectangle from two arbitrary corners.
    pub fn from_corners(a: TileCell, b: TileCell) -> Self {
        Self {
            min: TileCell {
                x: a.x.min(b.x),
                y: a.y.min(b.y),
            },
            max: TileCell {
                x: a.x.max(b.x),
                y: a.y.max(b.y),
            },
        }
    }

    fn cells(self) -> impl Iterator<Item = TileCell> {
        (self.min.y..=self.max.y)
            .flat_map(move |y| (self.min.x..=self.max.x).map(move |x| TileCell { x, y }))
    }
}

/// Clipboard-like sparse Tile Map stamp using offsets from its origin.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TileStamp {
    /// Sparse `(offset, tile)` entries in deterministic order.
    pub cells: Vec<(TileCell, TileId)>,
}

/// Commit result for exactly one Tile Map pointer gesture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileMapGestureCommit {
    /// Layer/chunk pairs invalidated by the gesture only.
    pub affected_chunks: Vec<TileMapChunkKey>,
    /// Semantic undo entries produced by the gesture; always one.
    pub undo_entries: u32,
}

#[derive(Debug, Clone)]
struct TileMapGesture {
    before: TileMapDocument,
    working: TileMapDocument,
    affected: BTreeSet<TileMapChunkKey>,
}

impl TileMapGesture {
    fn new(document: &TileMapDocument) -> Self {
        Self {
            before: document.clone(),
            working: document.clone(),
            affected: BTreeSet::new(),
        }
    }
}

/// Transactional sparse Tile Map service. One pointer gesture is one undo entry.
#[derive(Debug, Clone)]
pub struct TileMapAuthoringService {
    document: TileMapDocument,
    active: Option<TileMapGesture>,
    undo: Vec<TileMapDocument>,
}

impl TileMapAuthoringService {
    /// Creates a service from one committed sparse Tile Map document.
    pub fn new(document: TileMapDocument) -> Self {
        Self {
            document,
            active: None,
            undo: Vec::new(),
        }
    }

    /// Returns the current committed document.
    pub fn document(&self) -> &TileMapDocument {
        &self.document
    }

    /// Returns transient preview content while a gesture is active.
    pub fn preview(&self) -> &TileMapDocument {
        self.active
            .as_ref()
            .map_or(&self.document, |gesture| &gesture.working)
    }

    /// Begins one pointer gesture. Nested gestures are rejected.
    pub fn begin_gesture(&mut self) -> Result<(), Native2dAuthoringError> {
        if self.active.is_some() {
            return Err(Native2dAuthoringError::GestureAlreadyActive);
        }
        self.active = Some(TileMapGesture::new(&self.document));
        Ok(())
    }

    /// Paints or erases one world cell in the transient gesture.
    pub fn paint(
        &mut self,
        layer: &TileLayerId,
        cell: TileCell,
        tile: Option<TileId>,
    ) -> Result<TileMapChunkKey, Native2dAuthoringError> {
        let gesture = self
            .active
            .as_mut()
            .ok_or(Native2dAuthoringError::GestureNotActive)?;
        let (coord, local) = gesture.working.split_cell(cell);
        let Some(layer_document) = gesture
            .working
            .layers
            .iter_mut()
            .find(|candidate| &candidate.id == layer)
        else {
            return Err(Native2dAuthoringError::LayerNotFound(layer.clone()));
        };
        if layer_document.locked {
            return Err(Native2dAuthoringError::LayerLocked(layer.clone()));
        }
        let chunk_index = layer_document
            .chunks
            .iter()
            .position(|chunk| chunk.coord == coord);
        if let Some(index) = chunk_index {
            layer_document.chunks[index].set(local, tile);
            if layer_document.chunks[index].cells.is_empty() {
                layer_document.chunks.remove(index);
            }
        } else if let Some(tile) = tile {
            let mut chunk = TileChunk {
                coord,
                cells: Vec::new(),
            };
            chunk.set(local, Some(tile));
            layer_document.chunks.push(chunk);
            layer_document
                .chunks
                .sort_by_key(|chunk| (chunk.coord.y, chunk.coord.x));
        }
        let key = TileMapChunkKey {
            layer: layer.clone(),
            chunk: coord,
        };
        gesture.affected.insert(key.clone());
        Ok(key)
    }

    /// Paints or erases an inclusive rectangle within one gesture.
    pub fn rectangle(
        &mut self,
        layer: &TileLayerId,
        rect: TileRect,
        tile: Option<TileId>,
        max_cells: usize,
    ) -> Result<(), Native2dAuthoringError> {
        let cells = rect.cells().collect::<Vec<_>>();
        if cells.len() > max_cells {
            return Err(Native2dAuthoringError::WorkBudgetExceeded { limit: max_cells });
        }
        for cell in cells {
            self.paint(layer, cell, tile.clone())?;
        }
        Ok(())
    }

    /// Paints a Bresenham line within one gesture and explicit work budget.
    pub fn line(
        &mut self,
        layer: &TileLayerId,
        start: TileCell,
        end: TileCell,
        tile: Option<TileId>,
        max_cells: usize,
    ) -> Result<(), Native2dAuthoringError> {
        let mut x = start.x;
        let mut y = start.y;
        let dx = (end.x - start.x).abs();
        let sx = if start.x < end.x { 1 } else { -1 };
        let dy = -(end.y - start.y).abs();
        let sy = if start.y < end.y { 1 } else { -1 };
        let mut error = dx + dy;
        let mut cells = Vec::new();
        loop {
            cells.push(TileCell { x, y });
            if cells.len() > max_cells {
                return Err(Native2dAuthoringError::WorkBudgetExceeded { limit: max_cells });
            }
            if x == end.x && y == end.y {
                break;
            }
            let twice = 2 * error;
            if twice >= dy {
                error += dy;
                x += sx;
            }
            if twice <= dx {
                error += dx;
                y += sy;
            }
        }
        for cell in cells {
            self.paint(layer, cell, tile.clone())?;
        }
        Ok(())
    }

    /// Flood-fills a finite rectangle, never searching outside the caller's bounds.
    pub fn fill_bounded(
        &mut self,
        layer: &TileLayerId,
        start: TileCell,
        bounds: TileRect,
        tile: Option<TileId>,
        max_cells: usize,
    ) -> Result<(), Native2dAuthoringError> {
        if start.x < bounds.min.x
            || start.x > bounds.max.x
            || start.y < bounds.min.y
            || start.y > bounds.max.y
        {
            return Ok(());
        }
        let target = self.preview().tile_at(layer, start).cloned();
        if target == tile {
            return Ok(());
        }
        let before = self.active.clone();
        let mut queue = VecDeque::from([start]);
        let mut visited = BTreeSet::new();
        while let Some(cell) = queue.pop_front() {
            if cell.x < bounds.min.x
                || cell.x > bounds.max.x
                || cell.y < bounds.min.y
                || cell.y > bounds.max.y
                || !visited.insert(cell)
            {
                continue;
            }
            if self.preview().tile_at(layer, cell).cloned() != target {
                continue;
            }
            if visited.len() > max_cells {
                self.active = before;
                return Err(Native2dAuthoringError::WorkBudgetExceeded { limit: max_cells });
            }
            self.paint(layer, cell, tile.clone())?;
            queue.push_back(TileCell {
                x: cell.x + 1,
                y: cell.y,
            });
            queue.push_back(TileCell {
                x: cell.x - 1,
                y: cell.y,
            });
            queue.push_back(TileCell {
                x: cell.x,
                y: cell.y + 1,
            });
            queue.push_back(TileCell {
                x: cell.x,
                y: cell.y - 1,
            });
        }
        Ok(())
    }

    /// Eyedrops the stable TileId at one world cell from the transient preview.
    pub fn eyedropper(&self, layer: &TileLayerId, cell: TileCell) -> Option<TileId> {
        self.preview().tile_at(layer, cell).cloned()
    }

    /// Copies a finite rectangle into a sparse stamp whose origin is the rectangle minimum.
    pub fn copy_stamp(&self, layer: &TileLayerId, rect: TileRect) -> TileStamp {
        let mut cells = rect
            .cells()
            .filter_map(|cell| {
                self.preview().tile_at(layer, cell).cloned().map(|tile| {
                    (
                        TileCell {
                            x: cell.x - rect.min.x,
                            y: cell.y - rect.min.y,
                        },
                        tile,
                    )
                })
            })
            .collect::<Vec<_>>();
        cells.sort_by_key(|(cell, tile)| (cell.y, cell.x, tile.as_str().to_owned()));
        TileStamp { cells }
    }

    /// Pastes one sparse stamp as part of the active gesture.
    pub fn paste_stamp(
        &mut self,
        layer: &TileLayerId,
        origin: TileCell,
        stamp: &TileStamp,
        max_cells: usize,
    ) -> Result<(), Native2dAuthoringError> {
        if stamp.cells.len() > max_cells {
            return Err(Native2dAuthoringError::WorkBudgetExceeded { limit: max_cells });
        }
        for (offset, tile) in &stamp.cells {
            self.paint(
                layer,
                TileCell {
                    x: origin.x + offset.x,
                    y: origin.y + offset.y,
                },
                Some(tile.clone()),
            )?;
        }
        Ok(())
    }

    /// Cancels a gesture and restores the exact pre-gesture snapshot.
    pub fn cancel_gesture(&mut self) -> Result<(), Native2dAuthoringError> {
        let gesture = self
            .active
            .take()
            .ok_or(Native2dAuthoringError::GestureNotActive)?;
        self.document = gesture.before;
        Ok(())
    }

    /// Commits the complete gesture as exactly one semantic undo entry.
    pub fn commit_gesture(&mut self) -> Result<TileMapGestureCommit, Native2dAuthoringError> {
        let gesture = self
            .active
            .take()
            .ok_or(Native2dAuthoringError::GestureNotActive)?;
        self.undo.push(gesture.before);
        self.document = gesture.working;
        Ok(TileMapGestureCommit {
            affected_chunks: gesture.affected.into_iter().collect(),
            undo_entries: 1,
        })
    }

    /// Reverts the latest completed gesture. Active gestures are never crossed.
    pub fn undo(&mut self) -> bool {
        if self.active.is_some() {
            return false;
        }
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.document = previous;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_2d_assets::{TILE_MAP_SCHEMA_VERSION, TileMapLayer};
    use crate::{AssetId, SortingLayerId};

    fn map() -> TileMapDocument {
        TileMapDocument {
            schema_version: TILE_MAP_SCHEMA_VERSION,
            tile_set: AssetId::generate(),
            chunk_size: 32,
            layers: vec![TileMapLayer {
                id: TileLayerId::generate(),
                name: "World".to_owned(),
                enabled: true,
                locked: false,
                sorting_layer: SortingLayerId::generate(),
                order_in_layer: 0,
                chunks: Vec::new(),
            }],
        }
    }

    #[test]
    fn one_pointer_stroke_is_one_undo_entry_and_cancel_is_exact() {
        let original = map();
        let layer = original.layers[0].id.clone();
        let mut service = TileMapAuthoringService::new(original.clone());
        service.begin_gesture().unwrap();
        service
            .paint(&layer, TileCell { x: 2, y: 3 }, Some(TileId::generate()))
            .unwrap();
        service.cancel_gesture().unwrap();
        assert_eq!(service.document(), &original);

        service.begin_gesture().unwrap();
        service
            .paint(&layer, TileCell { x: 33, y: 3 }, Some(TileId::generate()))
            .unwrap();
        let commit = service.commit_gesture().unwrap();
        assert_eq!(commit.undo_entries, 1);
        assert_eq!(
            commit.affected_chunks[0].chunk,
            TileChunkCoord { x: 1, y: 0 }
        );
        assert!(service.undo());
        assert_eq!(service.document(), &original);
    }

    #[test]
    fn bounded_fill_rolls_back_transient_changes_on_budget_failure() {
        let original = map();
        let layer = original.layers[0].id.clone();
        let mut service = TileMapAuthoringService::new(original);
        service.begin_gesture().unwrap();
        let before = service.preview().clone();
        assert!(matches!(
            service.fill_bounded(
                &layer,
                TileCell { x: 0, y: 0 },
                TileRect::from_corners(TileCell { x: 0, y: 0 }, TileCell { x: 4, y: 4 }),
                Some(TileId::generate()),
                4,
            ),
            Err(Native2dAuthoringError::WorkBudgetExceeded { .. })
        ));
        assert_eq!(service.preview(), &before);
    }
}
