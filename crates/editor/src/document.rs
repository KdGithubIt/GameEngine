//! Document-opening logic for scene and graph files.
//!
//! This module is intentionally free of GUI dependencies so that all
//! document-loading logic can be covered by unit tests.
//!
//! All I/O functions accept absolute paths that have already been resolved
//! (e.g. through [`engine_authoring::ProjectRoot::resolve_asset`]).

use engine_authoring::{
    load_scene_from_json, AuthoringScene, Diagnostic, Graph, GraphView, SceneLoadError, Severity,
    UiDocument, UiDocumentError,
};
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

// ---------------------------------------------------------------------------
// Graph data carrier (returned alongside CurrentDocument for graph opens)
// ---------------------------------------------------------------------------

/// Graph and optional view data loaded from disk.
///
/// Returned by [`open_graph_from_path`] alongside the [`CurrentDocument`]
/// metadata.  The caller (session layer) owns these values and is responsible
/// for placing them in the appropriate live fields.
#[derive(Debug)]
pub struct LoadedGraphData {
    /// The parsed semantic graph.
    pub graph: Graph,
    /// The presentation graph view, if present.
    pub view: Option<GraphView>,
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Represents the document currently held open by the editor session.
///
/// The editor starts with [`CurrentDocument::None`] and transitions to a
/// [`Scene`][`CurrentDocument::Scene`], [`Graph`][`CurrentDocument::Graph`],
/// or [`Ui`][`CurrentDocument::Ui`] variant when the user opens a file.
#[derive(Debug)]
pub enum CurrentDocument {
    /// No document is open.
    None,

    /// A scene file (`*.scene.json`) is open.
    Scene {
        /// The parsed authoring scene.
        scene: AuthoringScene,
        /// Absolute path to the `*.scene.json` file.
        path: PathBuf,
        /// Whether the in-memory scene differs from the file on disk.
        is_dirty: bool,
    },

    /// A graph (`*.graph.json`) and optional view (`*.graph.view.json`) are
    /// open.
    ///
    /// The live graph and view data are stored in the session's `graph` and
    /// `graph_view` fields, not here.
    Graph {
        /// Absolute path to the (current or target) `*.graph.json` file.
        graph_path: PathBuf,
        /// Absolute path to the (current or target) `*.graph.view.json` file,
        /// when a view was loaded or a target path could be derived.
        view_path: Option<PathBuf>,
        /// Whether the in-memory document differs from the files on disk.
        is_dirty: bool,
    },

    /// A declarative UI document (`*.ui.json`) is open.
    Ui {
        /// The parsed UI tree edited through authoring commands.
        document: UiDocument,
        /// Absolute path to the `*.ui.json` file.
        path: PathBuf,
        /// Whether the in-memory document differs from the file on disk.
        is_dirty: bool,
    },
}

impl CurrentDocument {
    /// Returns `true` when the in-memory state has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        match self {
            Self::None => false,
            Self::Scene { is_dirty, .. }
            | Self::Graph { is_dirty, .. }
            | Self::Ui { is_dirty, .. } => *is_dirty,
        }
    }

    /// Marks the document as having unsaved changes.
    ///
    /// Has no effect when no document is open.
    pub(crate) fn mark_dirty(&mut self) {
        match self {
            Self::None => {}
            Self::Scene { is_dirty, .. }
            | Self::Graph { is_dirty, .. }
            | Self::Ui { is_dirty, .. } => *is_dirty = true,
        }
    }

    /// Marks the document as matching the files on disk.
    ///
    /// Has no effect when no document is open.
    pub(crate) fn mark_clean(&mut self) {
        match self {
            Self::None => {}
            Self::Scene { is_dirty, .. }
            | Self::Graph { is_dirty, .. }
            | Self::Ui { is_dirty, .. } => *is_dirty = false,
        }
    }
}

// ---------------------------------------------------------------------------
// Open errors
// ---------------------------------------------------------------------------

/// Describes why a document could not be opened.
#[derive(Debug)]
pub enum OpenDocumentError {
    /// File read failed.
    Io(io::Error),
    /// Scene JSON could not be parsed or failed version validation.
    SceneLoad(SceneLoadError),
    /// Semantic graph JSON could not be deserialized.
    GraphDeserialize(serde_json::Error),
    /// Graph view JSON could not be deserialized.
    GraphViewDeserialize(serde_json::Error),
    /// UI JSON could not be parsed or uses a future schema version.
    UiDocument(UiDocumentError),
    /// UI JSON parsed but failed whole-document validation.
    UiValidation(Vec<Diagnostic>),
    /// The current document has unsaved changes.  Open was refused to prevent
    /// data loss.  Save or discard changes before opening another file.
    UnsavedChanges,
}

