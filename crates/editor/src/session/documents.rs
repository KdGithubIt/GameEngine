//! Opening scene, graph, and declarative UI documents.
//!
//! Each open method has a `_discarding_changes` counterpart. The plain form
//! refuses to replace a dirty document so the editor shell can present a
//! save/discard/cancel decision before data is lost.

use super::errors::EditorSessionError;
use super::persistence::remove_recovery_file;
use super::{EditorGraphDomain, EditorSession};
use crate::document::{
    open_graph_from_path, open_scene_from_path, open_ui_from_path, CurrentDocument,
    OpenDocumentError,
};
use engine_authoring::{AuthoringSession, UiDocument, UiDocumentCommand, UiDocumentTransaction};
use std::path::{Path, PathBuf};

impl EditorSession {
    /// Closes the current document after the caller has resolved data loss.
    pub fn close_document_discarding_changes(&mut self) {
        // Discarding is an explicit decision to abandon both the in-memory
        // edit and its crash-recovery copy. Leaving the recovery sidecar would
        // restore the discarded edit and mark the document dirty next time.
        if let Some(path) = self.current_document_path().map(Path::to_path_buf) {
            remove_recovery_file(&path);
        }
        self.current_document = CurrentDocument::None;
        self.scene_session = None;
        self.undo_stack.clear();
        self.untitled_dirty = false;
        self.clean_snapshot = None;
        self.bump_document_revision();
    }

    /// Opens a `*.scene.json` file and replaces the current document.
    ///
    /// # Errors
    ///
    /// - [`OpenDocumentError::UnsavedChanges`] when the current document has
    ///   unsaved changes. Save or discard changes before calling this method.
    /// - Any [`OpenDocumentError`] variant returned by the underlying I/O and
    ///   parsing functions.
    pub fn open_scene(&mut self, path: PathBuf) -> Result<(), OpenDocumentError> {
        if self.is_dirty() {
            return Err(OpenDocumentError::UnsavedChanges);
        }
        self.open_scene_discarding_changes(path)
    }

    /// Opens a `*.scene.json` file, replacing the current document even when
    /// it has unsaved changes.
    ///
    /// Unsaved changes in the current document are lost. Callers must confirm
    /// the discard with the user first; the editor shell shows a
    /// save/discard/cancel dialog before calling this method.
    ///
    /// # Errors
    ///
    /// Any [`OpenDocumentError`] variant returned by the underlying I/O and
    /// parsing functions.
    pub fn open_scene_discarding_changes(
        &mut self,
        path: PathBuf,
    ) -> Result<(), OpenDocumentError> {
        let doc = open_scene_from_path(&path)?;
        if let CurrentDocument::Scene { scene, .. } = &doc {
            self.scene_session = Some(AuthoringSession::new(scene.clone()));
        }
        self.current_document = doc;
        self.undo_stack.clear();
        self.untitled_dirty = false;
        self.record_clean_snapshot();
        self.restore_newer_recovery();
        self.bump_document_revision();
        Ok(())
    }

    /// Opens a `*.graph.json` file and replaces the current document.
    ///
    /// A sibling `*.graph.view.json` is auto-loaded when present.  A missing
    /// view is not an error; the graph canvas will show no layout until the
    /// user applies auto-layout.
    ///
    /// Loading a new graph clears the undo/redo history.
    ///
    /// # Errors
    ///
    /// - [`OpenDocumentError::UnsavedChanges`] when the current document has
    ///   unsaved changes.
    /// - Any [`OpenDocumentError`] variant returned by the underlying I/O and
    ///   parsing functions.
    pub fn open_graph(&mut self, graph_path: PathBuf) -> Result<(), OpenDocumentError> {
        if self.is_dirty() {
            return Err(OpenDocumentError::UnsavedChanges);
        }
        self.open_graph_discarding_changes(graph_path)
    }

