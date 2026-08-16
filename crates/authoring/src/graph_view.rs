//! Presentation graph view authoring model.
//!
//! This module contains the Phase 3-B graph view foundation. It owns
//! presentation graph view storage, reference validation, graph view commands,
//! graph view changes, single-document transactions, and deterministic graph
//! view JSON serialization. It does not contain semantic graph mutation,
//! auto-layout execution, concrete graph domains, runtime execution, or
//! renderer state.

pub mod authoring_service;

use crate::diagnostic::{Diagnostic, DiagnosticTarget};
use crate::graph::{DottedIdError, Graph};
use crate::id::{EdgeId, GraphId, GroupId, NodeId};
use crate::value::Value;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

const CURRENT_GRAPH_VIEW_SCHEMA_VERSION: u32 = 1;

/// Presentation annotations attached to graph view objects.
///
/// Annotation keys are deterministic strings. Graph view annotations are
/// presentation/editor metadata, not semantic graph state.
pub type PresentationAnnotations = BTreeMap<String, Value>;

/// Identifies a stored presentation layout policy.
///
/// `LayoutPolicyId` is a validated dotted identifier such as
/// `layout.manual`. Phase 3-B stores and exposes this identifier only; it does
/// not perform behavior lookup or auto-layout execution.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LayoutPolicyId(String);

impl LayoutPolicyId {
    /// Creates a layout policy identifier from a known-valid dotted string.
    ///
    /// # Panics
    ///
    /// Panics when `value` is not a valid lowercase dotted identifier. Use
    /// [`try_new`](Self::try_new) for untrusted input.
    pub fn new(value: impl Into<String>) -> Self {
        Self::try_new(value).expect("layout policy identifier must be valid")
    }

    /// Creates a layout policy identifier from a lowercase dotted string.
    ///
    /// # Errors
    ///
    /// Returns [`DottedIdError`] when the value is empty, lacks a dot, contains
    /// an empty segment, or contains unsupported characters.
    pub fn try_new(value: impl Into<String>) -> Result<Self, DottedIdError> {
        let value = value.into();
        validate_dotted_id(&value)?;
        Ok(Self(value))
    }

    /// Returns the string representation of this identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LayoutPolicyId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl<'de> Deserialize<'de> for LayoutPolicyId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(de::Error::custom)
    }
}

fn validate_dotted_id(value: &str) -> Result<(), DottedIdError> {
    if value.is_empty() {
        return Err(DottedIdError::Empty);
    }
    if !value.contains('.') {
        return Err(DottedIdError::MissingNamespace);
    }
    for segment in value.split('.') {
        if segment.is_empty() {
            return Err(DottedIdError::EmptySegment);
        }
        let mut bytes = segment.bytes();
        let starts_with_lowercase = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase());
        let remaining_are_valid =
            bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        if !starts_with_lowercase || !remaining_are_valid {
            return Err(DottedIdError::InvalidSegment {
                segment: segment.to_owned(),
            });
        }
    }
    Ok(())
}

/// Two-dimensional presentation coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vec2 {
    /// Horizontal component.
    pub x: f64,
    /// Vertical component.
    pub y: f64,
}

impl Vec2 {
    /// Creates a two-dimensional value.
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// Presentation rectangle.
///
/// Group bounds use non-negative sizes. Zero-sized bounds are allowed so tools
/// can create a group before calculating a visible extent.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    /// Rectangle origin.
    pub position: Vec2,
    /// Rectangle size. Both components must be finite and non-negative.
    pub size: Vec2,
}

impl Rect {
    /// Creates a rectangle from an origin and size.
    pub fn new(position: Vec2, size: Vec2) -> Self {
        Self { position, size }
    }

    fn is_valid(self) -> bool {
        self.position.is_finite()
            && self.size.is_finite()
            && self.size.x >= 0.0
            && self.size.y >= 0.0
    }
}

/// Viewport presentation state.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    /// Pan offset in presentation coordinates.
    pub pan: Vec2,
    /// Zoom factor. Must be finite and greater than zero.
    pub zoom: f64,
}

impl Viewport {
    /// Creates a viewport.
    pub fn new(pan: Vec2, zoom: f64) -> Self {
        Self { pan, zoom }
    }

    fn is_valid(self) -> bool {
        self.pan.is_finite() && self.zoom.is_finite() && self.zoom > 0.0
    }
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            pan: Vec2::new(0.0, 0.0),
            zoom: 1.0,
        }
    }
}

/// Presentation layout for one graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeLayout {
    /// Node position in presentation coordinates.
    pub position: Vec2,
    /// Whether the node is visually collapsed.
    pub collapsed: bool,
    /// Whether layout tooling should avoid moving the node.
    pub pinned: bool,
    /// Presentation annotations.
    pub annotations: PresentationAnnotations,
}

impl NodeLayout {
    /// Creates a node layout at `position`.
    pub fn new(position: Vec2) -> Self {
        Self {
            position,
            collapsed: false,
            pinned: false,
            annotations: BTreeMap::new(),
        }
    }
}

/// Presentation layout for one semantic group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupLayout {
    /// Group bounds in presentation coordinates.
    pub bounds: Rect,
    /// Whether the group is visually collapsed.
    pub collapsed: bool,
    /// Presentation annotations.
    pub annotations: PresentationAnnotations,
}

