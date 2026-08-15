//! Toolkit-independent editor session state.

use crate::document::{
    derive_view_path, open_graph_from_path, open_scene_from_path, open_ui_from_path,
    CurrentDocument, OpenDocumentError,
};
use crate::geometry::{fallback_position_for_index, fallback_position_for_node};
use engine_authoring::{
    animation_graph_motion_slots, motion_slots_annotation_value, replace_file_contents,
    AnimationGraphDomain, AuthoringCommand, AuthoringEntity, AuthoringScene, AuthoringSession,
    BehaviorTreeAuthoringService, BehaviorTreeDomain, BehaviorTreeServiceError, ComponentTypeId,
    Diagnostic, Edge, EdgeId, EntityId, Graph, GraphCommand, GraphDomain, GraphId, GraphSaveError,
    GraphSchemaRegistry, GraphTransaction, GraphTransactionError, GraphTransactionValidationError,
    GraphView, GraphViewCommand, GraphViewSaveError, GraphViewTransaction,
    GraphViewTransactionError, MotionSlot, MotionSlotId, Node, NodeId, NodeLayout, PersistError,
    PortRef, PropertyPath, PropertyPathSegment, SceneSaveError, Selection, TransactionError,
    UiDocument, UiDocumentCommand, UiDocumentEditError, UiDocumentTransaction, Value, Vec2,
    MOTION_SLOTS_ANNOTATION,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    fmt, io,
    path::{Path, PathBuf},
};

// ── Undo/Redo ────────────────────────────────────────────────────────────────

const UNDO_LIMIT: usize = 100;
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

/// Cartesian axis used by repeated scene-authoring operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneAxis {
    /// Horizontal X axis.
    X,
    /// Vertical Y axis.
    Y,
    /// Depth Z axis.
    Z,
}

/// Reference coordinate used when aligning selected entity origins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneAlignment {
    /// Smallest coordinate in the selection.
    Minimum,
    /// Midpoint between the smallest and largest coordinates.
    Center,
    /// Largest coordinate in the selection.
    Maximum,
}