    /// Opens a `*.graph.json` file, replacing the current document even when
    /// it has unsaved changes.
    ///
    /// Unsaved changes in the current document are lost. Callers must confirm
    /// the discard with the user first; the editor shell shows a
    /// save/discard/cancel dialog before calling this method.
    ///
    /// # Errors
    ///
    /// Any [`OpenDocumentError`] variant returned by the underlying I/O and
    /// parsing functions.
    pub fn open_graph_discarding_changes(
        &mut self,
        graph_path: PathBuf,
    ) -> Result<(), OpenDocumentError> {
        let (doc, data) = open_graph_from_path(&graph_path)?;
        self.graph = data.graph;
        self.graph_view = data.view;
        self.domain = EditorGraphDomain::for_graph(&self.graph);
        self.diagnostics = self.domain.validate_domain(&self.graph);
        self.undo_stack.clear();
        self.scene_session = None;
        self.current_document = doc;
        self.untitled_dirty = false;
        self.record_clean_snapshot();
        self.restore_newer_recovery();
        // A view file or recovery snapshot written before the repair existed
        // can still name deleted nodes. Repairing on open makes the document
        // editable again without discarding the file by hand.
        self.prune_graph_view();
        self.bump_document_revision();
        Ok(())
    }

    /// Opens a validated `*.ui.json` document.
    pub fn open_ui(&mut self, path: PathBuf) -> Result<(), OpenDocumentError> {
        if self.is_dirty() {
            return Err(OpenDocumentError::UnsavedChanges);
        }
        self.open_ui_discarding_changes(path)
    }

    /// Opens a validated `*.ui.json` document after the caller has confirmed
    /// that replacing dirty editor state is safe.
    pub fn open_ui_discarding_changes(&mut self, path: PathBuf) -> Result<(), OpenDocumentError> {
        self.current_document = open_ui_from_path(&path)?;
        self.scene_session = None;
        self.undo_stack.clear();
        self.untitled_dirty = false;
        self.diagnostics = self
            .ui_document()
            .map(UiDocument::validate)
            .unwrap_or_default();
        self.record_clean_snapshot();
        self.restore_newer_recovery();
        self.bump_document_revision();
        Ok(())
    }

    /// Applies one UI authoring command as one undoable editor operation.
    pub fn apply_ui_command(
        &mut self,
        command: UiDocumentCommand,
    ) -> Result<(), EditorSessionError> {
        self.apply_ui_commands(std::iter::once(command))
    }

    /// Applies multiple UI commands as one validated undoable operation.
    pub fn apply_ui_commands(
        &mut self,
        commands: impl IntoIterator<Item = UiDocumentCommand>,
    ) -> Result<(), EditorSessionError> {
        let Some(document) = self.ui_document().cloned() else {
            return Err(EditorSessionError::NoUiDocument);
        };
        let checkpoint = self.snapshot();
        let mut transaction = UiDocumentTransaction::begin(&document);
        let mut diagnostics = Vec::new();
        for command in commands {
            diagnostics = transaction
                .apply(command)
                .map_err(EditorSessionError::UiEdit)?
                .diagnostics;
        }
        let document = transaction
            .commit()
            .map_err(|error| EditorSessionError::UiValidation {
                message: error.to_string(),
            })?;
        if let Some(checkpoint) = checkpoint {
            self.undo_stack.push(checkpoint);
        }
        if let CurrentDocument::Ui {
            document: current, ..
        } = &mut self.current_document
        {
            *current = document;
        }
        self.diagnostics = diagnostics;
        self.mark_dirty();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::derive_view_path;
    use crate::session::{AnimationNodeInsertKind, BehaviorNodeInsertKind};
    use engine_authoring::{Graph, Vec2};

    fn set_dirty_graph_document(session: &mut EditorSession) {
        session.current_document = CurrentDocument::Graph {
            graph_path: PathBuf::from("dirty.graph.json"),
            view_path: Some(PathBuf::from("dirty.graph.view.json")),
            is_dirty: false,
        };
        session.current_document.mark_dirty();
    }


    #[test]
    fn opening_a_graph_repairs_a_view_that_names_a_deleted_node() {
        let mut session = EditorSession::empty_animation_graph();
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(220.0, 0.0)))
            .expect("State should be added");
        let dir = tempfile::tempdir().unwrap();
        let graph_path = dir.path().join("stale.anim.graph.json");
        session
            .save_as(graph_path.clone())
            .expect("save must succeed");
        let view_path = derive_view_path(&graph_path).expect("graph path must derive a view path");