impl GroupLayout {
    /// Creates a group layout with `bounds`.
    pub fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            collapsed: false,
            annotations: BTreeMap::new(),
        }
    }
}

/// Persisted presentation selection.
///
/// Selection is editor convenience state, not semantic graph state. Phase 3-B
/// stores selected nodes, edges, and groups only. Port selection is not part of
/// this model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// Selected semantic nodes.
    pub nodes: BTreeSet<NodeId>,
    /// Selected semantic edges.
    pub edges: BTreeSet<EdgeId>,
    /// Selected semantic groups.
    pub groups: BTreeSet<GroupId>,
}

impl Selection {
    /// Creates an empty selection.
    pub fn new() -> Self {
        Self {
            nodes: BTreeSet::new(),
            edges: BTreeSet::new(),
            groups: BTreeSet::new(),
        }
    }
}

impl Default for Selection {
    fn default() -> Self {
        Self::new()
    }
}

/// Optional presentation document for one semantic graph.
#[derive(Debug, Serialize)]
pub struct GraphView {
    /// Semantic graph this presentation document belongs to.
    pub graph: GraphId,
    /// Graph view schema version.
    pub schema_version: u32,
    /// Stored layout policy identifier. Phase 3-B does not execute it.
    pub layout_policy: LayoutPolicyId,
    /// Current viewport presentation state.
    pub viewport: Viewport,
    /// Node layouts keyed by semantic node ID.
    pub nodes: BTreeMap<NodeId, NodeLayout>,
    /// Group layouts keyed by semantic group ID.
    pub groups: BTreeMap<GroupId, GroupLayout>,
    /// Persisted presentation selection.
    pub selection: Selection,
    /// Presentation annotations for the graph view document.
    pub annotations: PresentationAnnotations,
    #[serde(skip_serializing)]
    identity: u64,
    #[serde(skip_serializing)]
    revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphViewDocument {
    graph: GraphId,
    schema_version: u32,
    layout_policy: LayoutPolicyId,
    viewport: Viewport,
    nodes: BTreeMap<NodeId, NodeLayout>,
    groups: BTreeMap<GroupId, GroupLayout>,
    selection: Selection,
    annotations: PresentationAnnotations,
}

impl<'de> Deserialize<'de> for GraphView {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = GraphViewDocument::deserialize(deserializer)?;
        if document.schema_version != CURRENT_GRAPH_VIEW_SCHEMA_VERSION {
            return Err(de::Error::custom(format!(
                "unsupported graph view schema_version {}, expected {}",
                document.schema_version, CURRENT_GRAPH_VIEW_SCHEMA_VERSION
            )));
        }
        Ok(Self {
            graph: document.graph,
            schema_version: document.schema_version,
            layout_policy: document.layout_policy,
            viewport: document.viewport,
            nodes: document.nodes,
            groups: document.groups,
            selection: document.selection,
            annotations: document.annotations,
            identity: next_graph_view_identity(),
            revision: 0,
        })
    }
}

impl GraphView {
    /// Creates an empty graph view for `graph`.
    pub fn new(graph: GraphId) -> Self {
        Self {
            graph,
            schema_version: CURRENT_GRAPH_VIEW_SCHEMA_VERSION,
            layout_policy: LayoutPolicyId::new("layout.manual"),
            viewport: Viewport::default(),
            nodes: BTreeMap::new(),
            groups: BTreeMap::new(),
            selection: Selection::new(),
            annotations: BTreeMap::new(),
            identity: next_graph_view_identity(),
            revision: 0,
        }
    }

    /// Returns structured presentation diagnostics for this graph view.
    pub fn validate(&self, graph: &Graph) -> Vec<Diagnostic> {
        validate_graph_view(self, graph)
    }

    /// Serializes this graph view to deterministic canonical pretty-printed
    /// JSON.
    ///
    /// This method validates the graph view against `graph` before
    /// serialization. It does not write files, mutate the semantic graph, or
    /// serialize semantic graph data.
    ///
    /// # Errors
    ///
    /// Returns an error when presentation validation fails or JSON
    /// serialization cannot complete.
    pub fn to_canonical_json(&self, graph: &Graph) -> Result<String, GraphViewSaveError> {
        let diagnostics = self.validate(graph);
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(GraphViewSaveError::ValidationFailed { diagnostics });
        }
        serde_json::to_string_pretty(self).map_err(GraphViewSaveError::Json)
    }

    fn identity(&self) -> u64 {
        self.identity
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn clone_for_transaction(&self) -> Self {
        Self {
            graph: self.graph.clone(),
            schema_version: self.schema_version,
            layout_policy: self.layout_policy.clone(),
            viewport: self.viewport,
            nodes: self.nodes.clone(),
            groups: self.groups.clone(),
            selection: self.selection.clone(),
            annotations: self.annotations.clone(),
            identity: self.identity,
            revision: self.revision,
        }
    }

    fn commit_from(&mut self, mut working: GraphView) {
        working.identity = self.identity;
        working.revision = self.revision.saturating_add(1);
        *self = working;
    }
}

