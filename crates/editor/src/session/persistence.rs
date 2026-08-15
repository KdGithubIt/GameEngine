//! Saving documents, crash-recovery sidecars, and loading.
//!
//! A dirty document is mirrored to a `.autosave` sidecar beside its source.
//! Opening a document restores that sidecar when it is newer than the file it
//! shadows; a normal save removes it again.

use super::errors::{EditorLoadError, EditorPersistError};
use super::EditorSession;
use crate::document::{derive_view_path, CurrentDocument};
use engine_authoring::{
    replace_file_contents, AuthoringSession, Diagnostic, Graph, GraphView, UiDocument,
};
use std::path::{Path, PathBuf};

pub(super) fn remove_recovery_file(source: &Path) {
    let recovery = recovery_path(source);
    if recovery.is_file() {
        let _ = std::fs::remove_file(recovery);
    }
}

impl EditorSession {
    /// Saves the current document to its existing path.
    ///
    /// Graph documents are saved in the ADR 0022 separate-file layout:
    /// semantic graph first, then graph view when one exists.
    ///
    /// # Errors
    ///
    /// Returns [`EditorPersistError::NoDocument`] when no document path is
    /// associated with the session. Use [`save_as`][Self::save_as] first.
    pub fn save(&mut self) -> Result<(), EditorPersistError> {
        match &self.current_document {
            CurrentDocument::None => Err(EditorPersistError::NoDocument),
            CurrentDocument::Scene { scene, path, .. } => {
                let path = path.clone();
                let json = scene
                    .to_canonical_json()
                    .map_err(EditorPersistError::SceneSave)?;
                replace_file_contents(&path, &json).map_err(EditorPersistError::Persist)?;
                self.mark_clean();
                remove_recovery_file(&path);
                Ok(())
            }
            CurrentDocument::Graph {
                graph_path,
                view_path,
                ..
            } => {
                let graph_path = graph_path.clone();
                let view_path = view_path.clone();
                self.save_graph_files(&graph_path, view_path.as_deref())?;
                self.mark_clean();
                remove_recovery_file(&graph_path);
                Ok(())
            }
            CurrentDocument::Ui { document, path, .. } => {
                let path = path.clone();
                let diagnostics = document.validate();
                if diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.severity == engine_authoring::Severity::Error)
                {
                    return Err(EditorPersistError::InvalidUiDocument { diagnostics });
                }
                let json = document
                    .to_json_string()
                    .map_err(EditorPersistError::UiSerialize)?;
                replace_file_contents(&path, &json).map_err(EditorPersistError::Persist)?;
                self.mark_clean();
                remove_recovery_file(&path);
                Ok(())
            }
        }
    }

    /// Saves the current document to `new_path` and updates the document path.
    ///
    /// For graph sessions, `new_path` must end with `.graph.json`; the graph
    /// view target is derived as the sibling `.graph.view.json`.
    pub fn save_as(&mut self, new_path: PathBuf) -> Result<(), EditorPersistError> {
        let previous_path = self.current_document_path().map(Path::to_path_buf);
        if let CurrentDocument::Scene { scene, .. } = &self.current_document {
            let json = scene
                .to_canonical_json()
                .map_err(EditorPersistError::SceneSave)?;
            replace_file_contents(&new_path, &json).map_err(EditorPersistError::Persist)?;
            if let CurrentDocument::Scene { path, is_dirty, .. } = &mut self.current_document {
                *path = new_path.clone();
                *is_dirty = false;
            }
            self.record_clean_snapshot();
            remove_previous_recovery(previous_path.as_deref(), &new_path);
            return Ok(());
        }

        if let CurrentDocument::Ui { document, .. } = &self.current_document {
            let diagnostics = document.validate();
            if diagnostics
                .iter()
                .any(|diagnostic| diagnostic.severity == engine_authoring::Severity::Error)
            {
                return Err(EditorPersistError::InvalidUiDocument { diagnostics });
            }
            let json = document
                .to_json_string()
                .map_err(EditorPersistError::UiSerialize)?;
            replace_file_contents(&new_path, &json).map_err(EditorPersistError::Persist)?;
            self.current_document = CurrentDocument::Ui {
                document: document.clone(),
                path: new_path.clone(),
                is_dirty: false,
            };
            self.record_clean_snapshot();
            remove_previous_recovery(previous_path.as_deref(), &new_path);
            return Ok(());
        }

        let view_path =
            derive_view_path(&new_path).ok_or_else(|| EditorPersistError::InvalidGraphPath {
                path: new_path.clone(),
            })?;
        self.save_graph_files(&new_path, Some(&view_path))?;
        self.current_document = CurrentDocument::Graph {
            graph_path: new_path.clone(),
            view_path: Some(view_path),
            is_dirty: false,
        };
        self.record_clean_snapshot();
        remove_previous_recovery(previous_path.as_deref(), &new_path);
        Ok(())
    }

    /// Writes a crash-recovery snapshot beside the current source document.
    ///
    /// The exact `.autosave` sibling is ignored by the Asset Browser because
    /// the browser lists only recognized asset suffixes. Normal saves remove it.
    pub fn autosave_recovery(&self) -> Result<Option<PathBuf>, EditorPersistError> {
        let Some(path) = self.current_document_path() else {
            return Ok(None);
        };
        if !self.is_dirty() {
            return Ok(None);
        }
        let contents = match &self.current_document {
            CurrentDocument::Scene { scene, .. } => scene
                .to_canonical_json()
                .map_err(EditorPersistError::SceneSave)?,
            CurrentDocument::Graph { .. } => {
                #[derive(serde::Serialize)]
                struct GraphRecovery<'a> {
                    graph: &'a Graph,
                    graph_view: Option<&'a GraphView>,
                }
                serde_json::to_string_pretty(&GraphRecovery {
                    graph: &self.graph,
                    graph_view: self.graph_view.as_ref(),
                })
                .map_err(EditorPersistError::UiSerialize)?
            }
            CurrentDocument::Ui { document, .. } => document
                .to_json_string()
                .map_err(EditorPersistError::UiSerialize)?,
            CurrentDocument::None => return Ok(None),
        };
        let recovery = recovery_path(path);
        replace_file_contents(&recovery, &contents).map_err(EditorPersistError::Persist)?;
        Ok(Some(recovery))
    }

    /// Loads an editor session from a combined JSON file at `path`.
    ///
    /// Undo/redo history is empty in the returned session. Callers should call
    /// [`EditorSession::clear_undo_redo`] if replacing an existing session.
    ///
    /// # Errors
    ///
    /// Returns [`EditorLoadError`] if the file cannot be read, the JSON is
    /// invalid, `format_version` is not `1`, or a document fails to
    /// deserialize.
    pub fn load_from_path(path: &Path) -> Result<Self, EditorLoadError> {
        let json = std::fs::read_to_string(path).map_err(EditorLoadError::Io)?;
        let doc: serde_json::Value = serde_json::from_str(&json).map_err(EditorLoadError::Json)?;

        let version = doc
            .get("format_version")
            .and_then(|v| v.as_u64())
            .ok_or(EditorLoadError::MissingField("format_version"))?;
        if version != 1 {
            return Err(EditorLoadError::UnsupportedVersion(version));
        }

        let graph_value = doc
            .get("graph")
            .cloned()
            .ok_or(EditorLoadError::MissingField("graph"))?;
        let graph: Graph = serde_json::from_value(graph_value).map_err(EditorLoadError::Json)?;

        let graph_view: Option<GraphView> = match doc.get("graph_view") {
            Some(v) if !v.is_null() => {
                Some(serde_json::from_value(v.clone()).map_err(EditorLoadError::Json)?)
            }
            _ => None,
        };

        let mut session = EditorSession::new(graph, graph_view);
        session.diagnostics = session.domain.validate_domain(&session.graph);
        Ok(session)
    }

    fn save_graph_files(
        &self,
        graph_path: &Path,
        view_path: Option<&Path>,
    ) -> Result<(), EditorPersistError> {
        let graph_json = self
            .graph
            .to_canonical_json(self.domain.schema_registry())
            .map_err(EditorPersistError::GraphSave)?;
        replace_file_contents(graph_path, &graph_json).map_err(EditorPersistError::Persist)?;

        if let Some(view) = &self.graph_view {
            let view_path = view_path.ok_or(EditorPersistError::MissingGraphViewPath)?;
            let view_json = view
                .to_canonical_json(&self.graph)
                .map_err(EditorPersistError::GraphViewSave)?;
            replace_file_contents(view_path, &view_json).map_err(EditorPersistError::Persist)?;
        }

        Ok(())
    }

    pub(super) fn restore_newer_recovery(&mut self) {
        let Some(source) = self.current_document_path().map(Path::to_path_buf) else {
            return;
        };
        let recovery = recovery_path(&source);
        if !recovery_is_newer(&source, &recovery) {
            return;
        }
        let Ok(json) = std::fs::read_to_string(&recovery) else {
            return;
        };
        let restored = match &mut self.current_document {
            CurrentDocument::Scene { scene, .. } => {
                match engine_authoring::load_scene_from_json(&json) {
                    Ok(recovered) => {
                        *scene = recovered.clone();
                        self.scene_session = Some(AuthoringSession::new(recovered));
                        true
                    }
                    Err(_) => false,
                }
            }
            CurrentDocument::Graph { .. } => {
                #[derive(serde::Deserialize)]
                struct GraphRecovery {
                    graph: Graph,
                    graph_view: Option<GraphView>,
                }
                match serde_json::from_str::<GraphRecovery>(&json) {
                    Ok(recovered) => {
                        self.graph = recovered.graph;
                        self.graph_view = recovered.graph_view;
                        true
                    }
                    Err(_) => false,
                }
            }
            CurrentDocument::Ui { document, .. } => match UiDocument::from_json_str(&json) {
                Ok(recovered) if recovered.validate().iter().all(|item| !item.is_blocking()) => {
                    *document = recovered;
                    true
                }
                Ok(_) | Err(_) => false,
            },
            CurrentDocument::None => false,
        };
        if restored {
            self.undo_stack.clear();
            self.mark_dirty();
            self.diagnostics.push(Diagnostic::warning(
                "editor.recovery_restored",
                format!(
                    "restored a newer crash-recovery snapshot from {}",
                    recovery.display()
                ),
            ));
        } else {
            self.diagnostics.push(Diagnostic::warning(
                "editor.recovery_invalid",
                format!(
                    "ignored an invalid crash-recovery snapshot at {}",
                    recovery.display()
                ),
            ));
        }
    }
}

