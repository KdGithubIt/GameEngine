//! Toolkit-independent editor session state.
//!
//! [`EditorSession`] owns the open document, its authoring history, and the
//! diagnostics the editor shell renders. The type is deliberately free of any
//! GUI toolkit dependency: UI code translates gestures into authoring commands
//! and applies them through this boundary.
//!
//! The session surface is large enough that it is split by editing area. This
//! module owns the state itself, its constructors and accessors, and the scene
//! command funnel every scene-editing submodule commits through. Each
//! submodule extends [`EditorSession`] with one area:
//!
//! - [`undo`] — snapshot history, dirty tracking, and clean baselines
//! - [`graph_edit`] — domain-neutral semantic graph and presentation editing
//! - [`behavior_edit`] — Behavior Tree node creation and child edges
//! - [`animation_edit`] — Animation Graph states, transitions, motion slots
//! - [`scene_entities`] — scene entity lifecycle, hierarchy, and metadata
//! - [`scene_components`] — component add, remove, and value edits
//! - [`scene_models`] — Skinned Model and renderer part relationships
//! - [`scene_clipboard`] — duplicate and paste with stable ID remapping
//! - [`scene_transform`] — batch transform, align, and distribute operations
//! - [`documents`] — opening scene, graph, and UI documents
//! - [`persistence`] — saving, crash recovery, and loading
//! - [`errors`] — the error types every area returns

mod animation_edit;
mod behavior_edit;
mod documents;
mod errors;
mod graph_edit;
mod persistence;
mod scene_clipboard;
mod scene_components;
mod scene_entities;
mod scene_models;
mod scene_transform;
mod undo;

#[cfg(test)]
mod test_support;

pub use animation_edit::AnimationNodeInsertKind;
pub use behavior_edit::BehaviorNodeInsertKind;
pub use errors::{EditorLoadError, EditorPersistError, EditorSessionError};
pub use graph_edit::GraphNodeInsertKind;
pub use scene_transform::{SceneAlignment, SceneAxis};

use self::undo::{CleanSnapshot, UndoStack};
use crate::document::CurrentDocument;
use engine_authoring::{
    AnimationGraphDomain, AuthoringCommand, AuthoringEntity, AuthoringScene, AuthoringSession,
    BehaviorTreeAuthoringService, BehaviorTreeDomain, BehaviorTreeServiceError, Diagnostic, EdgeId,
    EntityId, Graph, GraphDomain, GraphId, GraphSchemaRegistry, GraphView, NodeId,
    TransactionError, UiDocument, Vec2,
};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Component type identifier of the authored position transform.
const TRANSFORM_COMPONENT_ID: &str = "engine.transform";

/// Process-wide source of document revisions (ADR 0072).
///
/// Each open document lives in its own tab-owned [`EditorSession`] with an
/// independent counter, so a per-session counter would hand two different
/// documents the same value (both reach 1 on their first open) and let the
/// shared Scene View mistake one scene for another. Drawing every revision
/// from one global sequence makes the value unique across all sessions and
/// tabs, so any document switch changes it.
static DOCUMENT_REVISION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Concrete graph domain selected from the semantic document's stable kind.
///
/// The visual editor keeps this adapter enum instead of teaching the
/// domain-neutral graph foundation about concrete products. Each variant owns
/// its schema and validation implementation while the session presents one
/// command-backed editing surface.
enum EditorGraphDomain {
    /// Behavior Tree authoring and ordered child edges.
    BehaviorTree(BehaviorTreeDomain),
    /// Animation state-machine authoring and directed transitions.
    Animation(AnimationGraphDomain),
}

impl EditorGraphDomain {
    /// Selects the concrete editor domain for a loaded semantic graph.
    fn for_graph(graph: &Graph) -> Self {
        let animation = AnimationGraphDomain::new();
        if graph.kind == *animation.graph_kind() {
            Self::Animation(animation)
        } else {
            Self::BehaviorTree(BehaviorTreeDomain::new())
        }
    }

    /// Returns the schema registry used by structural graph transactions.
    fn schema_registry(&self) -> &dyn GraphSchemaRegistry {
        match self {
            Self::BehaviorTree(domain) => domain.schema_registry(),
            Self::Animation(domain) => domain.schema_registry(),
        }
    }