/// Reports why a graph view could not be serialized.
#[derive(Debug)]
pub enum GraphViewSaveError {
    /// Graph view presentation data is invalid.
    ValidationFailed {
        /// All diagnostics produced by graph view validation.
        diagnostics: Vec<Diagnostic>,
    },
    /// JSON serialization failed.
    Json(serde_json::Error),
}

impl fmt::Display for GraphViewSaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed { diagnostics } => write!(
                formatter,
                "graph view cannot be saved because validation produced {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::Json(error) => write!(formatter, "graph view JSON serialization failed: {error}"),
        }
    }
}

impl std::error::Error for GraphViewSaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ValidationFailed { .. } => None,
            Self::Json(error) => Some(error),
        }
    }
}

fn next_graph_view_identity() -> u64 {
    static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);
    NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

fn validate_graph_view(view: &GraphView, graph: &Graph) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if view.graph != graph.id {
        diagnostics.push(
            Diagnostic::error(
                "graph_view.graph_mismatch",
                format!(
                    "graph view references graph `{}`, but validation graph is `{}`",
                    view.graph.as_str(),
                    graph.id.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Graph {
                id: view.graph.clone(),
            }),
        );
    }

    if !view.viewport.is_valid() {
        diagnostics.push(
            Diagnostic::error(
                "graph_view.invalid_viewport",
                "graph view viewport pan must be finite and zoom must be finite and greater than zero",
            )
            .with_target(DiagnosticTarget::Graph {
                id: view.graph.clone(),
            }),
        );
    }
    validate_annotations(
        &view.graph,
        "graph_view",
        &view.annotations,
        DiagnosticTarget::Graph {
            id: view.graph.clone(),
        },
        &mut diagnostics,
    );

    for (node_id, layout) in &view.nodes {
        if !graph.nodes.contains_key(node_id) {
            diagnostics.push(
                Diagnostic::error(
                    "graph_view.missing_node",
                    format!("graph view references missing node `{}`", node_id.as_str()),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: view.graph.clone(),
                    node: node_id.clone(),
                }),
            );
        }
        if !layout.position.is_finite() {
            diagnostics.push(
                Diagnostic::error(
                    "graph_view.invalid_node_position",
                    format!("node `{}` has a non-finite position", node_id.as_str()),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: view.graph.clone(),
                    node: node_id.clone(),
                }),
            );
        }
        validate_annotations(
            &view.graph,
            &format!("node `{}`", node_id.as_str()),
            &layout.annotations,
            DiagnosticTarget::Node {
                graph: view.graph.clone(),
                node: node_id.clone(),
            },
            &mut diagnostics,
        );
    }

    for (group_id, layout) in &view.groups {
        if !graph.groups.contains_key(group_id) {
            diagnostics.push(
                Diagnostic::error(
                    "graph_view.missing_group",
                    format!(
                        "graph view references missing group `{}`",
                        group_id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Group {
                    graph: view.graph.clone(),
                    group: group_id.clone(),
                }),
            );
        }
        if !layout.bounds.is_valid() {
            diagnostics.push(
                Diagnostic::error(
                    "graph_view.invalid_group_bounds",
                    format!("group `{}` has invalid bounds", group_id.as_str()),
                )
                .with_target(DiagnosticTarget::Group {
                    graph: view.graph.clone(),
                    group: group_id.clone(),
                }),
            );
        }
        validate_annotations(
            &view.graph,
            &format!("group `{}`", group_id.as_str()),
            &layout.annotations,
            DiagnosticTarget::Group {
                graph: view.graph.clone(),
                group: group_id.clone(),
            },
            &mut diagnostics,
        );
    }

    for node_id in &view.selection.nodes {
        if !graph.nodes.contains_key(node_id) {
            diagnostics.push(
                Diagnostic::error(
                    "graph_view.selection_missing_node",
                    format!(
                        "graph view selection references missing node `{}`",
                        node_id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: view.graph.clone(),
                    node: node_id.clone(),
                }),
            );
        }
    }
    for edge_id in &view.selection.edges {
        if !graph.edges.contains_key(edge_id) {
            diagnostics.push(
                Diagnostic::error(
                    "graph_view.selection_missing_edge",
                    format!(
                        "graph view selection references missing edge `{}`",
                        edge_id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Edge {
                    graph: view.graph.clone(),
                    edge: edge_id.clone(),
                }),
            );
        }
    }
    for group_id in &view.selection.groups {
        if !graph.groups.contains_key(group_id) {
            diagnostics.push(
                Diagnostic::error(
                    "graph_view.selection_missing_group",
                    format!(
                        "graph view selection references missing group `{}`",
                        group_id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Group {
                    graph: view.graph.clone(),
                    group: group_id.clone(),
                }),
            );
        }
    }

    diagnostics
}

fn validate_annotations(
    graph: &GraphId,
    owner: &str,
    annotations: &PresentationAnnotations,
    target: DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (key, value) in annotations {
        let path = format!("annotations.{key}");
        validate_annotation_value(graph, owner, &path, value, &target, diagnostics);
    }
}

fn validate_annotation_value(
    graph: &GraphId,
    owner: &str,
    path: &str,
    value: &Value,
    target: &DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match value {
        Value::F64(number) if !number.is_finite() => {
            diagnostics.push(
                Diagnostic::error(
                    "graph_view.non_finite_annotation_value",
                    format!(
                        "graph view `{}` annotation `{}` contains a non-finite number for {}",
                        graph.as_str(),
                        path,
                        owner
                    ),
                )
                .with_target(target.clone()),
            );
        }
        Value::Array(values) => {
            for (index, nested) in values.iter().enumerate() {
                validate_annotation_value(
                    graph,
                    owner,
                    &format!("{path}[{index}]"),
                    nested,
                    target,
                    diagnostics,
                );
            }
        }
        Value::Object(values) => {
            for (key, nested) in values {
                validate_annotation_value(
                    graph,
                    owner,
                    &format!("{path}.{key}"),
                    nested,
                    target,
                    diagnostics,
                );
            }
        }
        Value::Null
        | Value::Bool(_)
        | Value::I64(_)
        | Value::U64(_)
        | Value::F64(_)
        | Value::String(_)
        | Value::EntityRef(_)
        | Value::AssetRef(_) => {}
    }
}

/// A deterministic mutation of one graph view document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphViewCommand {
    /// Sets or replaces a node layout.
    SetNodeLayout {
        /// The semantic node whose layout is changed.
        node: NodeId,
        /// The new node layout.
        layout: NodeLayout,
    },
    /// Removes a node layout entry.
    RemoveNodeLayout {
        /// The semantic node whose layout is removed.
        node: NodeId,
    },
    /// Sets or replaces a group layout.
    SetGroupLayout {
        /// The semantic group whose layout is changed.
        group: GroupId,
        /// The new group layout.
        layout: GroupLayout,
    },
    /// Removes a group layout entry.
    RemoveGroupLayout {
        /// The semantic group whose layout is removed.
        group: GroupId,
    },
    /// Replaces the viewport.
    SetViewport {
        /// The new viewport.
        viewport: Viewport,
    },
    /// Replaces the persisted selection.
    SetSelection {
        /// The new selection.
        selection: Selection,
    },
    /// Replaces the stored layout policy identifier.
    SetLayoutPolicy {
        /// The new layout policy identifier.
        layout_policy: LayoutPolicyId,
    },
}

/// Graph view change record used for preview diffs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphViewChange {
    /// A node layout changed.
    NodeLayoutChanged {
        /// The affected node.
        node: NodeId,
        /// Previous layout.
        old_layout: Option<NodeLayout>,
        /// New layout.
        new_layout: NodeLayout,
    },
    /// A node layout was removed.
    NodeLayoutRemoved {
        /// The affected node.
        node: NodeId,
        /// Removed layout.
        old_layout: NodeLayout,
    },
    /// A group layout changed.
    GroupLayoutChanged {
        /// The affected group.
        group: GroupId,
        /// Previous layout.
        old_layout: Option<GroupLayout>,
        /// New layout.
        new_layout: GroupLayout,
    },
    /// A group layout was removed.
    GroupLayoutRemoved {
        /// The affected group.
        group: GroupId,
        /// Removed layout.
        old_layout: GroupLayout,
    },
    /// The viewport changed.
    ViewportChanged {
        /// Previous viewport.
        old_viewport: Viewport,
        /// New viewport.
        new_viewport: Viewport,
    },
    /// The selection changed.
    SelectionChanged {
        /// Previous selection.
        old_selection: Selection,
        /// New selection.
        new_selection: Selection,
    },
    /// The stored layout policy changed.
    LayoutPolicyChanged {
        /// Previous layout policy identifier.
        old_layout_policy: LayoutPolicyId,
        /// New layout policy identifier.
        new_layout_policy: LayoutPolicyId,
    },
}