/// One snapshot entry in the session undo/redo history.
///
/// Both documents are serialized to JSON. See ADR 0018.
#[derive(Clone, PartialEq, Eq)]
enum UndoEntry {
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
enum CleanSnapshot {
    Scene(u64),
    Document(UndoEntry),
}

/// Session-local snapshot-based undo/redo stack.
///
/// History is in-memory only and does not survive process restarts.
/// See ADR 0018.
struct UndoStack {
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
}

impl UndoStack {
    fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Pushes `entry` as the most recent undo point and clears redo history.
    fn push(&mut self, entry: UndoEntry) {
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

    fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

// ── EditorSession ─────────────────────────────────────────────────────────────

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

    // ── Accessors ─────────────────────────────────────────────────────────

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
    fn bump_document_revision(&mut self) {
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

    /// Returns the node kinds that the current graph domain can create.
    ///
    /// Animation Graphs offer Entry only when one is missing, preserving the
    /// domain rule that exactly one Entry node owns the initial-state edge.
    pub fn available_graph_node_kinds(&self) -> Vec<GraphNodeInsertKind> {
        match &self.domain {
            EditorGraphDomain::BehaviorTree(_) => vec![
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Root),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Sequence),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Selector),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Condition),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Action),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Decorator),
            ],
            EditorGraphDomain::Animation(domain) => {
                let has_entry = self
                    .graph
                    .nodes
                    .values()
                    .any(|node| node.node_type == *domain.entry_type());
                let mut kinds = Vec::with_capacity(2);
                if !has_entry {
                    kinds.push(GraphNodeInsertKind::Animation(
                        AnimationNodeInsertKind::Entry,
                    ));
                }
                kinds.push(GraphNodeInsertKind::Animation(
                    AnimationNodeInsertKind::State,
                ));
                kinds
            }
        }
    }

    // ── Undo/Redo ─────────────────────────────────────────────────────────

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

    // ── Command application ────────────────────────────────────────────────

    /// Applies one semantic graph command and records current domain diagnostics.
    ///
    /// This method commits structurally valid semantic graph edits even when
    /// the resulting graph is temporarily invalid for its selected domain.
    /// This is required for interactive editing where a newly-created node may
    /// be configured or connected later.
    ///
    /// This method does not push an undo checkpoint. Callers that expose
    /// user-visible operations are responsible for checkpointing before calling
    /// this method.
    pub fn apply_graph_command(&mut self, command: GraphCommand) -> Result<(), EditorSessionError> {
        self.apply_graph_commands(std::iter::once(command))
    }

    /// Applies semantic graph commands as one private graph transaction.
    ///
    /// Callers use this for compound semantic edits such as replacing an edge
    /// while preserving its stable ID. Undo checkpointing remains the caller's
    /// responsibility so one user gesture produces one history entry.
    fn apply_graph_commands(
        &mut self,
        commands: impl IntoIterator<Item = GraphCommand>,
    ) -> Result<(), EditorSessionError> {
        let mut transaction = GraphTransaction::begin(&self.graph);
        for command in commands {
            transaction.apply(command);
        }
        let commit = transaction
            .commit_private(self.domain.schema_registry())
            .map_err(|source| EditorSessionError::GraphTransactionValidation { source })?;
        self.graph = commit.graph;
        self.diagnostics = commit.diagnostics;
        self.diagnostics
            .extend(self.domain.validate_domain(&self.graph));
        self.prune_graph_view();
        self.mark_dirty();
        Ok(())
    }

    /// Applies one presentation graph view command.
    ///
    /// A missing graph view is created before applying the command. The command
    /// is still validated through `GraphViewTransaction`.
    ///
    /// This method does not push an undo checkpoint.
    pub fn apply_graph_view_command(
        &mut self,
        command: GraphViewCommand,
    ) -> Result<(), EditorSessionError> {
        self.apply_graph_view_commands(std::iter::once(command))
    }

    /// Applies presentation graph view commands as one transaction.
    ///
    /// `GraphViewTransaction::commit` validates the whole presentation
    /// document rather than only the commands it received, so repairs that
    /// depend on each other must share a transaction. Removing a stale node
    /// layout while the selection still names that node, for example, cannot
    /// commit on its own.
    ///
    /// This method does not push an undo checkpoint.
    fn apply_graph_view_commands(
        &mut self,
        commands: impl IntoIterator<Item = GraphViewCommand>,
    ) -> Result<(), EditorSessionError> {
        self.ensure_graph_view();
        let view = self
            .graph_view
            .as_mut()
            .expect("graph view must exist after ensure_graph_view");
        let mut transaction = GraphViewTransaction::begin(view);
        for command in commands {
            transaction.apply(command);
        }
        transaction
            .commit(view, &self.graph)
            .map_err(|source| EditorSessionError::GraphViewTransaction { source })?;
        Ok(())
    }

    /// Drops presentation state whose semantic counterpart no longer exists.
    ///
    /// Presentation validation rejects a graph view that names a missing node,
    /// edge, or group, and it inspects the whole document on every commit. A
    /// view left holding a deleted identifier therefore blocks every later
    /// graph view command, including plain selection, until the document is
    /// reopened. Running this repair wherever the semantic graph is replaced
    /// keeps the view committable instead of letting one deletion strand it.
    fn prune_graph_view(&mut self) {
        let commands = self.graph_view_prune_commands();
        if commands.is_empty() {
            return;
        }
        if let Err(error) = self.apply_graph_view_commands(commands) {
            self.diagnostics.push(Diagnostic::warning(
                "editor.graph_view_prune_failed",
                format!("stale graph view presentation state could not be dropped: {error}"),
            ));
        }
    }

    /// Builds the commands that remove every graph view reference the semantic
    /// graph no longer defines.
    fn graph_view_prune_commands(&self) -> Vec<GraphViewCommand> {
        let Some(view) = self.graph_view.as_ref() else {
            return Vec::new();
        };
        let mut commands = Vec::new();
        for node in view.nodes.keys() {
            if !self.graph.nodes.contains_key(node) {
                commands.push(GraphViewCommand::RemoveNodeLayout { node: node.clone() });
            }
        }
        for group in view.groups.keys() {
            if !self.graph.groups.contains_key(group) {
                commands.push(GraphViewCommand::RemoveGroupLayout {
                    group: group.clone(),
                });
            }
        }
        let mut selection = view.selection.clone();
        selection
            .nodes
            .retain(|node| self.graph.nodes.contains_key(node));
        selection
            .edges
            .retain(|edge| self.graph.edges.contains_key(edge));
        selection
            .groups
            .retain(|group| self.graph.groups.contains_key(group));
        if selection != view.selection {
            commands.push(GraphViewCommand::SetSelection { selection });
        }
        commands
    }

    /// Selects one semantic node through `GraphViewCommand`.
    ///
    /// Selection is not undoable; it is a transient navigation state.
    pub fn select_node(&mut self, node: Option<NodeId>) -> Result<(), EditorSessionError> {
        let mut selection = Selection::new();
        if let Some(node) = node {
            selection.nodes.insert(node);
        }
        self.apply_graph_view_command(GraphViewCommand::SetSelection { selection })
    }

    /// Selects one semantic edge through [`GraphViewCommand`].
    ///
    /// Selection is presentation state and therefore does not enter semantic
    /// graph undo history.
    pub fn select_edge(&mut self, edge: Option<EdgeId>) -> Result<(), EditorSessionError> {
        let mut selection = Selection::new();
        if let Some(edge) = edge {
            selection.edges.insert(edge);
        }
        self.apply_graph_view_command(GraphViewCommand::SetSelection { selection })
    }

    /// Updates one node layout through `GraphViewCommand`.
    ///
    /// This method does not push an undo checkpoint.
    pub fn set_node_layout(
        &mut self,
        node: NodeId,
        layout: NodeLayout,
    ) -> Result<(), EditorSessionError> {
        self.apply_graph_view_command(GraphViewCommand::SetNodeLayout { node, layout })?;
        self.mark_dirty();
        Ok(())
    }

    /// Moves one node through `GraphViewCommand` while preserving collapsed,
    /// pinned, and annotation state.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn move_node(&mut self, node: NodeId, position: Vec2) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        let mut layout = self
            .graph_view
            .as_ref()
            .and_then(|view| view.nodes.get(&node))
            .cloned()
            .unwrap_or_else(|| NodeLayout::new(position));
        layout.position = position;
        self.set_node_layout(node, layout)
    }

    /// Sets one node's pin state through `GraphViewCommand`.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn set_node_pinned(
        &mut self,
        node: NodeId,
        pinned: bool,
    ) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        let mut layout = self
            .graph_view
            .as_ref()
            .and_then(|view| view.nodes.get(&node))
            .cloned()
            .unwrap_or_else(|| NodeLayout::new(fallback_position_for_node(&self.graph, &node)));
        layout.pinned = pinned;
        self.set_node_layout(node, layout)
    }

    /// Replaces a node property value through `GraphCommand`.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn set_node_property(
        &mut self,
        node: NodeId,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetNodeProperty { node, value })
    }

    /// Replaces one node's optional human-readable name through the semantic
    /// graph command boundary.
    pub fn set_node_name(
        &mut self,
        node: NodeId,
        name: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        let name = name.into().trim().to_owned();
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetNodeName {
            node,
            name: (!name.is_empty()).then_some(name),
        })
    }

    /// Adds a Behavior Tree node through `GraphCommand`.
    ///
    /// Semantic commit is performed first. Presentation placement and selection
    /// are applied afterwards; their failures produce warnings rather than
    /// errors so that the semantic node is never silently discarded.
    ///
    /// Pushes one undo checkpoint before any mutation. The checkpoint covers
    /// the entire compound operation (semantic + presentation).
    pub fn add_behavior_node(
        &mut self,
        kind: BehaviorNodeInsertKind,
        behavior: impl Into<String>,
        position: Option<Vec2>,
    ) -> Result<NodeId, EditorSessionError> {
        let node_id = NodeId::generate();
        let node = self.make_behavior_node(node_id.clone(), kind, behavior.into())?;
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddNode { node })?;

        if let Some(position) = position {
            // Use set_node_layout directly so move_node does not push a
            // second checkpoint for this compound operation.
            let layout = NodeLayout::new(position);
            if let Err(error) = self.set_node_layout(node_id.clone(), layout) {
                self.diagnostics.push(Diagnostic::warning(
                    "editor.presentation_after_semantic_failed",
                    format!("semantic node was added but presentation placement failed: {error}"),
                ));
            }
        }
        if let Err(error) = self.select_node(Some(node_id.clone())) {
            self.diagnostics.push(Diagnostic::warning(
                "editor.presentation_after_semantic_failed",
                format!("semantic node was added but selection failed: {error}"),
            ));
        }
        Ok(node_id)
    }

    /// Adds one schema-defined Animation Graph node.
    ///
    /// Entry and State nodes are constructed from the active Animation Graph
    /// domain so the editor never duplicates stable node type identifiers.
    /// Semantic creation commits before optional presentation placement, in
    /// accordance with ADR 0016.
    pub fn add_animation_node(
        &mut self,
        kind: AnimationNodeInsertKind,
        position: Option<Vec2>,
    ) -> Result<NodeId, EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "add Animation Graph node",
            });
        };
        let node_id = NodeId::generate();
        let properties = match kind {
            AnimationNodeInsertKind::Entry => Value::Object(BTreeMap::new()),
            AnimationNodeInsertKind::State => Value::Object(BTreeMap::from([(
                engine_authoring::ANIMATION_STATE_PLAYBACK_MODE_PROPERTY.to_owned(),
                Value::String(
                    engine_authoring::AnimationStatePlaybackMode::Loop
                        .persisted_name()
                        .to_owned(),
                ),
            )])),
        };
        let node_type = match kind {
            AnimationNodeInsertKind::Entry => domain.entry_type().clone(),
            AnimationNodeInsertKind::State => domain.state_type().clone(),
        };
        let node = Node::new(node_id.clone(), node_type, properties);

        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddNode { node })?;
        if let Some(position) = position
            && let Err(error) = self.set_node_layout(node_id.clone(), NodeLayout::new(position)) {
                self.diagnostics.push(Diagnostic::warning(
                    "editor.presentation_after_semantic_failed",
                    format!(
                        "semantic animation node was added but presentation placement failed: {error}"
                    ),
                ));
            }
        if let Err(error) = self.select_node(Some(node_id.clone())) {
            self.diagnostics.push(Diagnostic::warning(
                "editor.presentation_after_semantic_failed",
                format!("semantic animation node was added but selection failed: {error}"),
            ));
        }
        Ok(node_id)
    }

    /// Assigns or clears one Animation State's graph-owned Motion Slot.
    ///
    /// A chosen slot is always a complete value, so this commits on its own as
    /// a single undo step rather than waiting for a separate confirmation. Use
    /// [`EditorSession::set_node_name`] to rename the same State; the two edits
    /// are independent and undo separately.
    ///
    /// The State's `motion_name` label is dropped because the stable slot
    /// replaces it.
    ///
    /// # Errors
    ///
    /// Returns an error when the active document is not an Animation Graph,
    /// when `node` is not an Animation State, or when `motion_slot` is not
    /// owned by the active graph.
    pub fn set_animation_state_motion_slot(
        &mut self,
        node: NodeId,
        motion_slot: Option<MotionSlotId>,
    ) -> Result<(), EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph state",
            });
        };
        let Some(existing) = self.graph.nodes.get(&node) else {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "state node `{}` no longer exists",
                node.as_str()
            )));
        };
        if existing.node_type != *domain.state_type() {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph state",
            });
        }
        let mut properties = match existing.properties.clone() {
            Value::Object(properties) => properties,
            _ => BTreeMap::new(),
        };
        if let Some(slot) = motion_slot {
            let slots = self.motion_slots()?;
            if !slots.iter().any(|candidate| candidate.id == slot) {
                return Err(EditorSessionError::InvalidGraphConnection(format!(
                    "motion slot `{}` is not owned by the active Animation Graph",
                    slot.as_str()
                )));
            }
            properties.insert(
                "motion_slot".to_owned(),
                Value::String(slot.as_str().to_owned()),
            );
        } else {
            properties.remove("motion_slot");
        }
        properties.remove("motion_name");
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetNodeProperty {
            node,
            value: Value::Object(properties),
        })
    }

    /// Assigns one Animation State's clip completion behavior.
    ///
    /// [`engine_authoring::AnimationStatePlaybackMode::Loop`] and
    /// [`engine_authoring::AnimationStatePlaybackMode::Once`] are persisted
    /// directly on the State.
    ///
    /// # Errors
    ///
    /// Returns an error when the active document is not an Animation Graph or
    /// when `node` is not an Animation State.
    pub fn set_animation_state_playback_mode(
        &mut self,
        node: NodeId,
        playback_mode: engine_authoring::AnimationStatePlaybackMode,
    ) -> Result<(), EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph state playback mode",
            });
        };
        let Some(existing) = self.graph.nodes.get(&node) else {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "state node `{}` no longer exists",
                node.as_str()
            )));
        };
        if existing.node_type != *domain.state_type() {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph state playback mode",
            });
        }
        let mut properties = match existing.properties.clone() {
            Value::Object(properties) => properties,
            _ => BTreeMap::new(),
        };
        properties.insert(
            engine_authoring::ANIMATION_STATE_PLAYBACK_MODE_PROPERTY.to_owned(),
            Value::String(playback_mode.persisted_name().to_owned()),
        );
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetNodeProperty {
            node,
            value: Value::Object(properties),
        })
    }

    /// Returns the stable motion slots owned by the active Animation Graph.
    ///
    /// A graph without the slot annotation owns no slots and returns an empty
    /// list; slots are never derived from State properties.
    pub fn motion_slots(&self) -> Result<Vec<MotionSlot>, EditorSessionError> {
        if !self.is_animation_graph() {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "query Animation Graph motion slots",
            });
        }
        animation_graph_motion_slots(&self.graph)
            .map_err(EditorSessionError::InvalidGraphConnection)
    }

    /// Adds one graph-owned motion slot and returns its stable ID.
    pub fn add_motion_slot(
        &mut self,
        display_name: impl Into<String>,
    ) -> Result<MotionSlotId, EditorSessionError> {
        let display_name = display_name.into().trim().to_owned();
        if display_name.is_empty() {
            return Err(EditorSessionError::InvalidGraphConnection(
                "motion slot display name must not be blank".to_owned(),
            ));
        }
        let mut slots = self.motion_slots()?;
        if slots
            .iter()
            .any(|slot| slot.display_name.eq_ignore_ascii_case(&display_name))
        {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "motion slot display name `{display_name}` is already in use"
            )));
        }
        let id = MotionSlotId::generate();
        slots.push(MotionSlot {
            id: id.clone(),
            display_name,
        });
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetGraphAnnotation {
            key: MOTION_SLOTS_ANNOTATION.to_owned(),
            value: Some(motion_slots_annotation_value(&slots)),
        })?;
        Ok(id)
    }

    /// Renames one graph-owned motion slot without changing its stable ID.
    pub fn rename_motion_slot(
        &mut self,
        id: MotionSlotId,
        display_name: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        let display_name = display_name.into().trim().to_owned();
        if display_name.is_empty() {
            return Err(EditorSessionError::InvalidGraphConnection(
                "motion slot display name must not be blank".to_owned(),
            ));
        }
        let mut slots = self.motion_slots()?;
        if slots
            .iter()
            .any(|slot| slot.id != id && slot.display_name.eq_ignore_ascii_case(&display_name))
        {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "motion slot display name `{display_name}` is already in use"
            )));
        }
        let Some(slot) = slots.iter_mut().find(|slot| slot.id == id) else {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "motion slot `{}` is not owned by the active Animation Graph",
                id.as_str()
            )));
        };
        slot.display_name = display_name;
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetGraphAnnotation {
            key: MOTION_SLOTS_ANNOTATION.to_owned(),
            value: Some(motion_slots_annotation_value(&slots)),
        })
    }

    /// Returns State nodes that currently select `id`.
    pub fn states_using_motion_slot(&self, id: &MotionSlotId) -> Vec<(NodeId, String)> {
        self.graph
            .nodes
            .iter()
            .filter_map(|(node_id, node)| {
                if node.node_type.as_str() != "anim.state" {
                    return None;
                }
                let Value::Object(properties) = &node.properties else {
                    return None;
                };
                let uses_slot = matches!(
                    properties.get("motion_slot"),
                    Some(Value::String(value)) if value == id.as_str()
                );
                uses_slot.then(|| {
                    (
                        node_id.clone(),
                        node.name
                            .clone()
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or_else(|| node_id.as_str().to_owned()),
                    )
                })
            })
            .collect()
    }

    /// Deletes one graph-owned slot and clears every State that used it.
    ///
    /// The caller is responsible for confirming the affected States shown by
    /// [`Self::states_using_motion_slot`] before invoking this operation.
    pub fn delete_motion_slot(&mut self, id: MotionSlotId) -> Result<(), EditorSessionError> {
        let mut slots = self.motion_slots()?;
        let previous_len = slots.len();
        slots.retain(|slot| slot.id != id);
        if slots.len() == previous_len {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "motion slot `{}` is not owned by the active Animation Graph",
                id.as_str()
            )));
        }

        let affected = self
            .states_using_motion_slot(&id)
            .into_iter()
            .map(|(node, _)| node)
            .collect::<BTreeSet<_>>();
        let mut commands = vec![GraphCommand::SetGraphAnnotation {
            key: MOTION_SLOTS_ANNOTATION.to_owned(),
            value: Some(motion_slots_annotation_value(&slots)),
        }];
        for node_id in affected {
            let Some(node) = self.graph.nodes.get(&node_id) else {
                continue;
            };
            let Value::Object(mut properties) = node.properties.clone() else {
                continue;
            };
            properties.remove("motion_slot");
            properties.remove("motion_name");
            commands.push(GraphCommand::SetNodeProperty {
                node: node_id,
                value: Value::Object(properties),
            });
        }
        self.push_undo_checkpoint();
        self.apply_graph_commands(commands)
    }

    /// Adds a node offered by the active graph domain.
    ///
    /// `behavior` is used only for Behavior Tree action and condition nodes;
    /// Animation Graph nodes ignore it because their clip is edited through
    /// the typed state Inspector after creation.
    pub fn add_graph_node(
        &mut self,
        kind: GraphNodeInsertKind,
        behavior: impl Into<String>,
        position: Option<Vec2>,
    ) -> Result<NodeId, EditorSessionError> {
        match kind {
            GraphNodeInsertKind::Behavior(kind) => self.add_behavior_node(kind, behavior, position),
            GraphNodeInsertKind::Animation(kind) => self.add_animation_node(kind, position),
        }
    }

    /// Deletes a node through `GraphCommand`.
    ///
    /// The node's layout and any selection entry naming it, or naming an edge
    /// the deletion cascaded away, are dropped by the presentation repair that
    /// every semantic graph edit runs.
    ///
    /// Pushes one undo checkpoint before any mutation.
    pub fn delete_node(&mut self, node: NodeId) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::DeleteNode { node })
    }

    /// Deletes one semantic edge and clears presentation selection.
    ///
    /// This operation is used for both Behavior Tree child edges and Animation
    /// Graph transitions and records one undo checkpoint.
    pub fn delete_edge(&mut self, edge: EdgeId) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::DeleteEdge { edge })
    }

    /// Connects two Behavior Tree nodes through `GraphCommand`.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn connect_child(
        &mut self,
        parent: NodeId,
        child: NodeId,
    ) -> Result<EdgeId, EditorSessionError> {
        let EditorGraphDomain::BehaviorTree(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "connect Behavior Tree child",
            });
        };
        let order = self.next_child_order(&parent);
        let edge_id = EdgeId::generate();
        let edge = domain.child_edge(edge_id.clone(), parent, child, order);
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddEdge { edge })?;
        Ok(edge_id)
    }

    /// Connects two Animation Graph nodes with a directed transition edge.
    ///
    /// Entry may connect to State and State may connect to State. State to
    /// Entry and Entry to Entry are rejected before a graph command is issued,
    /// producing an actionable editor error instead of a low-level port
    /// diagnostic.
    pub fn connect_animation_transition(
        &mut self,
        source: NodeId,
        target: NodeId,
    ) -> Result<EdgeId, EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "connect Animation Graph transition",
            });
        };
        let source_node = self.graph.nodes.get(&source).ok_or_else(|| {
            EditorSessionError::InvalidGraphConnection(format!(
                "source node `{}` no longer exists",
                source.as_str()
            ))
        })?;
        let target_node = self.graph.nodes.get(&target).ok_or_else(|| {
            EditorSessionError::InvalidGraphConnection(format!(
                "target node `{}` no longer exists",
                target.as_str()
            ))
        })?;
        if target_node.node_type != *domain.state_type() {
            return Err(EditorSessionError::InvalidGraphConnection(
                "Animation Graph transitions must target a State node".to_owned(),
            ));
        }
        let source_is_entry = source_node.node_type == *domain.entry_type();
        if source_is_entry
            && self
                .graph
                .edges
                .values()
                .any(|edge| edge.from.node == source)
        {
            return Err(EditorSessionError::InvalidGraphConnection(
                "Entry already has an initial State; delete its existing connection first"
                    .to_owned(),
            ));
        }
        let source_port = if source_is_entry {
            domain.entry_out_port().clone()
        } else if source_node.node_type == *domain.state_type() {
            domain.state_out_port().clone()
        } else {
            return Err(EditorSessionError::InvalidGraphConnection(
                "Animation Graph transitions must start from Entry or State".to_owned(),
            ));
        };
        let edge_id = EdgeId::generate();
        let edge = Edge::new(
            edge_id.clone(),
            PortRef::new(source, source_port),
            PortRef::new(target, domain.state_in_port().clone()),
        );
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddEdge { edge })?;
        Ok(edge_id)
    }

    /// Connects nodes using the active graph domain's edge semantics.
    pub fn connect_nodes(
        &mut self,
        source: NodeId,
        target: NodeId,
    ) -> Result<EdgeId, EditorSessionError> {
        if self.is_animation_graph() {
            self.connect_animation_transition(source, target)
        } else {
            self.connect_child(source, target)
        }
    }

    /// Replaces one State-to-State transition's condition and fade override.
    ///
    /// A blank condition remains the domain's explicit unconditional form.
    /// `fade_duration == None` delegates to the Animation Controller default.
    /// The existing edge ID and unrelated annotations are preserved.
    pub fn set_animation_transition(
        &mut self,
        edge: EdgeId,
        condition: impl Into<String>,
        fade_duration: Option<f64>,
    ) -> Result<(), EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph transition",
            });
        };
        if fade_duration.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(EditorSessionError::InvalidAnimationTransition(
                "fade duration must be a finite non-negative number".to_owned(),
            ));
        }
        let existing = self.graph.edges.get(&edge).cloned().ok_or_else(|| {
            EditorSessionError::InvalidAnimationTransition(format!(
                "transition `{}` no longer exists",
                edge.as_str()
            ))
        })?;
        let source_is_state = self
            .graph
            .nodes
            .get(&existing.from.node)
            .is_some_and(|node| node.node_type == *domain.state_type());
        let target_is_state = self
            .graph
            .nodes
            .get(&existing.to.node)
            .is_some_and(|node| node.node_type == *domain.state_type());
        if !source_is_state || !target_is_state {
            return Err(EditorSessionError::InvalidAnimationTransition(
                "Entry connections do not have runtime transition conditions".to_owned(),
            ));
        }

        let mut replacement = existing.clone();
        let condition = condition.into().trim().to_owned();
        if condition.is_empty() {
            replacement.annotations.remove("condition");
        } else {
            replacement
                .annotations
                .insert("condition".to_owned(), Value::String(condition));
        }
        if let Some(fade_duration) = fade_duration {
            replacement
                .annotations
                .insert("fade_duration".to_owned(), Value::F64(fade_duration));
        } else {
            replacement.annotations.remove("fade_duration");
        }

        self.push_undo_checkpoint();
        self.apply_graph_commands([
            GraphCommand::DeleteEdge { edge },
            GraphCommand::AddEdge { edge: replacement },
        ])
    }

    /// Applies deterministic incremental layout through `GraphViewCommand`.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn apply_incremental_layout(&mut self) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        let candidate_view = if self.is_animation_graph() {
            fallback_graph_view(&self.graph)
        } else {
            let candidate = BehaviorTreeAuthoringService::new().layout(&self.graph);
            if let Some(candidate_view) = candidate.graph_view {
                self.extend_diagnostics(candidate.diagnostics);
                candidate_view
            } else {
                self.extend_diagnostics(candidate.diagnostics);
                self.push_diagnostic(Diagnostic::warning(
                    "editor.layout_fallback_used",
                    "domain layout did not produce a graph view; using deterministic editor fallback",
                ));
                fallback_graph_view(&self.graph)
            }
        };
        let current = self.graph_view.as_ref();
        let commands = incremental_layout_commands(&self.graph, current, &candidate_view);
        let changed = !commands.is_empty();
        self.apply_graph_view_commands(commands)?;
        if changed {
            self.mark_dirty();
        }
        Ok(())
    }

    /// Creates a new root entity in the current scene.
    pub fn create_scene_entity(
        &mut self,
        name: impl Into<String>,
    ) -> Result<EntityId, EditorSessionError> {
        let id = EntityId::generate();
        self.apply_scene_command(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name: name.into(),
            parent: None,
        })?;
        Ok(id)
    }

    /// Creates one entity and all preset metadata/components as one undo step.
    ///
    /// This is the shared authoring path used by editor creation templates.
    /// Callers provide concrete component values obtained from registered
    /// schemas, so the session does not acquire an engine dependency.
    pub fn create_scene_entity_with_components(
        &mut self,
        name: impl Into<String>,
        display_name: impl Into<String>,
        description: impl Into<String>,
        components: Vec<(ComponentTypeId, Value)>,
    ) -> Result<EntityId, EditorSessionError> {
        self.create_scene_entity_from_template(name, display_name, description, None, components)
    }

    /// Creates a complete entity template atomically, including hierarchy.
    pub fn create_scene_entity_from_template(
        &mut self,
        name: impl Into<String>,
        display_name: impl Into<String>,
        description: impl Into<String>,
        parent: Option<EntityId>,
        components: Vec<(ComponentTypeId, Value)>,
    ) -> Result<EntityId, EditorSessionError> {
        let id = EntityId::generate();
        let name = name.into();
        let display_name = display_name.into();
        let description = description.into();
        let mut commands = Vec::with_capacity(3 + components.len());
        commands.push(AuthoringCommand::CreateEntity {
            id: id.clone(),
            name,
            parent,
        });
        if !display_name.is_empty() {
            commands.push(AuthoringCommand::SetEntityDisplayName {
                entity: id.clone(),
                display_name,
            });
        }
        if !description.is_empty() {
            commands.push(AuthoringCommand::SetEntityDescription {
                entity: id.clone(),
                description,
            });
        }
        commands.extend(components.into_iter().map(|(component_type, value)| {
            AuthoringCommand::AddComponent {
                entity: id.clone(),
                component_type,
                value,
            }
        }));
        self.apply_scene_commands(commands)?;
        Ok(id)
    }

    /// Deletes an entity from the current scene.
    pub fn delete_scene_entity(&mut self, entity: EntityId) -> Result<(), EditorSessionError> {
        let mut commands = self.renderer_model_detach_commands(std::slice::from_ref(&entity));
        commands.push(AuthoringCommand::DeleteEntity { entity });
        self.apply_scene_commands(commands)
    }

    /// Deletes an entity and all descendants as one reversible transaction.
    pub fn delete_scene_entity_subtree(
        &mut self,
        entity: EntityId,
    ) -> Result<(), EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        if scene.entity(&entity).is_none() {
            return Err(EditorSessionError::NoSceneDocument);
        }
        let mut ordered = vec![entity];
        let mut index = 0;
        while index < ordered.len() {
            let parent = ordered[index].clone();
            ordered.extend(
                scene
                    .entities()
                    .filter(|(_, candidate)| candidate.parent.as_ref() == Some(&parent))
                    .map(|(id, _)| id.clone()),
            );
            index += 1;
        }
        ordered.reverse();
        let detach = self.renderer_model_detach_commands(&ordered);
        self.apply_scene_commands(
            detach.into_iter().chain(
                ordered
                    .into_iter()
                    .map(|entity| AuthoringCommand::DeleteEntity { entity }),
            ),
        )
    }

    /// Deletes multiple selected subtrees as one undoable transaction.
    pub fn delete_scene_entity_subtrees(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
    ) -> Result<(), EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        let requested = entities
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let mut ordered = Vec::new();
        for entity in &requested {
            if scene.entity(entity).is_none() {
                continue;
            }
            let has_selected_ancestor = std::iter::successors(
                scene.entity(entity).and_then(|item| item.parent.clone()),
                |parent| scene.entity(parent).and_then(|item| item.parent.clone()),
            )
            .any(|ancestor| requested.contains(&ancestor));
            if !has_selected_ancestor {
                ordered.push(entity.clone());
            }
        }
        let mut index = 0;
        while index < ordered.len() {
            let parent = ordered[index].clone();
            ordered.extend(
                scene
                    .entities()
                    .filter(|(_, candidate)| candidate.parent.as_ref() == Some(&parent))
                    .map(|(id, _)| id.clone()),
            );
            index += 1;
        }
        let mut seen = std::collections::BTreeSet::new();
        ordered.retain(|entity| seen.insert(entity.clone()));
        ordered.reverse();
        let detach = self.renderer_model_detach_commands(&ordered);
        self.apply_scene_commands(
            detach.into_iter().chain(
                ordered
                    .into_iter()
                    .map(|entity| AuthoringCommand::DeleteEntity { entity }),
            ),
        )
    }

    /// Clears renderer Model references whose target is being deleted.
    ///
    /// Renderers outside the deleted hierarchy remain valid scene entities in
    /// a recoverable unassigned state instead of retaining a dangling
    /// `EntityRef` that would block scene validation and saving.
    fn renderer_model_detach_commands(&self, deleted: &[EntityId]) -> Vec<AuthoringCommand> {
        let Some(scene) = self.scene() else {
            return Vec::new();
        };
        let renderer_type =
            ComponentTypeId::new(engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT);
        let mut commands = Vec::new();
        for (entity_id, entity) in scene.entities() {
            if deleted.contains(entity_id) {
                continue;
            }
            let Some(Value::Object(fields)) = entity.components.get(&renderer_type) else {
                continue;
            };
            let referenced = fields
                .get("model")
                .or_else(|| fields.get("rig_source"))
                .or_else(|| fields.get("skeleton"));
            if !matches!(referenced, Some(Value::EntityRef(model)) if deleted.contains(model)) {
                continue;
            }
            let mut fields = fields.clone();
            fields.remove("model");
            fields.remove("rig_source");
            fields.remove("skeleton");
            commands.push(AuthoringCommand::SetComponentValue {
                entity: entity_id.clone(),
                component_type: renderer_type.clone(),
                value: Value::Object(fields),
            });
        }
        commands
    }

    /// Returns each render part of `model` paired with the mesh it draws.
    ///
    /// Parts that carry no resolvable mesh are omitted: they cannot be
    /// matched against the source either way.
    pub fn model_render_parts(
        &self,
        model: &EntityId,
    ) -> Vec<(EntityId, engine_authoring::AssetId)> {
        let Some(scene) = self.scene() else {
            return Vec::new();
        };
        let renderer_type =
            ComponentTypeId::new(engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT);
        if !scene.entity(model).is_some_and(|entity| {
            entity.components.contains_key(&ComponentTypeId::new(
                engine::scene_bridge::SKINNED_MODEL_COMPONENT,
            ))
        }) {
            return Vec::new();
        }
        scene
            .entities()
            .filter_map(|(part, entity)| {
                let Value::Object(fields) = entity.components.get(&renderer_type)? else {
                    return None;
                };
                let referenced = fields
                    .get("model")
                    .or_else(|| fields.get("rig_source"))
                    .or_else(|| fields.get("skeleton"));
                if !matches!(referenced, Some(Value::EntityRef(owner)) if owner == model) {
                    return None;
                }
                match fields.get("mesh") {
                    Some(Value::AssetRef(mesh)) => Some((part.clone(), mesh.clone())),
                    _ => None,
                }
            })
            .collect()
    }

    /// Returns the Skeleton sub-asset a Skinned Model entity owns.
    pub fn model_skeleton(&self, model: &EntityId) -> Option<engine_authoring::AssetId> {
        let model_type = ComponentTypeId::new(engine::scene_bridge::SKINNED_MODEL_COMPONENT);
        let Some(Value::Object(fields)) = self
            .scene()?
            .entity(model)
            .and_then(|entity| entity.components.get(&model_type))
        else {
            return None;
        };
        match fields.get("skeleton") {
            Some(Value::AssetRef(skeleton)) => Some(skeleton.clone()),
            _ => None,
        }
    }

    /// Applies a model-part difference by adding one renderer that references
    /// `model` per missing mesh (ADR 0087 5).
    ///
    /// Stale parts are reported by [`engine::model_part_sync`] and
    /// deliberately left in place; only additive repair is automatic, so no
    /// authored part is ever lost to a resync.
    pub fn resync_model_render_parts(
        &mut self,
        model: EntityId,
        sync: &engine::ModelPartSync,
    ) -> Result<usize, EditorSessionError> {
        if sync.missing.is_empty() {
            return Ok(0);
        }
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        if !scene.entity(&model).is_some_and(|entity| {
            entity.components.contains_key(&ComponentTypeId::new(
                engine::scene_bridge::SKINNED_MODEL_COMPONENT,
            ))
        }) {
            return Err(EditorSessionError::NoSceneDocument);
        }

        let mut commands = Vec::new();
        for (mesh, materials) in &sync.missing {
            let part_id = EntityId::generate();
            commands.push(AuthoringCommand::CreateEntity {
                id: part_id.clone(),
                name: mesh.as_str().to_owned(),
                parent: Some(model.clone()),
            });
            commands.push(AuthoringCommand::AddComponent {
                entity: part_id.clone(),
                component_type: ComponentTypeId::new(
                    engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT,
                ),
                value: engine::skinned_render_part_value(mesh, materials, &model),
            });
        }
        let added = sync.missing.len();
        self.apply_scene_commands(commands)?;
        Ok(added)
    }

    /// Changes one entity's parent through the shared authoring command path.
    pub fn set_scene_entity_parent(
        &mut self,
        entity: EntityId,
        parent: Option<EntityId>,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityParent { entity, parent })
    }

    /// Sets one scene entity's search slug.
    pub fn set_scene_entity_name(
        &mut self,
        entity: EntityId,
        name: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityName {
            entity,
            name: name.into(),
        })
    }

    /// Enables or disables one scene entity (and its subtree) for runtime
    /// conversion.
    pub fn set_scene_entity_enabled(
        &mut self,
        entity: EntityId,
        enabled: bool,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityEnabled { entity, enabled })
    }

    /// Sets one scene entity's display label.
    pub fn set_scene_entity_display_name(
        &mut self,
        entity: EntityId,
        display_name: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityDisplayName {
            entity,
            display_name: display_name.into(),
        })
    }

    /// Sets one scene entity's description.
    pub fn set_scene_entity_description(
        &mut self,
        entity: EntityId,
        description: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetEntityDescription {
            entity,
            description: description.into(),
        })
    }

    /// Adds a component with an initial value to one scene entity.
    pub fn add_scene_component(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::AddComponent {
            entity,
            component_type,
            value,
        })
    }

    /// Removes one component through the reversible authoring command path.
    pub fn remove_scene_component(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
    ) -> Result<(), EditorSessionError> {
        let mut commands =
            if component_type.as_str() == engine::scene_bridge::SKINNED_MODEL_COMPONENT {
                self.renderer_model_detach_commands(std::slice::from_ref(&entity))
            } else {
                Vec::new()
            };
        commands.push(AuthoringCommand::RemoveComponent {
            entity,
            component_type,
        });
        self.apply_scene_commands(commands)
    }

    /// Replaces one component's complete value on a scene entity.
    pub fn set_scene_component_value(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetComponentValue {
            entity,
            component_type,
            value,
        })
    }

    /// Replaces one nested property inside a scene component value.
    pub fn set_scene_component_property(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        path: Vec<PropertyPathSegment>,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetProperty {
            target: PropertyPath {
                entity,
                component_type,
                segments: path,
            },
            value,
        })
    }

    /// Creates a new scene entity from a dropped mesh asset (Phase 32).
    ///
    /// The entity receives an `engine.transform` component at the origin and
    /// one unified `engine.static_mesh_renderer` referencing `asset_id`. The
    /// operation is routed through [`AuthoringCommand`] and is undoable.
    ///
    /// # Errors
    ///
    /// Returns an error when no scene document is open or when any command
    /// fails validation.
    pub fn create_entity_from_mesh_asset(
        &mut self,
        asset_id: engine_authoring::id::AssetId,
        parent: Option<EntityId>,
    ) -> Result<EntityId, EditorSessionError> {
        use std::collections::BTreeMap;
        let id = EntityId::generate();
        let transform_value = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));
        let white_material = engine_authoring::id::AssetId::from_stable_id(
            engine_authoring::StableId::new(engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID),
        )
        .expect("built-in white material ID must be valid");
        let renderer_value = Value::Object(BTreeMap::from([
            ("mesh".into(), Value::AssetRef(asset_id)),
            ("material".into(), Value::AssetRef(white_material)),
            ("material_slots".into(), Value::Array(Vec::new())),
        ]));
        self.apply_scene_commands([
            AuthoringCommand::CreateEntity {
                id: id.clone(),
                name: "mesh_entity".into(),
                parent,
            },
            AuthoringCommand::AddComponent {
                entity: id.clone(),
                component_type: ComponentTypeId::new("engine.transform"),
                value: transform_value,
            },
            AuthoringCommand::AddComponent {
                entity: id.clone(),
                component_type: ComponentTypeId::new(
                    engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
                ),
                value: renderer_value,
            },
        ])?;
        Ok(id)
    }

    /// Assigns a dropped asset to a component field on a scene entity (Phase 32).
    ///
    /// The field at `path_segments` inside `component_type` on `entity` is
    /// replaced with a typed [`Value::AssetRef`].
    ///
    /// # Errors
    ///
    /// Returns an error when no scene document is open or when any command
    /// fails validation.
    pub fn assign_asset_to_field(
        &mut self,
        entity: EntityId,
        component_type: ComponentTypeId,
        path_segments: Vec<PropertyPathSegment>,
        asset_id: engine_authoring::id::AssetId,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_command(AuthoringCommand::SetProperty {
            target: PropertyPath {
                entity,
                component_type,
                segments: path_segments,
            },
            value: Value::AssetRef(asset_id),
        })
    }

    /// Duplicates the scene entity with `src_id`, returning the new entity's ID.
    ///
    /// Creates a new entity with the same name (suffixed `_copy`), description,
    /// and all components copied verbatim with fresh IDs.
    pub fn duplicate_scene_entity(
        &mut self,
        src_id: &EntityId,
    ) -> Result<EntityId, EditorSessionError> {
        let (new_name, display_name, description, parent, components) = {
            let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
            let src = scene
                .entity(src_id)
                .ok_or(EditorSessionError::NoSceneDocument)?;
            let new_name = format!("{}_copy", src.name);
            let display_name = src.display_name.clone();
            let description = src.description.clone();
            let parent = src.parent.clone();
            let components: Vec<(ComponentTypeId, Value)> = src
                .components
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            (new_name, display_name, description, parent, components)
        };
        self.create_scene_entity_from_template(
            new_name,
            display_name,
            description,
            parent,
            components,
        )
    }

    /// Duplicates a complete selection as one validated transaction.
    ///
    /// Parent links between selected entities are remapped to their new IDs;
    /// links to non-selected parents remain unchanged. `offset` is added to
    /// every copied transform position.
    pub fn duplicate_scene_entities(
        &mut self,
        selected: impl IntoIterator<Item = EntityId>,
        offset: [f64; 3],
    ) -> Result<Vec<EntityId>, EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        let selected = selected
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        if selected.is_empty() {
            return Ok(Vec::new());
        }
        let mut ordered = selected.iter().cloned().collect::<Vec<_>>();
        ordered.sort_by_key(|entity| scene_entity_depth(scene, entity));
        let id_map = ordered
            .iter()
            .cloned()
            .map(|old| (old, EntityId::generate()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut commands = Vec::new();
        for old_id in &ordered {
            let source = scene
                .entity(old_id)
                .ok_or(EditorSessionError::NoSceneDocument)?;
            let new_id = id_map
                .get(old_id)
                .expect("every ordered source receives a generated ID")
                .clone();
            let parent = source
                .parent
                .as_ref()
                .and_then(|parent| id_map.get(parent).cloned())
                .or_else(|| source.parent.clone());
            commands.push(AuthoringCommand::CreateEntity {
                id: new_id.clone(),
                name: format!("{}_copy", source.name),
                parent,
            });
            if !source.display_name.is_empty() {
                commands.push(AuthoringCommand::SetEntityDisplayName {
                    entity: new_id.clone(),
                    display_name: source.display_name.clone(),
                });
            }
            if !source.description.is_empty() {
                commands.push(AuthoringCommand::SetEntityDescription {
                    entity: new_id.clone(),
                    description: source.description.clone(),
                });
            }
            if !source.enabled {
                commands.push(AuthoringCommand::SetEntityEnabled {
                    entity: new_id.clone(),
                    enabled: false,
                });
            }
            for (component_type, value) in &source.components {
                let value = if component_type.as_str() == TRANSFORM_COMPONENT_ID {
                    offset_transform(value, old_id, offset)?
                } else {
                    value.clone()
                };
                commands.push(AuthoringCommand::AddComponent {
                    entity: new_id.clone(),
                    component_type: component_type.clone(),
                    value: remap_copied_entity_refs(&value, &id_map),
                });
            }
        }
        let new_selection = ordered
            .iter()
            .filter_map(|old| id_map.get(old).cloned())
            .collect::<Vec<_>>();
        self.apply_scene_commands(commands)?;
        Ok(new_selection)
    }

    /// Pastes copied scene entities as one validated and undoable transaction.
    ///
    /// Every pasted entity receives a fresh stable ID. Parent links and nested
    /// [`Value::EntityRef`] values that point to another copied entity are
    /// remapped to the corresponding pasted ID. References outside the copied
    /// set are retained only when that entity still exists in the open scene.
    pub fn paste_scene_entities(
        &mut self,
        copied: &[AuthoringEntity],
    ) -> Result<Vec<EntityId>, EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        if copied.is_empty() {
            return Ok(Vec::new());
        }

        let copied_by_id = copied
            .iter()
            .map(|entity| (entity.id.clone(), entity))
            .collect::<BTreeMap<_, _>>();
        let mut ordered = copied.iter().collect::<Vec<_>>();
        ordered.sort_by_key(|entity| copied_entity_depth(&copied_by_id, &entity.id));
        let id_map = ordered
            .iter()
            .map(|entity| (entity.id.clone(), EntityId::generate()))
            .collect::<BTreeMap<_, _>>();

        let mut commands = Vec::new();
        for source in &ordered {
            let new_id = id_map
                .get(&source.id)
                .expect("every copied entity receives a generated ID")
                .clone();
            let parent = source.parent.as_ref().and_then(|parent| {
                id_map.get(parent).cloned().or_else(|| {
                    scene
                        .entity(parent)
                        .is_some()
                        .then(|| parent.clone())
                })
            });
            commands.push(AuthoringCommand::CreateEntity {
                id: new_id.clone(),
                name: format!("{}_paste", source.name),
                parent,
            });
            if !source.display_name.is_empty() {
                commands.push(AuthoringCommand::SetEntityDisplayName {
                    entity: new_id.clone(),
                    display_name: source.display_name.clone(),
                });
            }
            if !source.description.is_empty() {
                commands.push(AuthoringCommand::SetEntityDescription {
                    entity: new_id.clone(),
                    description: source.description.clone(),
                });
            }
            if !source.enabled {
                commands.push(AuthoringCommand::SetEntityEnabled {
                    entity: new_id.clone(),
                    enabled: false,
                });
            }
            commands.extend(source.components.iter().map(|(component_type, value)| {
                AuthoringCommand::AddComponent {
                    entity: new_id.clone(),
                    component_type: component_type.clone(),
                    value: remap_copied_entity_refs(value, &id_map),
                }
            }));
        }

        let pasted = ordered
            .iter()
            .filter_map(|source| id_map.get(&source.id).cloned())
            .collect::<Vec<_>>();
        self.apply_scene_commands(commands)?;
        Ok(pasted)
    }

    /// Adds one component to every selected entity atomically.
    pub fn add_scene_component_to_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        component_type: ComponentTypeId,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.apply_scene_commands(entities.into_iter().map(|entity| {
            AuthoringCommand::AddComponent {
                entity,
                component_type: component_type.clone(),
                value: value.clone(),
            }
        }))
    }

    /// Removes one component from every selected entity atomically.
    pub fn remove_scene_component_from_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        component_type: ComponentTypeId,
    ) -> Result<(), EditorSessionError> {
        let entities = entities.into_iter().collect::<Vec<_>>();
        let mut commands =
            if component_type.as_str() == engine::scene_bridge::SKINNED_MODEL_COMPONENT {
                self.renderer_model_detach_commands(&entities)
            } else {
                Vec::new()
            };
        commands.extend(
            entities
                .into_iter()
                .map(|entity| AuthoringCommand::RemoveComponent {
                    entity,
                    component_type: component_type.clone(),
                }),
        );
        self.apply_scene_commands(commands)
    }

    /// Adds a relative position offset to each selected transform atomically.
    pub fn offset_scene_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        offset: [f64; 3],
    ) -> Result<(), EditorSessionError> {
        let commands = self.transform_commands(entities, |_, position| {
            [
                position[0] + offset[0],
                position[1] + offset[1],
                position[2] + offset[2],
            ]
        })?;
        self.apply_scene_commands(commands)
    }

    /// Returns transform positions after validating every requested entity.
    pub fn scene_entity_positions(
        &self,
        entities: &[EntityId],
    ) -> Result<Vec<(EntityId, [f64; 3])>, EditorSessionError> {
        self.scene_positions(entities)
    }

    /// Sets selected position axes to absolute values in one transaction.
    /// `None` preserves the current value for that axis.
    pub fn set_scene_entity_positions(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        axes: [Option<f64>; 3],
    ) -> Result<(), EditorSessionError> {
        let commands = self.transform_commands(entities, |_, mut position| {
            for (index, value) in axes.into_iter().enumerate() {
                if let Some(value) = value {
                    position[index] = value;
                }
            }
            position
        })?;
        self.apply_scene_commands(commands)
    }

    /// Aligns selected entity origins on one axis as one undo step.
    pub fn align_scene_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        axis: SceneAxis,
        alignment: SceneAlignment,
    ) -> Result<(), EditorSessionError> {
        let entities = entities.into_iter().collect::<Vec<_>>();
        let positions = self.scene_positions(&entities)?;
        if positions.is_empty() {
            return Ok(());
        }
        let axis_index = scene_axis_index(axis);
        let minimum = positions
            .iter()
            .map(|(_, position)| position[axis_index])
            .fold(f64::INFINITY, f64::min);
        let maximum = positions
            .iter()
            .map(|(_, position)| position[axis_index])
            .fold(f64::NEG_INFINITY, f64::max);
        let target = match alignment {
            SceneAlignment::Minimum => minimum,
            SceneAlignment::Center => (minimum + maximum) * 0.5,
            SceneAlignment::Maximum => maximum,
        };
        let commands = self.transform_commands(entities, |_, mut position| {
            position[axis_index] = target;
            position
        })?;
        self.apply_scene_commands(commands)
    }

    /// Distributes selected entity origins evenly between the endpoints.
    pub fn distribute_scene_entities(
        &mut self,
        entities: impl IntoIterator<Item = EntityId>,
        axis: SceneAxis,
    ) -> Result<(), EditorSessionError> {
        let entities = entities.into_iter().collect::<Vec<_>>();
        let mut positions = self.scene_positions(&entities)?;
        if positions.len() < 3 {
            return Ok(());
        }
        let axis_index = scene_axis_index(axis);
        positions.sort_by(|left, right| left.1[axis_index].total_cmp(&right.1[axis_index]));
        let minimum = positions[0].1[axis_index];
        let maximum = positions[positions.len() - 1].1[axis_index];
        let denominator = (positions.len() - 1) as f64;
        let targets = positions
            .iter()
            .enumerate()
            .map(|(index, (entity, _))| {
                (
                    entity.clone(),
                    minimum + (maximum - minimum) * index as f64 / denominator,
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let commands = self.transform_commands(entities, |entity, mut position| {
            if let Some(target) = targets.get(entity) {
                position[axis_index] = *target;
            }
            position
        })?;
        self.apply_scene_commands(commands)
    }

    fn scene_positions(
        &self,
        entities: &[EntityId],
    ) -> Result<Vec<(EntityId, [f64; 3])>, EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        entities
            .iter()
            .map(|entity| {
                let value = scene
                    .entity(entity)
                    .and_then(|item| {
                        item.components
                            .get(&ComponentTypeId::new(TRANSFORM_COMPONENT_ID))
                    })
                    .ok_or(EditorSessionError::MissingTransform(entity.clone()))?;
                Ok((entity.clone(), transform_position(value, entity)?))
            })
            .collect()
    }

    fn transform_commands(
        &self,
        entities: impl IntoIterator<Item = EntityId>,
        update: impl Fn(&EntityId, [f64; 3]) -> [f64; 3],
    ) -> Result<Vec<AuthoringCommand>, EditorSessionError> {
        let scene = self.scene().ok_or(EditorSessionError::NoSceneDocument)?;
        entities
            .into_iter()
            .map(|entity| {
                let component_type = ComponentTypeId::new(TRANSFORM_COMPONENT_ID);
                let current = scene
                    .entity(&entity)
                    .and_then(|item| item.components.get(&component_type))
                    .ok_or_else(|| EditorSessionError::MissingTransform(entity.clone()))?;
                let position = transform_position(current, &entity)?;
                let next = set_transform_position(current, &entity, update(&entity, position))?;
                Ok(AuthoringCommand::SetComponentValue {
                    entity,
                    component_type,
                    value: next,
                })
            })
            .collect()
    }

    // ── Document open ─────────────────────────────────────────────────────

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

    // ── File persistence ──────────────────────────────────────────────────

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

    // ── Private helpers ────────────────────────────────────────────────────

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

    fn apply_scene_command(&mut self, command: AuthoringCommand) -> Result<(), EditorSessionError> {
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

    fn sync_scene_document_from_session(&mut self) {
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

    fn mark_dirty(&mut self) {
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

    fn mark_clean(&mut self) {
        self.record_clean_snapshot();
    }

    fn record_clean_snapshot(&mut self) {
        self.clean_snapshot = self.document_snapshot();
        self.current_document.mark_clean();
        self.untitled_dirty = false;
    }

    fn restore_newer_recovery(&mut self) {
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

    fn document_snapshot(&self) -> Option<CleanSnapshot> {
        match &self.current_document {
            CurrentDocument::None => None,
            CurrentDocument::Scene { scene, .. } => Some(CleanSnapshot::Scene(scene.revision())),
            CurrentDocument::Graph { .. } | CurrentDocument::Ui { .. } => {
                self.snapshot().map(CleanSnapshot::Document)
            }
        }
    }

    fn ensure_graph_view(&mut self) {
        if self.graph_view.is_none()
            || self
                .graph_view
                .as_ref()
                .is_some_and(|v| v.graph != self.graph.id)
        {
            self.graph_view = Some(GraphView::new(self.graph.id.clone()));
        }
    }

    fn is_scene_document(&self) -> bool {
        matches!(self.current_document, CurrentDocument::Scene { .. })
    }

    fn make_behavior_node(
        &self,
        id: NodeId,
        kind: BehaviorNodeInsertKind,
        behavior: String,
    ) -> Result<Node, EditorSessionError> {
        let EditorGraphDomain::BehaviorTree(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "add Behavior Tree node",
            });
        };
        Ok(match kind {
            BehaviorNodeInsertKind::Root => domain.root_node(id),
            BehaviorNodeInsertKind::Sequence => domain.sequence_node(id),
            BehaviorNodeInsertKind::Selector => domain.selector_node(id),
            BehaviorNodeInsertKind::Condition => domain.condition_node(id, behavior),
            BehaviorNodeInsertKind::Action => domain.action_node(id, behavior),
            BehaviorNodeInsertKind::Decorator => domain.decorator_node(id, behavior),
        })
    }

    fn next_child_order(&self, parent: &NodeId) -> u32 {
        self.graph
            .edges
            .values()
            .filter(|edge| &edge.from.node == parent)
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Serializes the current graph and view as one undo snapshot.
    ///
    /// Returns `None` if serialization fails, which is unexpected for
    /// structurally valid documents.
    fn snapshot(&self) -> Option<UndoEntry> {
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
    fn push_undo_checkpoint(&mut self) {
        if let Some(entry) = self.snapshot() {
            self.undo_stack.push(entry);
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

fn remove_recovery_file(source: &Path) {
    let recovery = recovery_path(source);
    if recovery.is_file() {
        let _ = std::fs::remove_file(recovery);
    }
}

/// Removes a recovery sidecar from a document's former location after a
/// successful Save As. The active destination is never removed here because
/// a failed write must leave its recovery data intact.
fn remove_previous_recovery(previous: Option<&Path>, destination: &Path) {
    if let Some(previous) = previous.filter(|previous| *previous != destination) {
        remove_recovery_file(previous);
    }
}

fn fallback_graph_view(graph: &Graph) -> GraphView {
    let mut view = GraphView::new(graph.id.clone());
    for (index, node_id) in graph.nodes.keys().enumerate() {
        view.nodes.insert(
            node_id.clone(),
            NodeLayout::new(fallback_position_for_index(index)),
        );
    }
    view
}

fn scene_entity_depth(scene: &AuthoringScene, entity: &EntityId) -> usize {
    std::iter::successors(
        scene.entity(entity).and_then(|item| item.parent.clone()),
        |parent| scene.entity(parent).and_then(|item| item.parent.clone()),
    )
    .take(scene.entity_count())
    .count()
}

fn copied_entity_depth(
    copied: &BTreeMap<EntityId, &AuthoringEntity>,
    entity: &EntityId,
) -> usize {
    std::iter::successors(
        copied.get(entity).and_then(|item| item.parent.clone()),
        |parent| copied.get(parent).and_then(|item| item.parent.clone()),
    )
    .take(copied.len())
    .count()
}

fn remap_copied_entity_refs(value: &Value, id_map: &BTreeMap<EntityId, EntityId>) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| remap_copied_entity_refs(value, id_map))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        remap_copied_entity_refs(value, id_map),
                    )
                })
                .collect(),
        ),
        Value::EntityRef(entity) => Value::EntityRef(
            id_map
                .get(entity)
                .cloned()
                .unwrap_or_else(|| entity.clone()),
        ),
        other => other.clone(),
    }
}

fn scene_axis_index(axis: SceneAxis) -> usize {
    match axis {
        SceneAxis::X => 0,
        SceneAxis::Y => 1,
        SceneAxis::Z => 2,
    }
}

fn transform_position(value: &Value, entity: &EntityId) -> Result<[f64; 3], EditorSessionError> {
    let Value::Object(fields) = value else {
        return Err(EditorSessionError::InvalidTransform(entity.clone()));
    };
    let number = |field: &str| match fields.get(field) {
        Some(Value::F64(value)) if value.is_finite() => Some(*value),
        Some(Value::I64(value)) => Some(*value as f64),
        Some(Value::U64(value)) => Some(*value as f64),
        _ => None,
    };
    Ok([
        number("x").ok_or_else(|| EditorSessionError::InvalidTransform(entity.clone()))?,
        number("y").ok_or_else(|| EditorSessionError::InvalidTransform(entity.clone()))?,
        number("z").ok_or_else(|| EditorSessionError::InvalidTransform(entity.clone()))?,
    ])
}

fn set_transform_position(
    value: &Value,
    entity: &EntityId,
    position: [f64; 3],
) -> Result<Value, EditorSessionError> {
    if position.iter().any(|coordinate| !coordinate.is_finite()) {
        return Err(EditorSessionError::InvalidTransform(entity.clone()));
    }
    let Value::Object(mut fields) = value.clone() else {
        return Err(EditorSessionError::InvalidTransform(entity.clone()));
    };
    fields.insert("x".to_owned(), Value::F64(position[0]));
    fields.insert("y".to_owned(), Value::F64(position[1]));
    fields.insert("z".to_owned(), Value::F64(position[2]));
    Ok(Value::Object(fields))
}

fn offset_transform(
    value: &Value,
    entity: &EntityId,
    offset: [f64; 3],
) -> Result<Value, EditorSessionError> {
    let position = transform_position(value, entity)?;
    set_transform_position(
        value,
        entity,
        [
            position[0] + offset[0],
            position[1] + offset[1],
            position[2] + offset[2],
        ],
    )
}

impl Default for EditorSession {
    fn default() -> Self {
        Self::empty_behavior_tree()
    }
}

/// Behavior Tree node variants available in the Phase 8-A prototype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorNodeInsertKind {
    /// Root node.
    Root,
    /// Sequence composite node.
    Sequence,
    /// Selector composite node.
    Selector,
    /// Condition leaf node.
    Condition,
    /// Action leaf node.
    Action,
    /// Decorator node.
    Decorator,
}