fn recovery_path(source: &Path) -> PathBuf {
    let mut file_name = source
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "document".into());
    file_name.push(".autosave");
    source.with_file_name(file_name)
}

fn recovery_is_newer(source: &Path, recovery: &Path) -> bool {
    let Ok(recovery_modified) = recovery.metadata().and_then(|metadata| metadata.modified()) else {
        return false;
    };
    source
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map_or(true, |source_modified| recovery_modified > source_modified)
}

/// Removes a recovery sidecar from a document's former location after a
/// successful Save As. The active destination is never removed here because
/// a failed write must leave its recovery data intact.
fn remove_previous_recovery(previous: Option<&Path>, destination: &Path) {
    if let Some(previous) = previous.filter(|previous| *previous != destination) {
        remove_recovery_file(previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::open_graph_from_path;
    use crate::session::BehaviorNodeInsertKind;
    use engine_authoring::UiDocumentCommand;

    #[test]
    fn save_as_graph_writes_separate_files_and_marks_clean() {
        let mut session =
            EditorSession::behavior_tree_example().expect("example session should be valid");
        let original_node_count = session.graph().nodes.len();
        let original_edge_count = session.graph().edges.len();

        let dir = tempfile::tempdir().unwrap();
        let graph_path = dir.path().join("roundtrip.graph.json");
        let view_path = dir.path().join("roundtrip.graph.view.json");
        session
            .save_as(graph_path.clone())
            .expect("save_as must succeed");

        assert!(graph_path.exists(), "semantic graph file must be written");
        assert!(view_path.exists(), "graph view file must be written");
        assert!(!session.is_dirty(), "save_as must mark document clean");
        assert!(matches!(
            session.current_document(),
            CurrentDocument::Graph {
                is_dirty: false,
                ..
            }
        ));

        let (doc, data) = open_graph_from_path(&graph_path).expect("saved graph must open");
        assert!(matches!(doc, CurrentDocument::Graph { .. }));
        assert_eq!(data.graph.nodes.len(), original_node_count);
        assert_eq!(data.graph.edges.len(), original_edge_count);
        assert_eq!(data.graph.name, session.graph().name);
        assert!(data.view.is_some());
    }

    #[test]
    fn save_graph_marks_existing_document_clean() {
        let mut session =
            EditorSession::behavior_tree_example().expect("example session should be valid");
        let dir = tempfile::tempdir().unwrap();
        let graph_path = dir.path().join("existing.graph.json");
        session
            .save_as(graph_path.clone())
            .expect("initial save must succeed");

        session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "after_save", None)
            .expect("edit must succeed");
        assert!(session.is_dirty(), "edit must mark graph document dirty");

        session.save().expect("save must succeed");
        assert!(!session.is_dirty(), "save must mark document clean");
        assert!(
            graph_path.exists(),
            "semantic graph file must remain at original path"
        );
    }

    #[test]
    fn untitled_graph_edit_is_dirty_blocks_open_and_save_as_clears_it() {
        use crate::document::OpenDocumentError;

        let mut session = EditorSession::empty_behavior_tree();
        session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "new_action", None)
            .expect("edit must succeed");
        assert!(session.is_dirty(), "untitled graph edit must be dirty");

        let dir = tempfile::tempdir().unwrap();
        let scene_path = dir.path().join("other.scene.json");
        std::fs::write(&scene_path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let err = session
            .open_scene(scene_path)
            .expect_err("dirty untitled session must block open");
        assert!(matches!(err, OpenDocumentError::UnsavedChanges));

        let graph_path = dir.path().join("untitled.graph.json");
        session.save_as(graph_path).expect("save_as must succeed");
        assert!(
            !session.is_dirty(),
            "save_as must clear untitled dirty flag"
        );
    }

    #[test]
    fn save_scene_writes_canonical_json_and_marks_clean() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();

        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path.clone()).expect("open_scene");
        session.current_document.mark_dirty();
        assert!(session.is_dirty());

        session.save().expect("scene save must succeed");

        assert!(!session.is_dirty(), "scene save must mark document clean");
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved.contains("\"schema_version\": 1"),
            "saved scene must be canonical schema-versioned JSON"
        );
    }

    #[test]
    fn load_from_nonexistent_path_returns_io_error() {
        use crate::session::EditorLoadError;
        let path = std::env::temp_dir().join("this_file_should_not_exist_12345.json");
        let _ = std::fs::remove_file(&path);
        let result = EditorSession::load_from_path(&path);
        assert!(
            matches!(result, Err(EditorLoadError::Io(_))),
            "missing file must return EditorLoadError::Io"
        );
    }

    #[test]
    fn load_unsupported_version_returns_error() {
        use crate::session::EditorLoadError;
        let json = r#"{"format_version":99,"graph":{},"graph_view":null}"#;
        let path = std::env::temp_dir().join("engine_editor_bad_version_test.json");
        std::fs::write(&path, json).expect("write temp file must succeed");
        let result = EditorSession::load_from_path(&path);
        assert!(
            matches!(result, Err(EditorLoadError::UnsupportedVersion(99))),
            "version 99 must return EditorLoadError::UnsupportedVersion(99)"
        );
    }

    #[test]
    fn save_and_load_empty_session_with_no_view() {
        let session = EditorSession::empty_behavior_tree();
        let path = std::env::temp_dir().join("engine_editor_empty_test.json");
        let doc = serde_json::json!({
            "format_version": 1u32,
            "graph": serde_json::to_value(session.graph()).unwrap(),
            "graph_view": serde_json::Value::Null,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let loaded = EditorSession::load_from_path(&path).expect("load must succeed");
        assert_eq!(loaded.graph().nodes.len(), 0);
        assert_eq!(loaded.graph().edges.len(), 0);
        assert!(loaded.graph_view().is_none());
    }

    // ── open_scene tests ─────────────────────────────────────────────────

    #[test]
    fn newer_ui_autosave_is_restored_as_dirty_recovery_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("menu.ui.json");
        std::fs::write(&path, UiDocument::default().to_json_string().unwrap()).unwrap();
        let mut editing = EditorSession::empty_behavior_tree();
        editing.open_ui(path.clone()).unwrap();
        editing
            .apply_ui_command(UiDocumentCommand::InsertNode {
                parent: "root".into(),
                index: 0,
                node: engine_authoring::UiNode {
                    id: "recovered".into(),
                    kind: engine_authoring::UiNodeKind::Spacer { size: 12.0 },
                    children: Vec::new(),
                },
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let recovery = editing
            .autosave_recovery()
            .expect("recovery write succeeds")
            .expect("dirty document produces recovery file");
        assert!(recovery.is_file());

        let mut reopened = EditorSession::empty_behavior_tree();
        reopened.open_ui(path).expect("source and recovery open");
        assert!(reopened.is_dirty());
        assert!(
            engine_authoring::find_ui_node(reopened.ui_document().unwrap(), "recovered").is_some()
        );
        assert!(reopened
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.recovery_restored"));
    }
}