    /// Runs semantic validation owned by the selected concrete domain.
    fn validate_domain(&self, graph: &Graph) -> Vec<Diagnostic> {
        match self {
            Self::BehaviorTree(domain) => domain.validate_domain(graph),
            Self::Animation(domain) => domain.validate_domain(graph),
        }
    }
}

/// Returns the next globally unique document revision.
fn next_document_revision() -> u64 {
    DOCUMENT_REVISION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

/// Toolkit-independent editor session state.
///
/// This type intentionally does not expose or depend on egui, eframe, or any
/// GUI toolkit type. UI code should translate gestures into authoring commands
/// and apply them through this session boundary.
pub struct EditorSession {
    graph: Graph,
    graph_view: Option<GraphView>,
    diagnostics: Vec<Diagnostic>,
    domain: EditorGraphDomain,
    undo_stack: UndoStack,
    /// The document currently open in the session.
    current_document: CurrentDocument,
    /// Scene edit history for the open scene document.
    scene_session: Option<AuthoringSession>,
    /// Dirty state for a graph session that has not been saved to a document.
    untitled_dirty: bool,
    /// Baseline used to make undoing back to the saved state clean again.
    clean_snapshot: Option<CleanSnapshot>,
    /// Globally unique revision bumped whenever the open document's content or
    /// identity changes (ADR 0072). The Scene View compares this to decide
    /// whether its persistent preview world is stale. Values are drawn from a
    /// process-wide sequence so two tab-owned sessions never share one, which
    /// is what lets switching tabs or opening another scene invalidate the
    /// shared preview.
    document_revision: u64,
}

impl EditorSession {
    /// Creates an editor session around a semantic graph and optional graph
    /// view.
    pub fn new(graph: Graph, graph_view: Option<GraphView>) -> Self {
        let domain = EditorGraphDomain::for_graph(&graph);
        Self {
            graph,
            graph_view,
            diagnostics: Vec::new(),
            domain,
            undo_stack: UndoStack::new(),
            current_document: CurrentDocument::None,
            scene_session: None,
            untitled_dirty: false,
            clean_snapshot: None,
            document_revision: next_document_revision(),
        }
    }

    /// Creates a minimal empty Behavior Tree editor session.
    pub fn empty_behavior_tree() -> Self {
        let graph = Graph::new(
            GraphId::generate(),
            BehaviorTreeDomain::new().graph_kind().clone(),
            "untitled_behavior_tree",
        );
        Self::new(graph, None)
    }

    /// Creates a minimal Animation Graph session with its required Entry node.
    ///
    /// The initial node is created through the same semantic and presentation
    /// command paths used by the visual editor. The resulting session has no
    /// document path and is clean so callers can persist it as a newly-created
    /// project asset without inheriting construction history.
    pub fn empty_animation_graph() -> Self {
        let domain = AnimationGraphDomain::new();
        let graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "untitled_animation_graph",
        );
        let mut session = Self::new(graph, None);
        session
            .add_animation_node(AnimationNodeInsertKind::Entry, Some(Vec2::new(0.0, 0.0)))
            .expect("new Animation Graph must accept its schema-defined Entry node");
        session.undo_stack.clear();
        session.untitled_dirty = false;
        session
    }

    /// Creates a session using the Behavior Tree reference example.
    ///
    /// # Errors
    ///
    /// Returns [`BehaviorTreeServiceError`] if the reference graph cannot be
    /// built, validated, compiled, or laid out.
    pub fn behavior_tree_example() -> Result<Self, BehaviorTreeServiceError> {
        let example = BehaviorTreeAuthoringService::new().example()?;
        let mut session = Self::new(example.graph, Some(example.view));
        session.diagnostics = example.diagnostics;
        Ok(session)
    }

    /// Returns the document currently held open by the session.
    pub fn current_document(&self) -> &CurrentDocument {
        &self.current_document
    }

    /// Returns a value that changes whenever the open document's content or
    /// identity changes (ADR 0072).
    ///
    /// The Scene View uses this to gate rebuilding its persistent preview
    /// world: an unchanged revision means the committed scene is byte-for-byte
    /// the one the preview world was built from, so no re-conversion is needed.
    /// The value is monotonic but not contiguous; only equality across frames
    /// is meaningful.
    pub fn document_revision(&self) -> u64 {
        self.document_revision
    }

    /// Bumps [`document_revision`] after a scene edit or document switch.
    ///
    /// [`document_revision`]: Self::document_revision
    pub(super) fn bump_document_revision(&mut self) {
        self.document_revision = next_document_revision();
    }

    /// Returns the primary path of the current document, when one is open.
    pub fn current_document_path(&self) -> Option<&Path> {
        match &self.current_document {
            CurrentDocument::None => None,
            CurrentDocument::Scene { path, .. } => Some(path.as_path()),
            CurrentDocument::Graph { graph_path, .. } => Some(graph_path.as_path()),
            CurrentDocument::Ui { path, .. } => Some(path.as_path()),
        }
    }

    /// Returns `true` when the current document has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.current_document.is_dirty() || self.untitled_dirty
    }

    /// Returns the semantic graph being edited.
    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// Returns the optional presentation graph view.
    pub fn graph_view(&self) -> Option<&GraphView> {
        self.graph_view.as_ref()
    }

    /// Returns diagnostics currently associated with the editor session.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Returns the current scene document, when a scene is open.
    pub fn scene(&self) -> Option<&AuthoringScene> {
        match &self.current_document {
            CurrentDocument::Scene { scene, .. } => Some(scene),
            CurrentDocument::None | CurrentDocument::Graph { .. } | CurrentDocument::Ui { .. } => {
                None
            }
        }
    }

    /// Returns the open declarative UI document.
    pub fn ui_document(&self) -> Option<&UiDocument> {
        match &self.current_document {
            CurrentDocument::Ui { document, .. } => Some(document),
            CurrentDocument::None
            | CurrentDocument::Scene { .. }
            | CurrentDocument::Graph { .. } => None,
        }
    }

    /// Returns one entity from the current scene document.
    pub fn scene_entity(&self, entity: &EntityId) -> Option<&AuthoringEntity> {
        self.scene()?.entity(entity)
    }

    /// Replaces the current diagnostics.
    pub fn set_diagnostics(&mut self, diagnostics: Vec<Diagnostic>) {
        self.diagnostics = diagnostics;
    }

    /// Appends one diagnostic to the current diagnostics.
    pub fn push_diagnostic(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Appends diagnostics to the current diagnostics.
    pub fn extend_diagnostics(&mut self, diagnostics: impl IntoIterator<Item = Diagnostic>) {
        self.diagnostics.extend(diagnostics);
    }

    /// Returns the currently selected node, when the presentation graph view
    /// contains exactly one selected node.
    pub fn selected_node(&self) -> Option<&NodeId> {
        let view = self.graph_view.as_ref()?;
        let mut nodes = view.selection.nodes.iter();
        let first = nodes.next()?;
        if nodes.next().is_none() {
            Some(first)
        } else {
            None
        }
    }

    /// Returns the currently selected edge when exactly one edge is selected.
    pub fn selected_edge(&self) -> Option<&EdgeId> {
        let view = self.graph_view.as_ref()?;
        let mut edges = view.selection.edges.iter();
        let first = edges.next()?;
        if edges.next().is_none() {
            Some(first)
        } else {
            None
        }
    }

    /// Returns whether the open semantic graph uses the Animation Graph domain.
    pub fn is_animation_graph(&self) -> bool {
        matches!(self.domain, EditorGraphDomain::Animation(_))
    }

    pub(super) fn apply_scene_command(&mut self, command: AuthoringCommand) -> Result<(), EditorSessionError> {
        self.apply_scene_commands(std::iter::once(command))
    }

    pub(crate) fn apply_scene_commands(
        &mut self,
        commands: impl IntoIterator<Item = AuthoringCommand>,
    ) -> Result<(), EditorSessionError> {
        let scene_session = self
            .scene_session
            .as_mut()
            .ok_or(EditorSessionError::NoSceneDocument)?;
        let mut transaction = scene_session.begin_transaction();
        let mut diagnostics = Vec::new();
        for command in commands {
            diagnostics.extend(transaction.apply(command).diagnostics);
        }

        match scene_session.commit(transaction) {
            Ok(_) => {
                self.sync_scene_document_from_session();
                self.mark_dirty();
                self.extend_diagnostics(diagnostics);
                Ok(())
            }
            Err(source) => {
                if let TransactionError::ValidationFailed {
                    diagnostics: commit_diagnostics,
                } = &source
                {
                    diagnostics.extend(commit_diagnostics.clone());
                }
                self.extend_diagnostics(diagnostics);
                Err(EditorSessionError::SceneTransaction { source })
            }
        }
    }

    pub(super) fn sync_scene_document_from_session(&mut self) {
        let Some(scene_session) = &self.scene_session else {
            return;
        };
        if let CurrentDocument::Scene { scene, .. } = &mut self.current_document {
            *scene = scene_session.scene().clone();
        }
        // This is the single funnel for every committed scene edit, undo, and
        // redo, so the preview-invalidation revision advances here (ADR 0072).
        self.bump_document_revision();
    }

    pub(super) fn is_scene_document(&self) -> bool {
        matches!(self.current_document, CurrentDocument::Scene { .. })
    }
}