/// Result of applying a graph view command.
#[derive(Debug)]
pub struct GraphViewCommandResult {
    /// Presentation graph view changes produced by this command.
    pub changes: Vec<GraphViewChange>,
    /// Diagnostics produced by this command.
    pub diagnostics: Vec<Diagnostic>,
    /// Optional inverse graph view command.
    pub inverse: Option<GraphViewCommand>,
}

impl GraphViewCommandResult {
    /// Returns `true` when this command produced no blocking diagnostics.
    pub fn is_ok(&self) -> bool {
        !self.diagnostics.iter().any(Diagnostic::is_blocking)
    }
}

/// Reports why a graph view transaction could not be committed.
#[derive(Debug)]
pub enum GraphViewTransactionError {
    /// One or more diagnostics blocked commit.
    ValidationFailed {
        /// Diagnostics accumulated during the transaction.
        diagnostics: Vec<Diagnostic>,
    },
    /// The destination graph view is not the same revision used to begin the
    /// transaction.
    Conflict {
        /// The identity of the graph view used to begin the transaction.
        expected_identity: u64,
        /// The revision used to begin the transaction.
        expected_revision: u64,
        /// The identity of the graph view supplied to commit.
        actual_identity: u64,
        /// The current revision of the graph view supplied to commit.
        actual_revision: u64,
    },
}

impl fmt::Display for GraphViewTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed { diagnostics } => write!(
                formatter,
                "graph view transaction blocked by {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::Conflict {
                expected_identity,
                expected_revision,
                actual_identity,
                actual_revision,
            } => write!(
                formatter,
                "graph view transaction began from graph view {expected_identity} revision {expected_revision}, but commit target is graph view {actual_identity} revision {actual_revision}"
            ),
        }
    }
}