/// Animation state-machine node variants offered by the visual editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationNodeInsertKind {
    /// The unique graph entry point that selects the initial state.
    Entry,
    /// A playable state whose stable motion slot resolves through an Animation Set.
    State,
}

/// Domain-tagged node kind produced by the shared graph canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeInsertKind {
    /// A Behavior Tree node kind.
    Behavior(BehaviorNodeInsertKind),
    /// An Animation Graph node kind.
    Animation(AnimationNodeInsertKind),
}

impl GraphNodeInsertKind {
    /// Returns the concise human-facing node palette label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Behavior(BehaviorNodeInsertKind::Root) => "Root",
            Self::Behavior(BehaviorNodeInsertKind::Sequence) => "Sequence",
            Self::Behavior(BehaviorNodeInsertKind::Selector) => "Selector",
            Self::Behavior(BehaviorNodeInsertKind::Condition) => "Condition",
            Self::Behavior(BehaviorNodeInsertKind::Action) => "Action",
            Self::Behavior(BehaviorNodeInsertKind::Decorator) => "Decorator",
            Self::Animation(AnimationNodeInsertKind::Entry) => "Entry",
            Self::Animation(AnimationNodeInsertKind::State) => "State",
        }
    }
}

/// Editor session operation error.
#[derive(Debug)]
pub enum EditorSessionError {
    /// An operation required an open scene document.
    NoSceneDocument,
    /// An operation required an open declarative UI document.
    NoUiDocument,
    /// An operation was requested for a different concrete graph domain.
    WrongGraphDomain {
        /// Short description of the rejected operation.
        operation: &'static str,
    },
    /// A requested node-to-node connection is not valid for the active domain.
    InvalidGraphConnection(String),
    /// An Animation Graph transition edit contains invalid data or endpoints.
    InvalidAnimationTransition(String),
    /// A batch transform operation targeted an entity without a transform.
    MissingTransform(EntityId),
    /// An authored transform did not contain finite numeric position fields.
    InvalidTransform(EntityId),
    /// A UI authoring command was structurally invalid.
    UiEdit(UiDocumentEditError),
    /// Whole-document validation blocked a UI transaction commit.
    UiValidation {
        /// Stable user-facing summary retained after the transaction closes.
        message: String,
    },
    /// A scene transaction failed.
    SceneTransaction {
        /// Source transaction error.
        source: TransactionError,
    },
    /// A semantic graph transaction failed.
    GraphTransaction {
        /// Source transaction error.
        source: GraphTransactionError,
    },
    /// A semantic graph private transaction failed validation.
    GraphTransactionValidation {
        /// Source transaction validation error.
        source: GraphTransactionValidationError,
    },
    /// A graph view transaction failed.
    GraphViewTransaction {
        /// Source graph view transaction error.
        source: GraphViewTransactionError,
    },
    /// Domain diagnostics blocked an operation that requires a valid graph.
    Diagnostics,
}