impl Default for EditorSession {
    fn default() -> Self {
        Self::empty_behavior_tree()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::GraphKind;

    #[test]
    fn session_holds_graph_view_and_diagnostics_without_gui_types() {
        let graph = Graph::new(
            GraphId::generate(),
            GraphKind::new("test.graph"),
            "test_graph",
        );
        let view = GraphView::new(graph.id.clone());
        let mut session = EditorSession::new(graph, Some(view));

        session.set_diagnostics(vec![Diagnostic::warning(
            "editor.test_warning",
            "test warning",
        )]);

        assert_eq!(session.graph().name, "test_graph");
        assert!(session.graph_view().is_some());
        assert_eq!(session.diagnostics().len(), 1);
        assert_eq!(session.graph().nodes.len(), 0);
        assert_eq!(session.graph().edges.len(), 0);
    }

    #[test]
    fn behavior_tree_example_session_contains_graph_and_view() {
        let session =
            EditorSession::behavior_tree_example().expect("example session should be valid");

        assert_eq!(session.graph().kind.as_str(), "behavior_tree.graph");
        assert!(session.graph_view().is_some());
        assert!(!session.graph().nodes.is_empty());
        assert!(!session.graph().edges.is_empty());
    }

    #[test]
    fn document_revision_advances_on_edit_undo_redo_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();

        let before_open = session.document_revision();
        session.open_scene(path).expect("open_scene");
        let after_open = session.document_revision();
        assert_ne!(before_open, after_open, "opening a scene must invalidate");

        let id = session.create_scene_entity("new_entity").expect("create");
        let after_edit = session.document_revision();
        assert_ne!(after_open, after_edit, "a scene edit must invalidate");

        assert!(session.undo());
        let after_undo = session.document_revision();
        assert_ne!(after_edit, after_undo, "undo must invalidate");

        assert!(session.redo());
        let after_redo = session.document_revision();
        assert_ne!(after_undo, after_redo, "redo must invalidate");

        session.close_document_discarding_changes();
        assert_ne!(
            after_redo,
            session.document_revision(),
            "closing the document must invalidate"
        );
        let _ = id;
    }

    #[test]
    fn distinct_sessions_never_share_a_document_revision() {
        // Two tabs each own a session; the shared Scene View must be able to
        // tell their scenes apart even when both were just opened.
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.scene.json");
        let path_b = dir.path().join("b.scene.json");
        std::fs::write(&path_a, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        std::fs::write(&path_b, r#"{"schema_version":1,"entities":[]}"#).unwrap();

        let mut session_a = EditorSession::empty_behavior_tree();
        let mut session_b = EditorSession::empty_behavior_tree();
        session_a.open_scene(path_a).expect("open a");
        session_b.open_scene(path_b).expect("open b");

        assert_ne!(
            session_a.document_revision(),
            session_b.document_revision(),
            "freshly opened scenes in different tabs must not collide"
        );
    }

    #[test]
    fn document_revision_is_stable_without_document_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");

        let revision = session.document_revision();
        // Read-only queries must not advance the preview-invalidation revision.
        let _ = session.scene();
        let _ = session.can_undo();
        let _ = session.is_dirty();
        assert_eq!(
            revision,
            session.document_revision(),
            "queries that do not change the document must not invalidate the preview"
        );
    }
}