impl std::error::Error for GraphViewTransactionError {}

/// Isolated transaction over one graph view presentation document.
pub struct GraphViewTransaction {
    working: GraphView,
    changes: Vec<GraphViewChange>,
    diagnostics: Vec<Diagnostic>,
    base_identity: u64,
    base_revision: u64,
}

impl GraphViewTransaction {
    /// Begins a new transaction over `view`.
    pub fn begin(view: &GraphView) -> Self {
        Self {
            working: view.clone_for_transaction(),
            changes: Vec::new(),
            diagnostics: Vec::new(),
            base_identity: view.identity(),
            base_revision: view.revision(),
        }
    }

    /// Applies `command` to the working graph view.
    pub fn apply(&mut self, command: GraphViewCommand) -> GraphViewCommandResult {
        match command {
            GraphViewCommand::SetNodeLayout { node, layout } => self.set_node_layout(node, layout),
            GraphViewCommand::RemoveNodeLayout { node } => self.remove_node_layout(node),
            GraphViewCommand::SetGroupLayout { group, layout } => {
                self.set_group_layout(group, layout)
            }
            GraphViewCommand::RemoveGroupLayout { group } => self.remove_group_layout(group),
            GraphViewCommand::SetViewport { viewport } => self.set_viewport(viewport),
            GraphViewCommand::SetSelection { selection } => self.set_selection(selection),
            GraphViewCommand::SetLayoutPolicy { layout_policy } => {
                self.set_layout_policy(layout_policy)
            }
        }
    }

    /// Returns the presentation changes accumulated by this transaction.
    pub fn preview_diff(&self) -> &[GraphViewChange] {
        &self.changes
    }