impl fmt::Display for EditorSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSceneDocument => write!(formatter, "no scene document is open"),
            Self::NoUiDocument => write!(formatter, "no UI document is open"),
            Self::WrongGraphDomain { operation } => {
                write!(formatter, "cannot {operation} in the active graph domain")
            }
            Self::InvalidGraphConnection(message) => formatter.write_str(message),
            Self::InvalidAnimationTransition(message) => formatter.write_str(message),
            Self::MissingTransform(entity) => {
                write!(
                    formatter,
                    "entity `{entity}` has no engine.transform component"
                )
            }
            Self::InvalidTransform(entity) => {
                write!(
                    formatter,
                    "entity `{entity}` has an invalid transform position"
                )
            }
            Self::UiEdit(source) => write!(formatter, "{source}"),
            Self::UiValidation { message } => write!(formatter, "{message}"),
            Self::SceneTransaction { source } => write!(formatter, "{source}"),
            Self::GraphTransaction { source } => write!(formatter, "{source}"),
            Self::GraphTransactionValidation { source } => write!(formatter, "{source}"),
            Self::GraphViewTransaction { source } => write!(formatter, "{source}"),
            Self::Diagnostics => write!(formatter, "operation blocked by diagnostics"),
        }
    }
}

impl std::error::Error for EditorSessionError {}

