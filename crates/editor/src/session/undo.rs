//! Snapshot undo/redo history and saved-state tracking.
//!
//! Graph and UI documents undo through serialized JSON snapshots held by this
//! module (ADR 0018). Scene documents delegate to the authoring session that
//! owns their transaction history, so both paths share one editor-facing API.

use super::{EditorGraphDomain, EditorSession};
use crate::document::CurrentDocument;
use engine_authoring::{AuthoringSession, Graph, GraphView, UiDocument};

/// Maximum number of retained undo snapshots (ADR 0018).
const UNDO_LIMIT: usize = 100;

/// One snapshot entry in the session undo/redo history.
///
/// Both documents are serialized to JSON. See ADR 0018.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum UndoEntry {
    Graph {
        graph_json: String,
        graph_view_json: Option<String>,
    },
    Ui(String),
}

/// Document state last persisted to disk.
///
/// Scene documents use their authoring revision so dirty checks remain O(1).
/// Graph and UI documents retain the existing serialized snapshot comparison.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum CleanSnapshot {
    Scene(u64),
    Document(UndoEntry),
}

/// Session-local snapshot-based undo/redo stack.
///
/// History is in-memory only and does not survive process restarts.
/// See ADR 0018.
pub(super) struct UndoStack {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl UndoStack {
    pub(super) fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Pushes `entry` as the most recent undo point and clears redo history.
    pub(super) fn push(&mut self, entry: UndoEntry) {
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(entry);
        self.redo.clear();
    }

    /// Pops the most recent undo entry, pushing `current` onto redo.
    fn undo(&mut self, current: UndoEntry) -> Option<UndoEntry> {
        let entry = self.undo.pop()?;
        self.redo.push(current);
        Some(entry)
    }

    /// Pops the most recent redo entry, pushing `current` onto undo.
    fn redo(&mut self, current: UndoEntry) -> Option<UndoEntry> {
        let entry = self.redo.pop()?;
        self.undo.push(current);
        Some(entry)
    }

    fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

impl EditorSession {
    /// Returns `true` when the undo stack is non-empty.
    pub fn can_undo(&self) -> bool {
        if self.is_scene_document() {
            return self
                .scene_session
                .as_ref()
                .is_some_and(AuthoringSession::can_undo);
        }
        self.undo_stack.can_undo()
    }

    /// Returns `true` when the redo stack is non-empty.
    pub fn can_redo(&self) -> bool {
        if self.is_scene_document() {
            return self
                .scene_session
                .as_ref()
                .is_some_and(AuthoringSession::can_redo);
        }
        self.undo_stack.can_redo()
    }

    /// Restores the most recent undo snapshot.
    ///
    /// Returns `true` when an undo entry was available and applied.
    pub fn undo(&mut self) -> bool {
        if self.is_scene_document() {
            let Some(scene_session) = &mut self.scene_session else {
                return false;
            };
            if !scene_session.undo() {
                return false;
            }
            self.sync_scene_document_from_session();
            self.mark_dirty();
            return true;
        }

        let Some(current) = self.snapshot() else {
            return false;
        };
        let Some(entry) = self.undo_stack.undo(current) else {
            return false;
        };
        self.restore(entry);
        self.mark_dirty();
        true
    }

    /// Re-applies the most recently undone operation.
    ///
    /// Returns `true` when a redo entry was available and applied.
    pub fn redo(&mut self) -> bool {
        if self.is_scene_document() {
            let Some(scene_session) = &mut self.scene_session else {
                return false;
            };
            if !scene_session.redo() {
                return false;
            }
            self.sync_scene_document_from_session();
            self.mark_dirty();
            return true;
        }

        let Some(current) = self.snapshot() else {
            return false;
        };
        let Some(entry) = self.undo_stack.redo(current) else {
            return false;
        };
        self.restore(entry);
        self.mark_dirty();
        true
    }

    /// Discards all undo and redo history.
    ///
    /// Call this after loading a new document so that history from the
    /// previous session cannot be applied to different graph content.
    pub fn clear_undo_redo(&mut self) {
        self.undo_stack.clear();
    }

    pub(super) fn mark_dirty(&mut self) {
        if matches!(self.current_document, CurrentDocument::None) {
            self.untitled_dirty = true;
            return;
        }
        if self.document_snapshot() == self.clean_snapshot {
            self.current_document.mark_clean();
            self.untitled_dirty = false;
        } else {
            self.current_document.mark_dirty();
        }
    }

    pub(super) fn mark_clean(&mut self) {
        self.record_clean_snapshot();
    }

    pub(super) fn record_clean_snapshot(&mut self) {
        self.clean_snapshot = self.document_snapshot();
        self.current_document.mark_clean();
        self.untitled_dirty = false;
    }

    fn document_snapshot(&self) -> Option<CleanSnapshot> {
        match &self.current_document {
            CurrentDocument::None => None,
            CurrentDocument::Scene { scene, .. } => Some(CleanSnapshot::Scene(scene.revision())),
            CurrentDocument::Graph { .. } | CurrentDocument::Ui { .. } => {
                self.snapshot().map(CleanSnapshot::Document)
            }
        }
    }