        // Rewrite only the semantic file, leaving the view naming the State.
        let mut graph: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&graph_path).unwrap())
                .expect("saved graph must parse");
        graph["nodes"]
            .as_object_mut()
            .expect("graph must hold a node map")
            .remove(state.as_str())
            .expect("test fixture must remove a stored node");
        std::fs::write(
            &graph_path,
            serde_json::to_string_pretty(&graph).expect("graph must serialize"),
        )
        .unwrap();
        assert!(
            std::fs::read_to_string(&view_path)
                .unwrap()
                .contains(state.as_str()),
            "test fixture must keep the stale layout in the view file"
        );

        let mut reopened = EditorSession::empty_behavior_tree();
        reopened
            .open_graph(graph_path)
            .expect("stale document must open");

        let view = reopened.graph_view().expect("graph view must exist");
        assert!(
            !view.nodes.contains_key(&state),
            "stale layout survived the open"
        );
        let survivor = reopened
            .graph()
            .nodes
            .keys()
            .next()
            .expect("Entry must survive")
            .clone();
        reopened
            .select_node(Some(survivor))
            .expect("selection must commit in a reopened document");
    }

    #[test]
    fn open_scene_loads_scene_and_sets_current_document() {
        use crate::document::CurrentDocument;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();

        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene must succeed");

        assert!(
            matches!(session.current_document(), CurrentDocument::Scene { .. }),
            "current document must be Scene"
        );
    }

    #[test]
    fn open_scene_fails_on_unsaved_changes() {
        use crate::document::OpenDocumentError;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();

        let mut session = EditorSession::empty_behavior_tree();
        set_dirty_graph_document(&mut session);

        let err = session
            .open_scene(path)
            .expect_err("open must be rejected when dirty");
        assert!(
            matches!(err, OpenDocumentError::UnsavedChanges),
            "expected UnsavedChanges, got: {err}"
        );
    }

    #[test]
    fn open_scene_discarding_changes_replaces_dirty_document() {
        use crate::document::CurrentDocument;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();

        let mut session = EditorSession::empty_behavior_tree();
        set_dirty_graph_document(&mut session);

        session
            .open_scene_discarding_changes(path)
            .expect("discarding open must succeed while dirty");
        assert!(
            matches!(session.current_document(), CurrentDocument::Scene { .. }),
            "current document must be Scene"
        );
        assert!(!session.is_dirty(), "opened document must start clean");
    }

    // ── open_graph tests ─────────────────────────────────────────────────

    #[test]
    fn open_graph_loads_graph_and_syncs_session_graph() {
        use crate::document::CurrentDocument;
        use engine_authoring::{GraphId, GraphKind};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("player_ai.graph.json");
        let graph = Graph::new(
            GraphId::generate(),
            GraphKind::new("test.graph"),
            "player_ai",
        );
        std::fs::write(&path, serde_json::to_string(&graph).unwrap()).unwrap();

        let mut session = EditorSession::empty_behavior_tree();
        session.open_graph(path).expect("open_graph must succeed");

        assert!(
            matches!(session.current_document(), CurrentDocument::Graph { .. }),
            "current document must be Graph"
        );
        assert_eq!(session.graph().name, "player_ai");
    }

    #[test]
    fn open_graph_clears_undo_redo_history() {
        use engine_authoring::{GraphId, GraphKind};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clears_undo.graph.json");
        let graph = Graph::new(
            GraphId::generate(),
            GraphKind::new("test.graph"),
            "undo_test",
        );
        std::fs::write(&path, serde_json::to_string(&graph).unwrap()).unwrap();

        let mut session = EditorSession::empty_behavior_tree();
        session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "a", None)
            .expect("add node");
        assert!(session.can_undo(), "must have undo history before open");
        session
            .save_as(dir.path().join("current.graph.json"))
            .expect("save_as should clear dirty state before open");
        assert!(
            session.can_undo(),
            "save_as must not clear undo history before open"
        );

        session.open_graph(path).expect("open_graph must succeed");
        assert!(
            !session.can_undo(),
            "undo history must be cleared after open_graph"
        );
    }

    #[test]
    fn open_graph_fails_on_unsaved_changes() {
        use crate::document::OpenDocumentError;
        use engine_authoring::{GraphId, GraphKind};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("y.graph.json");
        let graph = Graph::new(GraphId::generate(), GraphKind::new("test.graph"), "y");
        std::fs::write(&path, serde_json::to_string(&graph).unwrap()).unwrap();

        let mut session = EditorSession::empty_behavior_tree();
        set_dirty_graph_document(&mut session);

        let err = session
            .open_graph(path)
            .expect_err("open must be rejected when dirty");
        assert!(matches!(err, OpenDocumentError::UnsavedChanges));
    }

    #[test]
    fn open_graph_discarding_changes_replaces_dirty_document() {
        use crate::document::CurrentDocument;
        use engine_authoring::{GraphId, GraphKind};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("other.graph.json");
        let graph = Graph::new(GraphId::generate(), GraphKind::new("test.graph"), "other");
        std::fs::write(&path, serde_json::to_string(&graph).unwrap()).unwrap();

        let mut session = EditorSession::empty_behavior_tree();
        set_dirty_graph_document(&mut session);

        session
            .open_graph_discarding_changes(path)
            .expect("discarding open must succeed while dirty");
        assert!(
            matches!(session.current_document(), CurrentDocument::Graph { .. }),
            "current document must be Graph"
        );
        assert_eq!(session.graph().name, "other");
        assert!(!session.is_dirty(), "opened document must start clean");
    }

    // ── legacy combined format rejection test ─────────────────────────────

    #[test]
    fn legacy_combined_json_cannot_be_opened_as_graph_or_scene() {
        use crate::document::{CurrentDocument, OpenDocumentError};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.json");
        std::fs::write(
            &path,
            r#"{"format_version":1,"graph":{},"graph_view":null}"#,
        )
        .unwrap();

        let mut session = EditorSession::empty_behavior_tree();

        // The UI dispatch pushes editor.open_unsupported_file for .json files.
        // Verify that neither supported open method accepts the combined format.
        let scene_err = session
            .open_scene(path.clone())
            .expect_err("legacy combined format must fail as scene");
        assert!(matches!(scene_err, OpenDocumentError::SceneLoad(_)));
        assert!(
            matches!(session.current_document(), CurrentDocument::None),
            "document must remain None after failed scene open"
        );

        let graph_err = session
            .open_graph(path.clone())
            .expect_err("legacy combined format must fail as graph");
        assert!(matches!(graph_err, OpenDocumentError::GraphDeserialize(_)));
        assert!(
            matches!(session.current_document(), CurrentDocument::None),
            "document must remain None after failed graph open"
        );
    }

    // ── Phase 32: Drag & Drop ─────────────────────────────────────────────

    #[test]
    fn ui_document_commands_are_saved_and_undoable_without_raw_json_editing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hud.ui.json");
        std::fs::write(
            &path,
            UiDocument::default()
                .to_json_string()
                .expect("default UI serializes"),
        )
        .unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_ui(path.clone()).expect("UI document opens");

        session
            .apply_ui_command(UiDocumentCommand::InsertNode {
                parent: "root".into(),
                index: 0,
                node: engine_authoring::UiNode {
                    id: "title".into(),
                    kind: engine_authoring::UiNodeKind::Text {
                        content: engine_authoring::UiString::Literal("Title".into()),
                        size: 28.0,
                        color: [1.0; 4],
                    },
                    children: Vec::new(),
                },
            })
            .expect("builder insertion succeeds");
        assert!(session.is_dirty());
        assert!(engine_authoring::find_ui_node(session.ui_document().unwrap(), "title").is_some());

        assert!(session.undo());
        assert!(engine_authoring::find_ui_node(session.ui_document().unwrap(), "title").is_none());
        assert!(!session.is_dirty(), "undo restores the saved snapshot");
        assert!(session.redo());
        session.save().expect("UI document saves atomically");
        let saved = UiDocument::from_json_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(engine_authoring::find_ui_node(&saved, "title").is_some());
        assert!(!session.is_dirty());
    }
}