    /// Returns diagnostics accumulated by this transaction.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Commits this transaction to `view` if validation passes.
    ///
    /// # Errors
    ///
    /// Returns a validation error when any command or final presentation
    /// validation produces blocking diagnostics. Returns a conflict error when
    /// `view` is not the revision used to begin the transaction.
    pub fn commit(
        mut self,
        view: &mut GraphView,
        graph: &Graph,
    ) -> Result<Vec<GraphViewChange>, GraphViewTransactionError> {
        if view.identity() != self.base_identity || view.revision() != self.base_revision {
            return Err(GraphViewTransactionError::Conflict {
                expected_identity: self.base_identity,
                expected_revision: self.base_revision,
                actual_identity: view.identity(),
                actual_revision: view.revision(),
            });
        }
        self.diagnostics.extend(self.working.validate(graph));
        if self.diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(GraphViewTransactionError::ValidationFailed {
                diagnostics: self.diagnostics,
            });
        }
        view.commit_from(self.working);
        Ok(self.changes)
    }

    /// Discards this transaction without modifying the source graph view.
    pub fn rollback(self) {}

    fn set_node_layout(&mut self, node: NodeId, layout: NodeLayout) -> GraphViewCommandResult {
        let old_layout = self.working.nodes.insert(node.clone(), layout.clone());
        let change = GraphViewChange::NodeLayoutChanged {
            node: node.clone(),
            old_layout: old_layout.clone(),
            new_layout: layout,
        };
        self.changes.push(change.clone());
        GraphViewCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: match old_layout {
                Some(layout) => Some(GraphViewCommand::SetNodeLayout { node, layout }),
                None => Some(GraphViewCommand::RemoveNodeLayout { node }),
            },
        }
    }

    fn remove_node_layout(&mut self, node: NodeId) -> GraphViewCommandResult {
        let Some(old_layout) = self.working.nodes.remove(&node) else {
            return self.fail(
                Diagnostic::error(
                    "graph_view.node_layout_not_found",
                    format!("node layout `{}` not found", node.as_str()),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: self.working.graph.clone(),
                    node,
                }),
            );
        };
        let change = GraphViewChange::NodeLayoutRemoved {
            node: node.clone(),
            old_layout: old_layout.clone(),
        };
        self.changes.push(change.clone());
        GraphViewCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphViewCommand::SetNodeLayout {
                node,
                layout: old_layout,
            }),
        }
    }

    fn set_group_layout(&mut self, group: GroupId, layout: GroupLayout) -> GraphViewCommandResult {
        let old_layout = self.working.groups.insert(group.clone(), layout.clone());
        let change = GraphViewChange::GroupLayoutChanged {
            group: group.clone(),
            old_layout: old_layout.clone(),
            new_layout: layout,
        };
        self.changes.push(change.clone());
        GraphViewCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: match old_layout {
                Some(layout) => Some(GraphViewCommand::SetGroupLayout { group, layout }),
                None => Some(GraphViewCommand::RemoveGroupLayout { group }),
            },
        }
    }

    fn remove_group_layout(&mut self, group: GroupId) -> GraphViewCommandResult {
        let Some(old_layout) = self.working.groups.remove(&group) else {
            return self.fail(
                Diagnostic::error(
                    "graph_view.group_layout_not_found",
                    format!("group layout `{}` not found", group.as_str()),
                )
                .with_target(DiagnosticTarget::Group {
                    graph: self.working.graph.clone(),
                    group,
                }),
            );
        };
        let change = GraphViewChange::GroupLayoutRemoved {
            group: group.clone(),
            old_layout: old_layout.clone(),
        };
        self.changes.push(change.clone());
        GraphViewCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphViewCommand::SetGroupLayout {
                group,
                layout: old_layout,
            }),
        }
    }

    fn set_viewport(&mut self, viewport: Viewport) -> GraphViewCommandResult {
        let old_viewport = self.working.viewport;
        self.working.viewport = viewport;
        let change = GraphViewChange::ViewportChanged {
            old_viewport,
            new_viewport: viewport,
        };
        self.changes.push(change.clone());
        GraphViewCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphViewCommand::SetViewport {
                viewport: old_viewport,
            }),
        }
    }

    fn set_selection(&mut self, selection: Selection) -> GraphViewCommandResult {
        let old_selection = self.working.selection.clone();
        self.working.selection = selection.clone();
        let change = GraphViewChange::SelectionChanged {
            old_selection: old_selection.clone(),
            new_selection: selection,
        };
        self.changes.push(change.clone());
        GraphViewCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphViewCommand::SetSelection {
                selection: old_selection,
            }),
        }
    }

    fn set_layout_policy(&mut self, layout_policy: LayoutPolicyId) -> GraphViewCommandResult {
        let old_layout_policy = self.working.layout_policy.clone();
        self.working.layout_policy = layout_policy.clone();
        let change = GraphViewChange::LayoutPolicyChanged {
            old_layout_policy: old_layout_policy.clone(),
            new_layout_policy: layout_policy,
        };
        self.changes.push(change.clone());
        GraphViewCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphViewCommand::SetLayoutPolicy {
                layout_policy: old_layout_policy,
            }),
        }
    }

    fn fail(&mut self, diagnostic: Diagnostic) -> GraphViewCommandResult {
        self.diagnostics.push(diagnostic.clone());
        GraphViewCommandResult {
            changes: Vec::new(),
            diagnostics: vec![diagnostic],
            inverse: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Graph, GraphKind, Group, PortRef};
    use crate::id::PortId;
    use crate::value::Value;

    fn graph() -> (Graph, NodeId, NodeId, EdgeId, GroupId) {
        let mut graph = Graph::new(
            GraphId::generate(),
            GraphKind::new("test.graph"),
            "test_graph",
        );
        let first = NodeId::generate();
        let second = NodeId::generate();
        let edge = EdgeId::generate();
        let group = GroupId::generate();
        graph.nodes.insert(
            first.clone(),
            crate::graph::Node::new(
                first.clone(),
                crate::graph::NodeTypeId::new("test.node"),
                Value::Object(BTreeMap::new()),
            ),
        );
        graph.nodes.insert(
            second.clone(),
            crate::graph::Node::new(
                second.clone(),
                crate::graph::NodeTypeId::new("test.node"),
                Value::Object(BTreeMap::new()),
            ),
        );
        graph.edges.insert(
            edge.clone(),
            Edge::new(
                edge.clone(),
                PortRef::new(first.clone(), PortId::generate()),
                PortRef::new(second.clone(), PortId::generate()),
            ),
        );
        graph
            .groups
            .insert(group.clone(), Group::new(group.clone(), "group"));
        (graph, first, second, edge, group)
    }

    fn view(graph: &Graph) -> GraphView {
        GraphView::new(graph.id.clone())
    }

    #[test]
    fn graph_view_can_be_created_for_graph_id() {
        let (graph, _, _, _, _) = graph();
        let view = view(&graph);
        assert_eq!(view.graph, graph.id);
        assert_eq!(view.layout_policy.as_str(), "layout.manual");
    }

    #[test]
    fn graph_is_valid_without_graph_view() {
        let (graph, _, _, _, _) = graph();
        assert!(graph.nodes.len() >= 2);
    }

    #[test]
    fn deterministic_json_serialization() {
        let (graph, first, _, _, _) = graph();
        let mut view = view(&graph);
        view.nodes
            .insert(first, NodeLayout::new(Vec2::new(10.0, 20.0)));
        let first_json = view
            .to_canonical_json(&graph)
            .expect("valid graph view must serialize");
        let second_json = view
            .to_canonical_json(&graph)
            .expect("valid graph view must serialize");
        assert_eq!(first_json, second_json);
    }

    #[test]
    fn serde_roundtrip_preserves_presentation_data() {
        let (graph, first, _, edge, group) = graph();
        let mut view = view(&graph);
        view.nodes
            .insert(first.clone(), NodeLayout::new(Vec2::new(10.0, 20.0)));
        view.groups.insert(
            group.clone(),
            GroupLayout::new(Rect::new(Vec2::new(0.0, 0.0), Vec2::new(100.0, 80.0))),
        );
        view.selection.nodes.insert(first);
        view.selection.edges.insert(edge);
        view.selection.groups.insert(group);
        let json = view
            .to_canonical_json(&graph)
            .expect("valid graph view must serialize");
        let loaded: GraphView = serde_json::from_str(&json).expect("graph view JSON must load");
        assert_eq!(view.graph, loaded.graph);
        assert_eq!(view.nodes, loaded.nodes);
        assert_eq!(view.groups, loaded.groups);
        assert_eq!(view.selection, loaded.selection);
        assert!(loaded.validate(&graph).is_empty());
    }

    #[test]
    fn serde_roundtrip_creates_new_in_memory_identity() {
        let (graph, _, _, _, _) = graph();
        let view = view(&graph);
        let json = view
            .to_canonical_json(&graph)
            .expect("valid graph view must serialize");
        let loaded: GraphView = serde_json::from_str(&json).expect("graph view JSON must load");
        assert_eq!(view.graph, loaded.graph);
        assert_ne!(view.identity(), loaded.identity());
        assert_eq!(loaded.revision(), 0);
    }

    #[test]
    fn graph_view_deserialize_rejects_unsupported_schema_versions() {
        let (graph, _, _, _, _) = graph();
        let view = view(&graph);
        let json = view
            .to_canonical_json(&graph)
            .expect("valid graph view must serialize");
        for version in [2_u32, 99] {
            let mut document: serde_json::Value =
                serde_json::from_str(&json).expect("graph view JSON must parse");
            document["schema_version"] = serde_json::Value::from(version);
            assert!(
                serde_json::from_value::<GraphView>(document).is_err(),
                "schema_version {version} must be rejected"
            );
        }
    }

    #[test]
    fn graph_view_deserialize_rejects_unknown_top_level_fields() {
        let (graph, _, _, _, _) = graph();
        let view = view(&graph);
        let json = view
            .to_canonical_json(&graph)
            .expect("valid graph view must serialize");
        let mut document: serde_json::Value =
            serde_json::from_str(&json).expect("graph view JSON must parse");
        document
            .as_object_mut()
            .expect("graph view JSON must be an object")
            .insert("future_field".into(), serde_json::Value::Bool(true));

        assert!(serde_json::from_value::<GraphView>(document).is_err());
    }

    #[test]
    fn validate_graph_mismatch() {
        let (graph, _, _, _, _) = graph();
        let mut view = GraphView::new(GraphId::generate());
        view.nodes.clear();
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.graph_mismatch"));
    }

    #[test]
    fn validate_missing_node_layout_reference() {
        let (graph, _, _, _, _) = graph();
        let mut view = view(&graph);
        view.nodes
            .insert(NodeId::generate(), NodeLayout::new(Vec2::new(10.0, 20.0)));
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.missing_node"));
    }

    #[test]
    fn validate_missing_group_layout_reference() {
        let (graph, _, _, _, _) = graph();
        let mut view = view(&graph);
        view.groups.insert(
            GroupId::generate(),
            GroupLayout::new(Rect::new(Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0))),
        );
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.missing_group"));
    }

    #[test]
    fn validate_missing_selected_node_edge_group() {
        let (graph, _, _, _, _) = graph();
        let mut view = view(&graph);
        view.selection.nodes.insert(NodeId::generate());
        view.selection.edges.insert(EdgeId::generate());
        view.selection.groups.insert(GroupId::generate());
        let diagnostics = view.validate(&graph);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.selection_missing_node"));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.selection_missing_edge"));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.selection_missing_group"));
    }

    #[test]
    fn reject_non_finite_position() {
        let (graph, first, _, _, _) = graph();
        let mut view = view(&graph);
        view.nodes
            .insert(first, NodeLayout::new(Vec2::new(f64::NAN, 0.0)));
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.invalid_node_position"));
    }

    #[test]
    fn reject_non_positive_zoom() {
        let (graph, _, _, _, _) = graph();
        let mut view = view(&graph);
        view.viewport.zoom = 0.0;
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.invalid_viewport"));
    }

    #[test]
    fn reject_non_finite_viewport_pan() {
        let (graph, _, _, _, _) = graph();
        let mut view = view(&graph);
        view.viewport.pan = Vec2::new(f64::INFINITY, 0.0);
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.invalid_viewport"));
    }

    #[test]
    fn reject_invalid_group_bounds() {
        let (graph, _, _, _, group) = graph();
        let mut view = view(&graph);
        view.groups.insert(
            group,
            GroupLayout::new(Rect::new(Vec2::new(0.0, 0.0), Vec2::new(-1.0, 1.0))),
        );
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.invalid_group_bounds"));
    }

    #[test]
    fn reject_non_finite_group_bounds_position() {
        let (graph, _, _, _, group) = graph();
        let mut view = view(&graph);
        view.groups.insert(
            group,
            GroupLayout::new(Rect::new(
                Vec2::new(f64::NEG_INFINITY, 0.0),
                Vec2::new(1.0, 1.0),
            )),
        );
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.invalid_group_bounds"));
    }

    #[test]
    fn reject_non_finite_group_bounds_size() {
        let (graph, _, _, _, group) = graph();
        let mut view = view(&graph);
        view.groups.insert(
            group,
            GroupLayout::new(Rect::new(
                Vec2::new(0.0, 0.0),
                Vec2::new(f64::INFINITY, 1.0),
            )),
        );
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.invalid_group_bounds"));
    }

    #[test]
    fn reject_non_finite_graph_view_annotation() {
        let (graph, _, _, _, _) = graph();
        let mut view = view(&graph);
        view.annotations.insert("bad".into(), Value::F64(f64::NAN));
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.non_finite_annotation_value"));
    }

    #[test]
    fn reject_non_finite_node_layout_annotation() {
        let (graph, first, _, _, _) = graph();
        let mut view = view(&graph);
        let mut layout = NodeLayout::new(Vec2::new(0.0, 0.0));
        layout
            .annotations
            .insert("bad".into(), Value::F64(f64::INFINITY));
        view.nodes.insert(first, layout);
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.non_finite_annotation_value"));
    }

    #[test]
    fn reject_non_finite_group_layout_annotation() {
        let (graph, _, _, _, group) = graph();
        let mut view = view(&graph);
        let mut layout = GroupLayout::new(Rect::new(Vec2::new(0.0, 0.0), Vec2::new(1.0, 1.0)));
        layout
            .annotations
            .insert("bad".into(), Value::F64(f64::NEG_INFINITY));
        view.groups.insert(group, layout);
        assert!(view
            .validate(&graph)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph_view.non_finite_annotation_value"));
    }

    #[test]
    fn layout_policy_id_rejects_invalid_json() {
        assert!(serde_json::from_str::<LayoutPolicyId>(r#""layout""#).is_err());
        assert!(serde_json::from_str::<LayoutPolicyId>(r#""Layout.Manual""#).is_err());
    }

    #[test]
    fn transaction_commit_succeeds() {
        let (graph, first, _, _, _) = graph();
        let mut view = view(&graph);
        let mut tx = GraphViewTransaction::begin(&view);
        let result = tx.apply(GraphViewCommand::SetNodeLayout {
            node: first.clone(),
            layout: NodeLayout::new(Vec2::new(1.0, 2.0)),
        });
        assert!(result.is_ok());
        let changes = tx
            .commit(&mut view, &graph)
            .expect("valid graph view transaction must commit");
        assert_eq!(changes.len(), 1);
        assert!(view.nodes.contains_key(&first));
    }

    #[test]
    fn transaction_rollback_leaves_view_unchanged() {
        let (graph, first, _, _, _) = graph();
        let view = view(&graph);
        let mut tx = GraphViewTransaction::begin(&view);
        tx.apply(GraphViewCommand::SetNodeLayout {
            node: first,
            layout: NodeLayout::new(Vec2::new(1.0, 2.0)),
        });
        tx.rollback();
        assert!(view.nodes.is_empty());
    }

    #[test]
    fn failed_commit_leaves_view_unchanged() {
        let (graph, _, _, _, _) = graph();
        let mut view = view(&graph);
        let mut tx = GraphViewTransaction::begin(&view);
        tx.apply(GraphViewCommand::SetViewport {
            viewport: Viewport::new(Vec2::new(0.0, 0.0), -1.0),
        });
        assert!(matches!(
            tx.commit(&mut view, &graph),
            Err(GraphViewTransactionError::ValidationFailed { .. })
        ));
        assert_eq!(view.viewport.zoom, 1.0);
    }

    #[test]
    fn stale_revision_conflict() {
        let (graph, first, second, _, _) = graph();
        let mut view = view(&graph);
        let stale = GraphViewTransaction::begin(&view);
        let mut current = GraphViewTransaction::begin(&view);
        current.apply(GraphViewCommand::SetNodeLayout {
            node: first,
            layout: NodeLayout::new(Vec2::new(1.0, 2.0)),
        });
        current
            .commit(&mut view, &graph)
            .expect("current transaction must commit");
        let mut stale = stale;
        stale.apply(GraphViewCommand::SetNodeLayout {
            node: second,
            layout: NodeLayout::new(Vec2::new(3.0, 4.0)),
        });
        assert!(matches!(
            stale.commit(&mut view, &graph),
            Err(GraphViewTransactionError::Conflict { .. })
        ));
    }

    #[test]
    fn unrelated_identity_conflict() {
        let (graph, first, _, _, _) = graph();
        let source = view(&graph);
        let mut unrelated = view(&graph);
        let mut tx = GraphViewTransaction::begin(&source);
        tx.apply(GraphViewCommand::SetNodeLayout {
            node: first,
            layout: NodeLayout::new(Vec2::new(1.0, 2.0)),
        });
        assert!(matches!(
            tx.commit(&mut unrelated, &graph),
            Err(GraphViewTransactionError::Conflict { .. })
        ));
    }

    #[test]
    fn graph_transaction_and_graph_view_transaction_remain_separate() {
        let (graph, first, _, _, _) = graph();
        let mut view = view(&graph);
        let mut tx = GraphViewTransaction::begin(&view);
        tx.apply(GraphViewCommand::SetNodeLayout {
            node: first,
            layout: NodeLayout::new(Vec2::new(1.0, 2.0)),
        });
        tx.commit(&mut view, &graph)
            .expect("view transaction must commit");
        assert!(graph.nodes.len() >= 2);
        assert_eq!(view.nodes.len(), 1);
    }

    #[test]
    fn stable_diagnostic_codes() {
        let codes = [
            "graph_view.graph_mismatch",
            "graph_view.invalid_viewport",
            "graph_view.missing_node",
            "graph_view.invalid_node_position",
            "graph_view.missing_group",
            "graph_view.invalid_group_bounds",
            "graph_view.selection_missing_node",
            "graph_view.selection_missing_edge",
            "graph_view.selection_missing_group",
            "graph_view.non_finite_annotation_value",
            "graph_view.node_layout_not_found",
            "graph_view.group_layout_not_found",
        ];
        assert_eq!(codes.len(), 12);
    }
}