    /// Serializes the current graph and view as one undo snapshot.
    ///
    /// Returns `None` if serialization fails, which is unexpected for
    /// structurally valid documents.
    pub(super) fn snapshot(&self) -> Option<UndoEntry> {
        match &self.current_document {
            CurrentDocument::Ui { document, .. } => {
                serde_json::to_string(document).ok().map(UndoEntry::Ui)
            }
            CurrentDocument::None
            | CurrentDocument::Scene { .. }
            | CurrentDocument::Graph { .. } => {
                let graph_json = serde_json::to_string(&self.graph).ok()?;
                let graph_view_json = self
                    .graph_view
                    .as_ref()
                    .and_then(|view| serde_json::to_string(view).ok());
                Some(UndoEntry::Graph {
                    graph_json,
                    graph_view_json,
                })
            }
        }
    }

    /// Replaces the current graph and view from a snapshot entry and
    /// re-runs domain validation to refresh diagnostics.
    fn restore(&mut self, entry: UndoEntry) {
        match entry {
            UndoEntry::Graph {
                graph_json,
                graph_view_json,
            } => {
                if let Ok(graph) = serde_json::from_str::<Graph>(&graph_json) {
                    self.graph = graph;
                }
                self.domain = EditorGraphDomain::for_graph(&self.graph);
                self.graph_view = graph_view_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<GraphView>(json).ok());
                self.diagnostics = self.domain.validate_domain(&self.graph);
            }
            UndoEntry::Ui(json) => {
                if let Ok(document) = serde_json::from_str::<UiDocument>(&json) {
                    self.diagnostics = document.validate();
                    if let CurrentDocument::Ui {
                        document: current, ..
                    } = &mut self.current_document
                    {
                        *current = document;
                    }
                }
            }
        }
    }

    /// Captures the current state as an undo checkpoint.
    ///
    /// Called at the start of every user-visible compound operation before
    /// any mutation occurs. Internal primitives (`apply_graph_command`,
    /// `apply_graph_view_command`, `set_node_layout`, `select_node`) do not
    /// call this method directly.
    pub(super) fn push_undo_checkpoint(&mut self) {
        if let Some(entry) = self.snapshot() {
            self.undo_stack.push(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::BehaviorNodeInsertKind;
    use engine_authoring::Vec2;

    #[test]
    fn undo_after_add_node_removes_it() {
        let mut session = EditorSession::empty_behavior_tree();
        let node = session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "a", None)
            .expect("add should succeed");
        assert!(session.graph().nodes.contains_key(&node));

        assert!(session.undo(), "undo must return true");
        assert!(
            !session.graph().nodes.contains_key(&node),
            "node must be gone after undo"
        );
    }

    #[test]
    fn redo_after_undo_restores_node() {
        let mut session = EditorSession::empty_behavior_tree();
        let node = session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "a", None)
            .expect("add should succeed");
        session.undo();

        assert!(session.redo(), "redo must return true");
        assert!(
            session.graph().nodes.contains_key(&node),
            "node must be restored after redo"
        );
    }

    #[test]
    fn undo_on_empty_stack_returns_false() {
        let mut session = EditorSession::empty_behavior_tree();
        assert!(!session.undo());
        assert!(!session.can_undo());
    }

    #[test]
    fn new_operation_clears_redo_stack() {
        let mut session = EditorSession::empty_behavior_tree();
        session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "a", None)
            .unwrap();
        session.undo();
        assert!(session.can_redo(), "redo must be available after undo");

        session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "b", None)
            .unwrap();
        assert!(
            !session.can_redo(),
            "redo must be cleared after a new operation"
        );
    }

    #[test]
    fn undo_move_node_restores_position() {
        let mut session = EditorSession::empty_behavior_tree();
        let node = session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "a",
                Some(Vec2::new(0.0, 0.0)),
            )
            .expect("add should succeed");

        session
            .move_node(node.clone(), Vec2::new(99.0, 77.0))
            .expect("move must apply");
        session.undo();

        let position = session.graph_view().expect("view must exist").nodes[&node].position;
        assert_eq!(
            position,
            Vec2::new(0.0, 0.0),
            "position must be restored to pre-move value after undo"
        );
    }

    // ── File persistence tests ─────────────────────────────────────────────

    #[test]
    fn undoing_scene_back_to_disk_baseline_clears_dirty_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("baseline.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");

        session
            .create_scene_entity("temporary")
            .expect("edit scene");
        assert!(session.is_dirty());
        assert!(session.undo());
        assert!(!session.is_dirty(), "saved scene snapshot must be clean");
        assert!(session.redo());
        assert!(session.is_dirty(), "redo must move away from baseline");
    }

    #[test]
    fn undoing_graph_back_to_disk_baseline_clears_dirty_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("baseline.graph.json");
        let mut session = EditorSession::empty_behavior_tree();
        session.save_as(path).expect("save graph baseline");

        session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "temporary", None)
            .expect("edit graph");
        assert!(session.is_dirty());
        assert!(session.undo());
        assert!(!session.is_dirty(), "saved graph snapshot must be clean");
        assert!(session.redo());
        assert!(session.is_dirty(), "redo must move away from baseline");
    }
}