impl fmt::Display for OpenDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::SceneLoad(e) => write!(f, "scene load failed: {e}"),
            Self::GraphDeserialize(e) => write!(f, "graph JSON error: {e}"),
            Self::GraphViewDeserialize(e) => write!(f, "graph view JSON error: {e}"),
            Self::UiDocument(e) => write!(f, "UI document load failed: {e}"),
            Self::UiValidation(diagnostics) => write!(
                f,
                "UI document validation failed with {} error(s)",
                diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.severity == Severity::Error)
                    .count()
            ),
            Self::UnsavedChanges => write!(
                f,
                "cannot open document: current document has unsaved changes"
            ),
        }
    }
}

impl std::error::Error for OpenDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::SceneLoad(e) => Some(e),
            Self::GraphDeserialize(e) | Self::GraphViewDeserialize(e) => Some(e),
            Self::UiDocument(e) => Some(e),
            Self::UiValidation(_) | Self::UnsavedChanges => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Document-opening functions
// ---------------------------------------------------------------------------

/// Opens a `*.scene.json` file from an absolute `path`.
///
/// Reads the file and delegates to [`load_scene_from_json`], which validates
/// the `schema_version` and returns [`SceneLoadError::UnsupportedVersion`]
/// when the file was written by a newer version of the engine.
///
/// # Errors
///
/// - [`OpenDocumentError::Io`] if the file cannot be read.
/// - [`OpenDocumentError::SceneLoad`] if JSON parsing or version validation
///   fails.
pub fn open_scene_from_path(path: &Path) -> Result<CurrentDocument, OpenDocumentError> {
    let json = fs::read_to_string(path).map_err(OpenDocumentError::Io)?;
    let scene = load_scene_from_json(&json).map_err(OpenDocumentError::SceneLoad)?;
    Ok(CurrentDocument::Scene {
        scene,
        path: path.to_path_buf(),
        is_dirty: false,
    })
}

/// Opens a `*.graph.json` file from an absolute `graph_path` and
/// auto-discovers a sibling `*.graph.view.json` when one exists.
///
/// A missing view file is not an error; the caller may regenerate a layout
/// from the semantic graph if needed.
///
/// Returns `(document_metadata, graph_data)`.  The caller (session layer)
/// is responsible for placing the graph data into its live fields.
///
/// # Errors
///
/// - [`OpenDocumentError::Io`] if either file cannot be read.
/// - [`OpenDocumentError::GraphDeserialize`] if the graph JSON is invalid.
/// - [`OpenDocumentError::GraphViewDeserialize`] if the view JSON is invalid.
pub fn open_graph_from_path(
    graph_path: &Path,
) -> Result<(CurrentDocument, LoadedGraphData), OpenDocumentError> {
    let json = fs::read_to_string(graph_path).map_err(OpenDocumentError::Io)?;
    let graph: Graph = serde_json::from_str(&json).map_err(OpenDocumentError::GraphDeserialize)?;

    let sibling = derive_view_path(graph_path);
    let (view, view_path) = match sibling {
        Some(ref vp) if vp.exists() => {
            let view_json = fs::read_to_string(vp).map_err(OpenDocumentError::Io)?;
            let view: GraphView = serde_json::from_str(&view_json)
                .map_err(OpenDocumentError::GraphViewDeserialize)?;
            (Some(view), Some(vp.clone()))
        }
        other => (None, other),
    };

    let doc = CurrentDocument::Graph {
        graph_path: graph_path.to_path_buf(),
        view_path,
        is_dirty: false,
    };
    Ok((doc, LoadedGraphData { graph, view }))
}