/// Error returned by [`EditorSession::save`] and [`EditorSession::save_as`].
#[derive(Debug)]
pub enum EditorPersistError {
    /// No document path is currently associated with the session.
    NoDocument,
    /// Graph save requires a `.graph.json` path.
    InvalidGraphPath {
        /// Path rejected by the graph save pipeline.
        path: PathBuf,
    },
    /// A graph view exists but the document has no view path.
    MissingGraphViewPath,
    /// Scene serialization or validation failed.
    SceneSave(SceneSaveError),
    /// Semantic graph serialization or validation failed.
    GraphSave(GraphSaveError),
    /// Graph view serialization or validation failed.
    GraphViewSave(GraphViewSaveError),
    /// UI document serialization failed.
    UiSerialize(serde_json::Error),
    /// UI validation produced blocking diagnostics.
    InvalidUiDocument {
        /// Diagnostics that must be resolved before saving.
        diagnostics: Vec<Diagnostic>,
    },
    /// File write failed.
    Persist(PersistError),
}

impl fmt::Display for EditorPersistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDocument => write!(formatter, "no document path is available; use Save As"),
            Self::InvalidGraphPath { path } => write!(
                formatter,
                "graph documents must be saved as .graph.json files: {}",
                path.display()
            ),
            Self::MissingGraphViewPath => {
                write!(
                    formatter,
                    "graph view exists but no .graph.view.json path is set"
                )
            }
            Self::SceneSave(source) => write!(formatter, "{source}"),
            Self::GraphSave(source) => write!(formatter, "{source}"),
            Self::GraphViewSave(source) => write!(formatter, "{source}"),
            Self::UiSerialize(source) => write!(formatter, "UI serialization failed: {source}"),
            Self::InvalidUiDocument { diagnostics } => write!(
                formatter,
                "UI document has {} blocking diagnostic(s)",
                diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.severity == engine_authoring::Severity::Error)
                    .count()
            ),
            Self::Persist(source) => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for EditorPersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SceneSave(source) => Some(source),
            Self::GraphSave(source) => Some(source),
            Self::GraphViewSave(source) => Some(source),
            Self::UiSerialize(source) => Some(source),
            Self::Persist(source) => Some(source),
            Self::NoDocument
            | Self::InvalidGraphPath { .. }
            | Self::MissingGraphViewPath
            | Self::InvalidUiDocument { .. } => None,
        }
    }
}

/// Error returned by [`EditorSession::load_from_path`].
#[derive(Debug)]
pub enum EditorLoadError {
    /// File read failed.
    Io(io::Error),
    /// JSON parse or deserialization failed.
    Json(serde_json::Error),
    /// A required top-level field is missing or has the wrong type.
    MissingField(&'static str),
    /// The file was written by a newer or incompatible editor version.
    UnsupportedVersion(u64),
}

impl fmt::Display for EditorLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "file read failed: {source}"),
            Self::Json(source) => write!(formatter, "JSON error: {source}"),
            Self::MissingField(name) => write!(formatter, "missing field: {name}"),
            Self::UnsupportedVersion(v) => {
                write!(formatter, "unsupported format_version {v}; expected 1")
            }
        }
    }
}

impl std::error::Error for EditorLoadError {}

fn incremental_layout_commands(
    graph: &Graph,
    current: Option<&GraphView>,
    candidate: &GraphView,
) -> Vec<GraphViewCommand> {
    let mut commands = Vec::new();
    if let Some(current) = current {
        for node in current.nodes.keys() {
            if !graph.nodes.contains_key(node) {
                commands.push(GraphViewCommand::RemoveNodeLayout { node: node.clone() });
            }
        }
    }

    for (index, node_id) in graph.nodes.keys().enumerate() {
        let candidate_layout = candidate
            .nodes
            .get(node_id)
            .cloned()
            .unwrap_or_else(|| NodeLayout::new(fallback_position_for_index(index)));
        let mut layout = candidate_layout;
        if let Some(existing) = current.and_then(|view| view.nodes.get(node_id)) {
            if existing.pinned {
                layout.position = existing.position;
                layout.pinned = true;
            }
            layout.collapsed = existing.collapsed;
            layout.annotations = existing.annotations.clone();
        }
        commands.push(GraphViewCommand::SetNodeLayout {
            node: node_id.clone(),
            layout,
        });
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{Diagnostic, GraphKind};
    use std::collections::BTreeMap;

    fn set_dirty_graph_document(session: &mut EditorSession) {
        session.current_document = CurrentDocument::Graph {
            graph_path: PathBuf::from("dirty.graph.json"),
            view_path: Some(PathBuf::from("dirty.graph.view.json")),
            is_dirty: false,
        };
        session.current_document.mark_dirty();
    }

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
    fn empty_animation_graph_contains_one_entry_and_view_layout() {
        let session = EditorSession::empty_animation_graph();

        assert!(session.is_animation_graph());
        assert_eq!(session.graph().kind.as_str(), "anim.graph");
        let entries = session
            .graph()
            .nodes
            .values()
            .filter(|node| node.node_type.as_str() == "anim.entry")
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(session
            .graph_view()
            .is_some_and(|view| view.nodes.contains_key(&entries[0].id)));
        assert_eq!(
            session.available_graph_node_kinds(),
            vec![GraphNodeInsertKind::Animation(
                AnimationNodeInsertKind::State
            )]
        );
    }

    /// Reproduces the graph view that a node deletion used to strand.
    ///
    /// Presentation validation inspects the whole document, so a view left
    /// naming the deleted node rejected every later command — clicking any
    /// node or transition reported a blocked transaction until the document
    /// was reopened.
    #[test]
    fn deleting_a_selected_state_leaves_the_graph_view_committable() {
        let mut session = EditorSession::empty_animation_graph();
        let entry = session
            .graph()
            .nodes
            .values()
            .find(|node| node.node_type.as_str() == "anim.entry")
            .expect("new graph must have Entry")
            .id
            .clone();
        let kept = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(220.0, 0.0)))
            .expect("kept State should be added");
        let removed = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(440.0, 0.0)))
            .expect("removed State should be added");
        session
            .connect_animation_transition(entry, kept.clone())
            .expect("Entry should connect to a State");
        session
            .connect_animation_transition(kept.clone(), removed.clone())
            .expect("State should connect to State");
        session
            .select_node(Some(removed.clone()))
            .expect("selecting a live State must succeed");

        session
            .delete_node(removed.clone())
            .expect("deleting a State must succeed");

        let view = session.graph_view().expect("graph view must exist");
        assert!(
            !view.nodes.contains_key(&removed),
            "deleted State kept its layout"
        );
        assert!(
            view.selection.nodes.is_empty(),
            "deleted State stayed selected"
        );
        assert!(
            view.selection.edges.is_empty(),
            "cascaded transition stayed selected"
        );
        session
            .select_node(Some(kept))
            .expect("selection must still commit after a deletion");
    }

    /// A transition selected when its endpoint is deleted must not block the
    /// view either; `DeleteNode` cascades incident edges away.
    #[test]
    fn deleting_a_state_clears_a_selected_cascaded_transition() {
        let mut session = EditorSession::empty_animation_graph();
        let entry = session
            .graph()
            .nodes
            .values()
            .find(|node| node.node_type.as_str() == "anim.entry")
            .expect("new graph must have Entry")
            .id
            .clone();
        let kept = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(220.0, 0.0)))
            .expect("kept State should be added");
        let removed = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(440.0, 0.0)))
            .expect("removed State should be added");
        session
            .connect_animation_transition(entry, kept.clone())
            .expect("Entry should connect to a State");
        let transition = session
            .connect_animation_transition(kept.clone(), removed.clone())
            .expect("State should connect to State");
        session
            .select_edge(Some(transition))
            .expect("selecting a live transition must succeed");

        session
            .delete_node(removed)
            .expect("deleting a State must succeed");

        assert!(session
            .graph_view()
            .expect("graph view must exist")
            .selection
            .edges
            .is_empty());
        session
            .select_node(Some(kept))
            .expect("selection must still commit after a cascaded deletion");
    }

    /// A view file written before the repair existed still names deleted
    /// nodes. Opening it must recover instead of leaving the document
    /// permanently unselectable.
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
    fn animation_transition_annotations_are_command_backed_and_undoable() {
        let mut session = EditorSession::empty_animation_graph();
        let entry = session
            .graph()
            .nodes
            .values()
            .find(|node| node.node_type.as_str() == "anim.entry")
            .expect("new graph must have Entry")
            .id
            .clone();
        let walk = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(220.0, 0.0)))
            .expect("walk State should be added");
        let run = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(440.0, 0.0)))
            .expect("run State should be added");

        session
            .connect_animation_transition(entry, walk.clone())
            .expect("Entry should connect to a State");
        let transition = session
            .connect_animation_transition(walk, run)
            .expect("State should connect to State");
        session
            .set_animation_transition(transition.clone(), "is_running", Some(0.15))
            .expect("typed transition settings should apply");

        let annotations = &session.graph().edges[&transition].annotations;
        assert_eq!(
            annotations.get("condition"),
            Some(&Value::String("is_running".to_owned()))
        );
        assert_eq!(annotations.get("fade_duration"), Some(&Value::F64(0.15)));

        assert!(
            session.undo(),
            "transition edit should create one undo step"
        );
        assert!(session.graph().edges[&transition].annotations.is_empty());
    }

    #[test]
    fn animation_state_name_and_graph_owned_slot_undo_independently() {
        let mut session = EditorSession::empty_animation_graph();
        let motion_slot = session
            .add_motion_slot("Walk")
            .expect("Motion Slot should be added");
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("State should be added");

        session
            .set_node_name(state.clone(), "Walk")
            .expect("state name should apply");
        session
            .set_animation_state_motion_slot(state.clone(), Some(motion_slot.clone()))
            .expect("state slot should apply");

        assert_eq!(session.graph().nodes[&state].name.as_deref(), Some("Walk"));
        assert_eq!(
            session.graph().nodes[&state].properties,
            Value::Object(BTreeMap::from([
                (
                    "motion_slot".to_owned(),
                    Value::String(motion_slot.as_str().to_owned()),
                ),
                ("playback_mode".to_owned(), Value::String("loop".to_owned()),),
            ]))
        );

        assert!(session.undo(), "slot edit should be its own undo step");
        assert_eq!(
            session.graph().nodes[&state].properties,
            Value::Object(BTreeMap::from([(
                "playback_mode".to_owned(),
                Value::String("loop".to_owned()),
            )]))
        );
        assert_eq!(
            session.graph().nodes[&state].name.as_deref(),
            Some("Walk"),
            "undoing the slot must not revert the separately committed name"
        );

        assert!(session.undo(), "name edit should be its own undo step");
        assert_eq!(session.graph().nodes[&state].name, None);
    }

    #[test]
    fn assigning_motion_slot_drops_the_state_motion_label() {
        let mut session = EditorSession::empty_animation_graph();
        let motion_slot = session
            .add_motion_slot("Idle")
            .expect("Motion Slot should be added");
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("State should be added");
        session
            .set_node_property(
                state.clone(),
                Value::Object(BTreeMap::from([(
                    "motion_name".to_owned(),
                    Value::String("Idle".to_owned()),
                )])),
            )
            .expect("state properties should be writable");

        session
            .set_animation_state_motion_slot(state.clone(), Some(motion_slot.clone()))
            .expect("state slot should apply");

        assert_eq!(
            session.graph().nodes[&state].properties,
            Value::Object(BTreeMap::from([(
                "motion_slot".to_owned(),
                Value::String(motion_slot.as_str().to_owned()),
            )]))
        );
    }

    #[test]
    fn motion_slot_not_owned_by_active_graph_is_rejected() {
        let mut session = EditorSession::empty_animation_graph();
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("State should be added");
        let foreign = MotionSlotId::generate();

        let error = session
            .set_animation_state_motion_slot(state.clone(), Some(foreign))
            .expect_err("a slot from another graph must be rejected");

        assert!(matches!(
            error,
            EditorSessionError::InvalidGraphConnection(_)
        ));
        assert_eq!(
            session.graph().nodes[&state].properties,
            Value::Object(BTreeMap::from([(
                "playback_mode".to_owned(),
                Value::String("loop".to_owned()),
            )])),
            "a rejected slot must not mutate the State"
        );
    }

    #[test]
    fn deleting_motion_slot_unassigns_using_states_reports_diagnostic_and_undo_restores_all() {
        let mut session = EditorSession::empty_animation_graph();
        let motion_slot = session
            .add_motion_slot("Idle")
            .expect("Motion Slot should be added");
        let first = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("first State should be added");
        let second = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("second State should be added");
        for (node, name) in [(first.clone(), "Idle A"), (second.clone(), "Idle B")] {
            session
                .set_node_name(node.clone(), name)
                .expect("State should be renamed");
            session
                .set_animation_state_motion_slot(node, Some(motion_slot.clone()))
                .expect("State should select graph slot");
        }

        let usages = session.states_using_motion_slot(&motion_slot);
        assert_eq!(usages.len(), 2);
        session
            .delete_motion_slot(motion_slot.clone())
            .expect("confirmed slot deletion should succeed");

        assert!(session
            .motion_slots()
            .expect("slot list should remain valid")
            .is_empty());
        for node in [&first, &second] {
            let Value::Object(properties) = &session.graph().nodes[node].properties else {
                panic!("State properties must remain an object");
            };
            assert!(!properties.contains_key("motion_slot"));
        }
        assert!(session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "anim.state_no_motion"));

        assert!(session.undo(), "slot deletion should be one undo step");
        assert_eq!(
            session
                .motion_slots()
                .expect("restored slot list should be valid")
                .len(),
            1
        );
        assert_eq!(session.states_using_motion_slot(&motion_slot).len(), 2);
    }

    #[test]
    fn selection_uses_graph_view_command_boundary() {
        let mut session =
            EditorSession::behavior_tree_example().expect("example session should be valid");
        let node = session
            .graph()
            .nodes
            .keys()
            .next()
            .expect("example has nodes")
            .clone();

        session
            .select_node(Some(node.clone()))
            .expect("selection should apply");

        assert_eq!(session.selected_node(), Some(&node));
        assert!(session
            .graph_view()
            .expect("selection creates or updates view")
            .selection
            .nodes
            .contains(&node));
    }

    #[test]
    fn semantic_property_edit_uses_graph_command_boundary() {
        let mut session =
            EditorSession::behavior_tree_example().expect("example session should be valid");
        let node = session
            .graph()
            .nodes
            .keys()
            .next()
            .expect("example has nodes")
            .clone();
        let value = Value::Object(BTreeMap::from([(
            "behavior".into(),
            Value::String("edited".into()),
        )]));

        session
            .set_node_property(node.clone(), value.clone())
            .expect("property edit should apply structurally");

        assert_eq!(session.graph().nodes[&node].properties, value);
    }

    #[test]
    fn dragging_updates_layout_but_preserves_pin_state() {
        let mut session =
            EditorSession::behavior_tree_example().expect("example session should be valid");
        let node = session
            .graph()
            .nodes
            .keys()
            .next()
            .expect("example has nodes")
            .clone();
        session
            .set_node_pinned(node.clone(), true)
            .expect("pin should apply");
        session
            .move_node(node.clone(), Vec2::new(42.0, 24.0))
            .expect("move should apply");

        let layout = &session.graph_view().expect("view exists").nodes[&node];
        assert!(layout.pinned);
        assert_eq!(layout.position, Vec2::new(42.0, 24.0));
    }

    #[test]
    fn pin_without_existing_layout_uses_node_fallback_position() {
        let mut session = EditorSession::empty_behavior_tree();
        let first = session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "first", None)
            .expect("first node should be added");
        let second = session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "second", None)
            .expect("second node should be added");

        session
            .set_node_pinned(second.clone(), true)
            .expect("pin should apply");

        let layout = &session.graph_view().expect("view exists").nodes[&second];
        assert!(layout.pinned);
        assert_eq!(
            layout.position,
            fallback_position_for_node(session.graph(), &second)
        );
        assert!(!session
            .graph_view()
            .expect("view exists")
            .nodes
            .contains_key(&first));
    }

    #[test]
    fn add_node_commits_semantic_before_presentation() {
        let mut session = EditorSession::empty_behavior_tree();
        let node = session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "new_action",
                Some(Vec2::new(10.0, 20.0)),
            )
            .expect("add node should apply structurally");

        assert!(session.graph().nodes.contains_key(&node));
        assert!(session
            .graph_view()
            .expect("view exists")
            .nodes
            .contains_key(&node));
    }

    #[test]
    fn connect_child_uses_graph_command_boundary() {
        let mut session = EditorSession::empty_behavior_tree();
        let parent = session
            .add_behavior_node(BehaviorNodeInsertKind::Root, "", Some(Vec2::new(0.0, 0.0)))
            .expect("root should be added");
        let child = session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "child",
                Some(Vec2::new(200.0, 0.0)),
            )
            .expect("child should be added");

        let edge = session
            .connect_child(parent, child)
            .expect("edge should be added");

        assert!(session.graph().edges.contains_key(&edge));
    }

    #[test]
    fn incremental_layout_uses_fallback_for_incomplete_graph() {
        let mut session = EditorSession::empty_behavior_tree();
        let node = session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "orphan_action",
                Some(Vec2::new(500.0, 500.0)),
            )
            .expect("orphan action should be structurally valid");

        session
            .apply_incremental_layout()
            .expect("fallback layout should apply through graph view commands");

        assert!(session
            .graph_view()
            .expect("view exists")
            .nodes
            .contains_key(&node));
        assert!(session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.layout_fallback_used"));
    }

    #[test]
    fn incremental_layout_preserves_existing_diagnostics() {
        let mut session = EditorSession::empty_behavior_tree();
        session.push_diagnostic(Diagnostic::warning(
            "editor.existing_warning",
            "existing warning",
        ));

        session
            .apply_incremental_layout()
            .expect("empty graph layout should apply");

        assert!(session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.existing_warning"));
    }

    // ── Undo/Redo tests ────────────────────────────────────────────────────

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
    fn add_behavior_node_with_position_is_single_undo_entry() {
        let mut session = EditorSession::empty_behavior_tree();
        session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "a",
                Some(Vec2::new(100.0, 50.0)),
            )
            .expect("add should succeed");

        session.undo();

        assert_eq!(
            session.graph().nodes.len(),
            0,
            "one undo must revert both semantic and presentation state"
        );
        assert!(
            !session.can_undo(),
            "undo stack must be empty after reverting the only operation"
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
    fn scene_entity_create_delete_and_undo_redo_use_authoring_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");

        let id = session
            .create_scene_entity("new_entity")
            .expect("entity create should succeed");

        assert!(session.scene().unwrap().entity(&id).is_some());
        assert!(session.is_dirty());
        assert!(session.can_undo());

        assert!(session.undo());
        assert!(session.scene().unwrap().entity(&id).is_none());
        assert!(session.can_redo());

        assert!(session.redo());
        assert!(session.scene().unwrap().entity(&id).is_some());

        session
            .delete_scene_entity(id.clone())
            .expect("leaf entity delete should succeed");
        assert!(session.scene().unwrap().entity(&id).is_none());
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

    #[test]
    fn scene_component_add_and_set_value_update_current_document_scene() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");
        let entity = session
            .create_scene_entity("new_entity")
            .expect("entity create should succeed");
        let component_type = ComponentTypeId::new("engine.transform");
        let value = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));

        session
            .add_scene_component(entity.clone(), component_type.clone(), value)
            .expect("component add should succeed");
        let edited = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(4.0)),
            ("y".into(), Value::F64(2.0)),
            ("z".into(), Value::F64(1.0)),
        ]));
        session
            .set_scene_component_value(entity.clone(), component_type.clone(), edited.clone())
            .expect("component set should succeed");

        assert_eq!(
            session.scene().unwrap().entity(&entity).unwrap().components[&component_type],
            edited
        );

        session
            .remove_scene_component(entity.clone(), component_type.clone())
            .expect("component remove should succeed");
        assert!(!session
            .scene()
            .unwrap()
            .entity(&entity)
            .unwrap()
            .components
            .contains_key(&component_type));
        assert!(session.undo(), "component removal must be undoable");
        assert_eq!(
            session.scene().unwrap().entity(&entity).unwrap().components[&component_type],
            edited
        );
    }

    #[test]
    fn scene_component_set_property_updates_current_document_scene() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");
        let entity = session
            .create_scene_entity("new_entity")
            .expect("entity create should succeed");
        let component_type = ComponentTypeId::new("engine.transform");
        let value = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));

        session
            .add_scene_component(entity.clone(), component_type.clone(), value)
            .expect("component add should succeed");
        session
            .set_scene_component_property(
                entity.clone(),
                component_type.clone(),
                vec![PropertyPathSegment::Field { name: "x".into() }],
                Value::F64(4.0),
            )
            .expect("component property set should succeed");

        let Value::Object(component) =
            &session.scene().unwrap().entity(&entity).unwrap().components[&component_type]
        else {
            panic!("component must remain an object");
        };
        assert_eq!(component.get("x"), Some(&Value::F64(4.0)));
        assert_eq!(component.get("y"), Some(&Value::F64(0.0)));
        assert_eq!(component.get("z"), Some(&Value::F64(0.0)));
    }

    #[test]
    fn delete_parent_entity_fails_when_it_has_children() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");
        let parent = session
            .create_scene_entity("parent")
            .expect("parent create");
        let child_id = EntityId::generate();
        session
            .apply_scene_command(AuthoringCommand::CreateEntity {
                id: child_id,
                name: "child".into(),
                parent: Some(parent.clone()),
            })
            .expect("child create must succeed");
        let result = session.delete_scene_entity(parent);
        assert!(
            result.is_err(),
            "deleting a parent entity with children must fail with entity.has_children"
        );
    }

    #[test]
    fn subtree_delete_is_atomic_and_undo_restores_every_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subtree.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");
        let parent = session.create_scene_entity("parent").expect("parent");
        let child = EntityId::generate();
        session
            .apply_scene_command(AuthoringCommand::CreateEntity {
                id: child.clone(),
                name: "child".into(),
                parent: Some(parent.clone()),
            })
            .expect("child");

        session
            .delete_scene_entity_subtree(parent.clone())
            .expect("subtree delete");
        assert!(session.scene_entity(&parent).is_none());
        assert!(session.scene_entity(&child).is_none());
        assert!(session.undo(), "subtree delete must be one undo step");
        assert!(session.scene_entity(&parent).is_some());
        assert!(session.scene_entity(&child).is_some());
    }

    #[test]
    fn removing_a_skinned_model_clears_surviving_renderer_references() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("skinned_model.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");
        let model = session.create_scene_entity("model").expect("model");
        let renderer = session.create_scene_entity("renderer").expect("renderer");
        let model_type = ComponentTypeId::new(engine::scene_bridge::SKINNED_MODEL_COMPONENT);
        let renderer_type =
            ComponentTypeId::new(engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT);
        session
            .add_scene_component(
                model.clone(),
                model_type.clone(),
                Value::Object(BTreeMap::new()),
            )
            .expect("model component");
        session
            .add_scene_component(
                renderer.clone(),
                renderer_type.clone(),
                Value::Object(BTreeMap::from([(
                    "model".to_owned(),
                    Value::EntityRef(model.clone()),
                )])),
            )
            .expect("renderer component");

        session
            .remove_scene_component(model.clone(), model_type.clone())
            .expect("remove model and detach renderer");

        assert!(!session
            .scene_entity(&model)
            .expect("model entity remains")
            .components
            .contains_key(&model_type));
        let Value::Object(renderer_fields) = session
            .scene_entity(&renderer)
            .expect("renderer remains")
            .components
            .get(&renderer_type)
            .expect("renderer component remains")
        else {
            panic!("renderer component must remain an object");
        };
        assert!(!renderer_fields.contains_key("model"));
        assert!(session.undo(), "detach and removal must be one undo step");
        assert_eq!(
            session
                .scene_entity(&renderer)
                .and_then(|entity| entity.components.get(&renderer_type))
                .and_then(|value| match value {
                    Value::Object(fields) => fields.get("model"),
                    _ => None,
                }),
            Some(&Value::EntityRef(model))
        );
    }

    #[test]
    fn deleting_a_model_entity_deletes_its_generated_renderer_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete_skinned_model.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");
        let model = session.create_scene_entity("model").expect("model");
        let renderer = EntityId::generate();
        session
            .apply_scene_commands([
                AuthoringCommand::AddComponent {
                    entity: model.clone(),
                    component_type: ComponentTypeId::new(TRANSFORM_COMPONENT_ID),
                    value: Value::Object(BTreeMap::from([
                        ("x".to_owned(), Value::F64(3.0)),
                        ("y".to_owned(), Value::F64(0.0)),
                        ("z".to_owned(), Value::F64(0.0)),
                    ])),
                },
                AuthoringCommand::AddComponent {
                    entity: model.clone(),
                    component_type: ComponentTypeId::new(
                        engine::scene_bridge::SKINNED_MODEL_COMPONENT,
                    ),
                    value: Value::Object(BTreeMap::new()),
                },
                AuthoringCommand::CreateEntity {
                    id: renderer.clone(),
                    name: "renderer".to_owned(),
                    parent: Some(model.clone()),
                },
                AuthoringCommand::AddComponent {
                    entity: renderer.clone(),
                    component_type: ComponentTypeId::new(TRANSFORM_COMPONENT_ID),
                    value: Value::Object(BTreeMap::from([
                        ("x".to_owned(), Value::F64(2.0)),
                        ("y".to_owned(), Value::F64(0.0)),
                        ("z".to_owned(), Value::F64(0.0)),
                    ])),
                },
                AuthoringCommand::AddComponent {
                    entity: renderer.clone(),
                    component_type: ComponentTypeId::new(
                        engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT,
                    ),
                    value: Value::Object(BTreeMap::from([(
                        "model".to_owned(),
                        Value::EntityRef(model.clone()),
                    )])),
                },
            ])
            .expect("model hierarchy");

        session
            .delete_scene_entity_subtree(model.clone())
            .expect("delete model");

        assert!(session.scene_entity(&model).is_none());
        assert!(session.scene_entity(&renderer).is_none());
        assert!(session.undo(), "subtree deletion must be one undo step");
        assert_eq!(
            session
                .scene_entity(&renderer)
                .and_then(|entity| entity.parent.as_ref()),
            Some(&model)
        );
    }

    #[test]
    fn duplicate_is_atomic_and_preserves_metadata_and_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicate.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open scene");
        let parent = session.create_scene_entity("parent").expect("parent");
        let source = session
            .create_scene_entity_from_template(
                "source",
                "Display Name",
                "Description",
                Some(parent.clone()),
                vec![(ComponentTypeId::new("gameplay.health"), Value::I64(100))],
            )
            .expect("source");

        let duplicate = session.duplicate_scene_entity(&source).expect("duplicate");
        let entity = session.scene_entity(&duplicate).expect("duplicated entity");
        assert_eq!(entity.display_name, "Display Name");
        assert_eq!(entity.description, "Description");
        assert_eq!(entity.parent, Some(parent));
        assert_eq!(
            entity.components[&ComponentTypeId::new("gameplay.health")],
            Value::I64(100)
        );

        assert!(session.undo(), "duplicate must be one undo step");
        assert!(session.scene_entity(&duplicate).is_none());
        assert!(session.scene_entity(&source).is_some());
    }

    #[test]
    fn set_scene_component_property_undo_restores_previous_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");
        let entity = session
            .create_scene_entity("entity")
            .expect("entity create should succeed");
        let component_type = ComponentTypeId::new("engine.transform");
        let initial = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));
        session
            .add_scene_component(entity.clone(), component_type.clone(), initial)
            .expect("component add should succeed");

        session
            .set_scene_component_property(
                entity.clone(),
                component_type.clone(),
                vec![PropertyPathSegment::Field { name: "x".into() }],
                Value::F64(7.0),
            )
            .expect("property set should succeed");

        let Value::Object(after) =
            &session.scene().unwrap().entity(&entity).unwrap().components[&component_type]
        else {
            panic!("component must remain an object after set");
        };
        assert_eq!(after.get("x"), Some(&Value::F64(7.0)));

        assert!(session.undo(), "undo must return true after SetProperty");

        let Value::Object(restored) =
            &session.scene().unwrap().entity(&entity).unwrap().components[&component_type]
        else {
            panic!("component must remain an object after undo");
        };
        assert_eq!(
            restored.get("x"),
            Some(&Value::F64(0.0)),
            "undo must restore the original field value"
        );
        assert_eq!(restored.get("y"), Some(&Value::F64(0.0)));
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
    fn create_entity_from_mesh_asset_adds_unified_static_mesh_renderer() {
        use engine_authoring::id::AssetId;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");

        let stable = engine_authoring::StableId::new("asset_01JP0000000000000000000DDD");
        let asset_id = AssetId::from_stable_id(stable).expect("id must be valid");
        let entity = session
            .create_entity_from_mesh_asset(asset_id.clone(), None)
            .expect("create must succeed");

        let scene = session.scene().expect("scene must be open");
        assert!(scene.entity(&entity).is_some(), "entity must exist");
        let renderer_type =
            ComponentTypeId::new(engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT);
        assert!(
            scene
                .entity(&entity)
                .unwrap()
                .components
                .contains_key(&renderer_type),
            "entity must have a static mesh renderer"
        );
        let Value::Object(renderer) = scene
            .entity(&entity)
            .and_then(|entity| entity.components.get(&renderer_type))
            .expect("renderer value must exist")
        else {
            panic!("renderer value must be an object");
        };
        assert_eq!(
            renderer.get("mesh"),
            Some(&Value::AssetRef(asset_id)),
            "drop must store the mesh asset inside the renderer"
        );
        assert_eq!(
            renderer.get("material"),
            Some(&Value::AssetRef(
                engine_authoring::id::AssetId::from_stable_id(engine_authoring::StableId::new(
                    engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID,
                ),)
                .expect("built-in white material ID must be valid")
            )),
            "new renderers must start with the white fallback material"
        );
        assert!(session.undo(), "the complete drop must be one undo step");
        assert!(
            session
                .scene()
                .expect("scene remains open")
                .entity(&entity)
                .is_none(),
            "one undo must remove the complete dropped entity"
        );
    }

    #[test]
    fn create_entity_from_mesh_asset_as_child_sets_parent() {
        use engine_authoring::id::AssetId;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).expect("open_scene");

        let parent = session
            .create_scene_entity("parent_entity")
            .expect("parent create");
        let stable = engine_authoring::StableId::new("asset_01JP0000000000000000000EEE");
        let asset_id = AssetId::from_stable_id(stable).expect("id must be valid");

        let child = session
            .create_entity_from_mesh_asset(asset_id, Some(parent.clone()))
            .expect("child create must succeed");

        let scene = session.scene().expect("scene must be open");
        let child_entity = scene.entity(&child).expect("child must exist");
        assert_eq!(
            child_entity.parent,
            Some(parent),
            "child entity must reference the parent"
        );
    }

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

    #[test]
    fn scene_reparent_is_command_backed_cycle_safe_and_undoable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hierarchy.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).unwrap();
        let first = session.create_scene_entity("first").unwrap();
        let second = session.create_scene_entity("second").unwrap();

        session
            .set_scene_entity_parent(second.clone(), Some(first.clone()))
            .unwrap();
        assert_eq!(
            session
                .scene_entity(&second)
                .and_then(|item| item.parent.clone()),
            Some(first.clone())
        );
        assert!(session
            .set_scene_entity_parent(first.clone(), Some(second.clone()))
            .is_err());
        assert!(session.undo());
        assert_eq!(
            session
                .scene_entity(&second)
                .and_then(|item| item.parent.clone()),
            None
        );
    }

    fn transform_value(x: f64, y: f64, z: f64) -> Value {
        Value::Object(BTreeMap::from([
            ("x".to_owned(), Value::F64(x)),
            ("y".to_owned(), Value::F64(y)),
            ("z".to_owned(), Value::F64(z)),
        ]))
    }

    #[test]
    fn duplicate_selection_remaps_parent_and_is_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("duplicate.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).unwrap();
        let parent = session.create_scene_entity("parent").unwrap();
        let child = session.create_scene_entity("child").unwrap();
        session
            .set_scene_entity_parent(child.clone(), Some(parent.clone()))
            .unwrap();
        for (entity, x) in [(parent.clone(), 1.0), (child.clone(), 2.0)] {
            session
                .add_scene_component(
                    entity,
                    ComponentTypeId::new(TRANSFORM_COMPONENT_ID),
                    transform_value(x, 0.0, 0.0),
                )
                .unwrap();
        }

        let duplicated = session
            .duplicate_scene_entities([parent.clone(), child.clone()], [3.0, 0.0, 0.0])
            .unwrap();
        let duplicated_parent = duplicated[0].clone();
        let duplicated_child = duplicated[1].clone();
        assert_eq!(
            session.scene_entity(&duplicated_child).unwrap().parent,
            Some(duplicated_parent)
        );
        assert!(session.undo());
        assert!(duplicated
            .iter()
            .all(|id| session.scene_entity(id).is_none()));
        assert!(session.scene_entity(&parent).is_some());
        assert!(session.scene_entity(&child).is_some());
    }

    #[test]
    fn paste_selection_remaps_parent_and_nested_entity_references() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("paste.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).unwrap();
        let parent = session.create_scene_entity("parent").unwrap();
        let child = session.create_scene_entity("child").unwrap();
        session
            .set_scene_entity_parent(child.clone(), Some(parent.clone()))
            .unwrap();
        session
            .add_scene_component(
                parent.clone(),
                ComponentTypeId::new("game.target"),
                Value::Object(BTreeMap::from([(
                    "nested".to_owned(),
                    Value::Array(vec![Value::EntityRef(child.clone())]),
                )])),
            )
            .unwrap();
        session
            .set_scene_entity_enabled(child.clone(), false)
            .unwrap();
        let copied = [parent, child]
            .iter()
            .map(|id| session.scene_entity(id).unwrap().clone())
            .collect::<Vec<_>>();

        let pasted = session.paste_scene_entities(&copied).unwrap();
        let pasted_parent = pasted[0].clone();
        let pasted_child = pasted[1].clone();
        assert_eq!(
            session.scene_entity(&pasted_child).unwrap().parent,
            Some(pasted_parent.clone())
        );
        assert!(!session.scene_entity(&pasted_child).unwrap().enabled);
        assert_eq!(
            session.scene_entity(&pasted_parent).unwrap().components
                [&ComponentTypeId::new("game.target")],
            Value::Object(BTreeMap::from([(
                "nested".to_owned(),
                Value::Array(vec![Value::EntityRef(pasted_child)]),
            )]))
        );
        assert!(session.undo(), "multi-paste must be one undo step");
        assert!(pasted
            .iter()
            .all(|entity| session.scene_entity(entity).is_none()));
    }

    #[test]
    fn failed_batch_transform_does_not_modify_valid_targets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("atomic.scene.json");
        std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
        let mut session = EditorSession::empty_behavior_tree();
        session.open_scene(path).unwrap();
        let valid = session.create_scene_entity("valid").unwrap();
        let invalid = session.create_scene_entity("invalid").unwrap();
        session
            .add_scene_component(
                valid.clone(),
                ComponentTypeId::new(TRANSFORM_COMPONENT_ID),
                transform_value(4.0, 0.0, 0.0),
            )
            .unwrap();

        assert!(session
            .offset_scene_entities([valid.clone(), invalid], [10.0, 0.0, 0.0])
            .is_err());
        let value = session
            .scene_entity(&valid)
            .unwrap()
            .components
            .get(&ComponentTypeId::new(TRANSFORM_COMPONENT_ID))
            .unwrap();
        assert_eq!(transform_position(value, &valid).unwrap(), [4.0, 0.0, 0.0]);
    }

    #[test]
    fn animation_state_playback_mode_defaults_to_loop_and_is_undoable() {
        let mut session = EditorSession::empty_animation_graph();
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("State creation must succeed");

        let playback_property = |session: &EditorSession| {
            let Value::Object(properties) = &session
                .graph()
                .nodes
                .get(&state)
                .expect("State must exist")
                .properties
            else {
                panic!("Animation State properties must be an object");
            };
            properties
                .get(engine_authoring::ANIMATION_STATE_PLAYBACK_MODE_PROPERTY)
                .cloned()
        };

        assert_eq!(
            playback_property(&session),
            Some(Value::String("loop".to_owned()))
        );
        session
            .set_animation_state_playback_mode(
                state.clone(),
                engine_authoring::AnimationStatePlaybackMode::Once,
            )
            .expect("playback mode edit must succeed");
        assert_eq!(
            playback_property(&session),
            Some(Value::String("once".to_owned()))
        );

        assert!(session.undo(), "playback mode edit must be one undo step");
        assert_eq!(
            playback_property(&session),
            Some(Value::String("loop".to_owned()))
        );
    }
}