/// Opens and validates a declarative `*.ui.json` document.
///
/// Invalid documents are rejected before they become editable session state,
/// so save and runtime preview never need to guess whether a malformed tree
/// was intentionally accepted.
pub fn open_ui_from_path(path: &Path) -> Result<CurrentDocument, OpenDocumentError> {
    let json = fs::read_to_string(path).map_err(OpenDocumentError::Io)?;
    let document = UiDocument::from_json_str(&json).map_err(OpenDocumentError::UiDocument)?;
    let diagnostics = document.validate();
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(OpenDocumentError::UiValidation(diagnostics));
    }
    Ok(CurrentDocument::Ui {
        document,
        path: path.to_path_buf(),
        is_dirty: false,
    })
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Given `foo.graph.json`, returns `Some(foo.graph.view.json)` in the same
/// directory.  Returns `None` when the path has no parent or the filename is
/// not valid UTF-8.
pub(crate) fn derive_view_path(graph_path: &Path) -> Option<PathBuf> {
    let name = graph_path.file_name()?.to_str()?;
    let stem = name.strip_suffix(".graph.json")?;
    let view_name = format!("{stem}.graph.view.json");
    graph_path.parent().map(|p| p.join(view_name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{GraphId, GraphKind, GraphView};
    use std::fs;

    // --- derive_view_path ---------------------------------------------------

    #[test]
    fn derive_view_path_appends_view_suffix() {
        let path = Path::new("/project/assets/graphs/foo.graph.json");
        let view = derive_view_path(path).expect("must derive view path");
        assert_eq!(
            view,
            Path::new("/project/assets/graphs/foo.graph.view.json")
        );
    }

    #[test]
    fn derive_view_path_returns_none_for_non_graph_json_name() {
        let path = Path::new("/project/assets/foo.json");
        assert_eq!(derive_view_path(path), None);
    }

    // --- open_scene_from_path -----------------------------------------------

    #[test]
    fn open_scene_from_path_loads_valid_scene() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("level1.scene.json");
        let json = r#"{"schema_version":1,"entities":[]}"#;
        fs::write(&path, json).unwrap();

        let doc = open_scene_from_path(&path).expect("open must succeed");
        match doc {
            CurrentDocument::Scene {
                is_dirty: false, ..
            } => {}
            _ => panic!("expected Scene with is_dirty=false"),
        }
    }

    #[test]
    fn open_scene_from_path_fails_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.scene.json");
        let err = open_scene_from_path(&path).expect_err("missing file must fail");
        assert!(
            matches!(err, OpenDocumentError::Io(_)),
            "expected Io error, got: {err}"
        );
    }

    #[test]
    fn open_scene_from_path_fails_on_unsupported_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.scene.json");
        fs::write(&path, r#"{"schema_version":99,"entities":[]}"#).unwrap();

        let err = open_scene_from_path(&path).expect_err("future version must fail");
        assert!(
            matches!(err, OpenDocumentError::SceneLoad(_)),
            "expected SceneLoad error, got: {err}"
        );
    }

    #[test]
    fn open_scene_from_path_fails_on_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.scene.json");
        fs::write(&path, b"not valid json").unwrap();

        let err = open_scene_from_path(&path).expect_err("malformed JSON must fail");
        assert!(
            matches!(err, OpenDocumentError::SceneLoad(_)),
            "expected SceneLoad error, got: {err}"
        );
    }

    // --- open_graph_from_path -----------------------------------------------

    fn write_minimal_graph(path: &Path) -> Graph {
        let graph = Graph::new(
            GraphId::generate(),
            GraphKind::new("test.graph"),
            "test_graph",
        );
        let json = serde_json::to_string_pretty(&graph).unwrap();
        fs::write(path, &json).unwrap();
        graph
    }

    #[test]
    fn open_graph_from_path_loads_graph_without_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("player_ai.graph.json");
        write_minimal_graph(&path);

        let (doc, data) = open_graph_from_path(&path).expect("open must succeed");
        assert!(
            matches!(
                doc,
                CurrentDocument::Graph {
                    is_dirty: false,
                    ..
                }
            ),
            "expected Graph with is_dirty=false"
        );
        assert!(data.view.is_none(), "expected no view");
    }

    #[test]
    fn open_graph_from_path_loads_sibling_view_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let graph_path = dir.path().join("player_ai.graph.json");
        let view_path = dir.path().join("player_ai.graph.view.json");

        let graph = write_minimal_graph(&graph_path);
        let view = GraphView::new(graph.id.clone());
        let view_json = serde_json::to_string_pretty(&view).unwrap();
        fs::write(&view_path, &view_json).unwrap();

        let (doc, data) = open_graph_from_path(&graph_path).expect("open must succeed");
        assert!(
            matches!(
                doc,
                CurrentDocument::Graph {
                    is_dirty: false,
                    ..
                }
            ),
            "expected Graph with is_dirty=false"
        );
        assert!(data.view.is_some(), "expected sibling view to be loaded");
    }

    #[test]
    fn open_graph_from_path_ignores_missing_sibling_view() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_view.graph.json");
        write_minimal_graph(&path);

        let (_doc, data) = open_graph_from_path(&path).expect("open without view must succeed");
        assert!(
            data.view.is_none(),
            "expected view=None when sibling absent"
        );
    }

    #[test]
    fn open_graph_from_path_fails_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.graph.json");
        let err = open_graph_from_path(&path).expect_err("missing file must fail");
        assert!(matches!(err, OpenDocumentError::Io(_)));
    }

    #[test]
    fn open_graph_from_path_fails_on_malformed_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.graph.json");
        fs::write(&path, b"not valid json").unwrap();

        let err = open_graph_from_path(&path).expect_err("malformed JSON must fail");
        assert!(
            matches!(err, OpenDocumentError::GraphDeserialize(_)),
            "expected GraphDeserialize error, got: {err}"
        );
    }

    #[test]
    fn open_graph_from_path_view_path_is_set_even_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.graph.json");
        write_minimal_graph(&path);

        let (doc, _data) = open_graph_from_path(&path).expect("must succeed");
        match &doc {
            CurrentDocument::Graph { view_path, .. } => {
                let vp = view_path.as_ref().expect("view_path must be set");
                assert!(vp.to_string_lossy().ends_with("x.graph.view.json"));
            }
            _ => panic!("expected Graph variant"),
        }
    }

    // --- CurrentDocument accessors ------------------------------------------

    #[test]
    fn current_document_none_is_not_dirty() {
        assert!(!CurrentDocument::None.is_dirty());
    }

    #[test]
    fn mark_dirty_sets_is_dirty_on_graph() {
        let mut doc = CurrentDocument::Graph {
            graph_path: PathBuf::from("x.graph.json"),
            view_path: None,
            is_dirty: false,
        };
        assert!(!doc.is_dirty());
        doc.mark_dirty();
        assert!(doc.is_dirty());
    }

    #[test]
    fn is_dirty_returns_true_for_dirty_scene() {
        let doc = CurrentDocument::Scene {
            scene: AuthoringScene::new(),
            path: PathBuf::from("x.scene.json"),
            is_dirty: true,
        };
        assert!(doc.is_dirty());
    }

    // --- BehaviorTreeDomain round-trip --------------------------------------

    #[test]
    fn open_graph_from_path_round_trips_behavior_tree_graph() {
        use engine_authoring::BehaviorTreeAuthoringService;

        let dir = tempfile::tempdir().unwrap();
        let graph_path = dir.path().join("bt.graph.json");

        let example = BehaviorTreeAuthoringService::new()
            .example()
            .expect("example must build");
        let json = serde_json::to_string_pretty(&example.graph).unwrap();
        fs::write(&graph_path, &json).unwrap();

        let view_path = derive_view_path(&graph_path).unwrap();
        let view_json = serde_json::to_string_pretty(&example.view).unwrap();
        fs::write(&view_path, &view_json).unwrap();

        let (doc, data) = open_graph_from_path(&graph_path).expect("round-trip must succeed");
        assert!(
            matches!(doc, CurrentDocument::Graph { .. }),
            "expected Graph variant"
        );
        assert!(
            !data.graph.nodes.is_empty(),
            "nodes must survive round-trip"
        );
        assert!(data.view.is_some(), "view must be loaded");
    }

    #[test]
    fn open_ui_from_path_loads_a_current_version_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hud.ui.json");
        fs::write(
            &path,
            r#"{"schema_version":3,"root":{"id":"root","type":"panel","children":[]}}"#,
        )
        .unwrap();

        let document = open_ui_from_path(&path).expect("UI document must open");
        match document {
            CurrentDocument::Ui {
                document,
                path: loaded_path,
                is_dirty,
            } => {
                assert_eq!(loaded_path, path);
                assert_eq!(document.schema_version, engine_authoring::UI_SCHEMA_VERSION);
                assert!(!is_dirty);
            }
            _ => panic!("expected UI document"),
        }
    }
}
