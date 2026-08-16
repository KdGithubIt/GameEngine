//! Domain-neutral semantic graph authoring model.
//!
//! This module contains the Phase 3-A graph foundation. It owns semantic graph
//! storage, schema descriptions, structural validation, graph commands, graph
//! changes, single-document transactions, and deterministic semantic JSON
//! serialization. It does not contain graph view data, auto-layout, concrete
//! graph domains, runtime execution, or renderer state.

/// Shared structured authoring service for semantic Graph documents.
#[path = "graph_authoring.rs"]
pub mod authoring_service;

use crate::diagnostic::{Diagnostic, DiagnosticTarget};
use crate::id::{EdgeId, GraphId, GroupId, NodeId, PortId};
use crate::value::Value;
use serde::de;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Semantic annotations attached to graph objects.
///
/// Annotation keys are deterministic strings. Concrete graph domains may
/// define namespaced keys, but the graph foundation treats annotations as
/// opaque authoring values.
pub type Annotations = BTreeMap<String, Value>;

macro_rules! define_dotted_id {
    ($(#[$attr:meta])* $name:ident) => {
        $(#[$attr])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates this identifier from a known-valid dotted string.
            ///
            /// # Panics
            ///
            /// Panics when `value` is not a valid lowercase dotted identifier.
            /// Use [`try_new`](Self::try_new) for untrusted input.
            pub fn new(value: impl Into<String>) -> Self {
                Self::try_new(value).expect("graph dotted identifier must be valid")
            }

            /// Creates this identifier from a lowercase dotted string.
            ///
            /// # Errors
            ///
            /// Returns [`DottedIdError`] when the value is empty, lacks a dot,
            /// contains an empty segment, or contains unsupported characters.
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

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(de::Error::custom)
            }
        }
    };
}

define_dotted_id!(
    /// Identifies a semantic graph kind, such as `logic.test`.
    GraphKind
);

define_dotted_id!(
    /// Identifies a graph node schema type.
    NodeTypeId
);

define_dotted_id!(
    /// Identifies a port value type exposed by graph schemas.
    PortValueTypeId
);

/// Reports why a graph dotted identifier was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DottedIdError {
    /// The identifier was empty.
    Empty,
    /// The identifier did not contain a namespace separator.
    MissingNamespace,
    /// One of the dot-separated namespace segments was empty.
    EmptySegment,
    /// A segment contained an unsupported character or did not start with a
    /// lowercase ASCII letter.
    InvalidSegment {
        /// The rejected segment.
        segment: String,
    },
}

impl fmt::Display for DottedIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("graph dotted identifier must not be empty"),
            Self::MissingNamespace => {
                formatter.write_str("graph dotted identifier must use a dotted namespace")
            }
            Self::EmptySegment => {
                formatter.write_str("graph dotted identifier must not contain empty segments")
            }
            Self::InvalidSegment { segment } => write!(
                formatter,
                "graph dotted identifier segment `{segment}` must start with a lowercase ASCII letter and contain only lowercase ASCII letters, digits, or underscores"
            ),
        }
    }
}

impl std::error::Error for DottedIdError {}

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

const CURRENT_GRAPH_SCHEMA_VERSION: u32 = 1;

/// A domain-neutral semantic graph document.
#[derive(Debug, Serialize)]
pub struct Graph {
    /// Stable project-wide graph identifier.
    pub id: GraphId,
    /// Domain-neutral graph kind identifier.
    pub kind: GraphKind,
    /// Semantic schema version for this graph document.
    pub schema_version: u32,
    /// AI-searchable graph slug.
    pub name: String,
    /// Human-readable display label.
    pub display_name: String,
    /// Human-readable description and AI context.
    pub description: String,
    /// Graph nodes in stable identifier order.
    pub nodes: BTreeMap<NodeId, Node>,
    /// Graph edges in stable identifier order.
    pub edges: BTreeMap<EdgeId, Edge>,
    /// Semantic groups in stable identifier order.
    pub groups: BTreeMap<GroupId, Group>,
    /// Semantic graph annotations.
    pub annotations: Annotations,
    #[serde(skip_serializing)]
    identity: u64,
    #[serde(skip_serializing)]
    revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphDocument {
    id: GraphId,
    kind: GraphKind,
    schema_version: u32,
    name: String,
    display_name: String,
    description: String,
    nodes: BTreeMap<NodeId, Node>,
    edges: BTreeMap<EdgeId, Edge>,
    groups: BTreeMap<GroupId, Group>,
    annotations: Annotations,
}

impl<'de> Deserialize<'de> for Graph {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = GraphDocument::deserialize(deserializer)?;
        if document.schema_version != CURRENT_GRAPH_SCHEMA_VERSION {
            return Err(de::Error::custom(format!(
                "unsupported graph schema_version {}, expected {}",
                document.schema_version, CURRENT_GRAPH_SCHEMA_VERSION
            )));
        }
        Ok(Self {
            id: document.id,
            kind: document.kind,
            schema_version: document.schema_version,
            name: document.name,
            display_name: document.display_name,
            description: document.description,
            nodes: document.nodes,
            edges: document.edges,
            groups: document.groups,
            annotations: document.annotations,
            identity: next_graph_identity(),
            revision: 0,
        })
    }
}

impl Graph {
    /// Creates an empty semantic graph.
    pub fn new(id: GraphId, kind: GraphKind, name: impl Into<String>) -> Self {
        Self {
            id,
            kind,
            schema_version: CURRENT_GRAPH_SCHEMA_VERSION,
            name: name.into(),
            display_name: String::new(),
            description: String::new(),
            nodes: BTreeMap::new(),
            edges: BTreeMap::new(),
            groups: BTreeMap::new(),
            annotations: BTreeMap::new(),
            identity: next_graph_identity(),
            revision: 0,
        }
    }

    /// Returns structured structural diagnostics for this graph.
    pub fn validate(&self, schemas: &dyn GraphSchemaRegistry) -> Vec<Diagnostic> {
        validate_graph(self, schemas)
    }

    /// Serializes this graph to deterministic canonical pretty-printed JSON.
    ///
    /// This method validates the graph before serialization. It does not write
    /// files or serialize any graph view data.
    ///
    /// # Errors
    ///
    /// Returns an error when structural validation fails or JSON serialization
    /// cannot complete.
    pub fn to_canonical_json(
        &self,
        schemas: &dyn GraphSchemaRegistry,
    ) -> Result<String, GraphSaveError> {
        let diagnostics = self.validate(schemas);
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(GraphSaveError::ValidationFailed { diagnostics });
        }
        serde_json::to_string_pretty(self).map_err(GraphSaveError::Json)
    }

    fn identity(&self) -> u64 {
        self.identity
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn clone_for_transaction(&self) -> Self {
        Self {
            id: self.id.clone(),
            kind: self.kind.clone(),
            schema_version: self.schema_version,
            name: self.name.clone(),
            display_name: self.display_name.clone(),
            description: self.description.clone(),
            nodes: self.nodes.clone(),
            edges: self.edges.clone(),
            groups: self.groups.clone(),
            annotations: self.annotations.clone(),
            identity: self.identity,
            revision: self.revision,
        }
    }

    fn commit_from(&mut self, mut working: Graph) {
        working.identity = self.identity;
        working.revision = self.revision.saturating_add(1);
        *self = working;
    }
}

/// Reports why a semantic graph could not be serialized.
#[derive(Debug)]
pub enum GraphSaveError {
    /// Graph structure is invalid.
    ValidationFailed {
        /// All diagnostics produced by graph validation.
        diagnostics: Vec<Diagnostic>,
    },
    /// JSON serialization failed.
    Json(serde_json::Error),
}

impl fmt::Display for GraphSaveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed { diagnostics } => write!(
                formatter,
                "graph cannot be saved because validation produced {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::Json(error) => write!(formatter, "graph JSON serialization failed: {error}"),
        }
    }
}

impl std::error::Error for GraphSaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ValidationFailed { .. } => None,
            Self::Json(error) => Some(error),
        }
    }
}

fn next_graph_identity() -> u64 {
    static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);
    NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

/// A semantic graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Stable graph-local node identifier.
    pub id: NodeId,
    /// Stable node schema type identifier.
    pub node_type: NodeTypeId,
    /// Optional AI-searchable node slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Domain-owned node properties.
    pub properties: Value,
    /// Semantic node annotations.
    pub annotations: Annotations,
}

impl Node {
    /// Creates a node without presentation coordinates.
    pub fn new(id: NodeId, node_type: NodeTypeId, properties: Value) -> Self {
        Self {
            id,
            node_type,
            name: None,
            properties,
            annotations: BTreeMap::new(),
        }
    }
}

/// A semantic graph edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// Stable graph-local edge identifier.
    pub id: EdgeId,
    /// Output endpoint.
    pub from: PortRef,
    /// Input endpoint.
    pub to: PortRef,
    /// Semantic edge annotations.
    pub annotations: Annotations,
}

impl Edge {
    /// Creates an edge from an output port to an input port.
    pub fn new(id: EdgeId, from: PortRef, to: PortRef) -> Self {
        Self {
            id,
            from,
            to,
            annotations: BTreeMap::new(),
        }
    }
}

/// A reference to a port on a graph node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PortRef {
    /// The node that owns the port.
    pub node: NodeId,
    /// The port on the node schema.
    pub port: PortId,
}

impl PortRef {
    /// Creates a port reference.
    pub fn new(node: NodeId, port: PortId) -> Self {
        Self { node, port }
    }
}

/// A semantic group of nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Stable graph-local group identifier.
    pub id: GroupId,
    /// AI-searchable group slug.
    pub name: String,
    /// Group member nodes.
    pub nodes: BTreeSet<NodeId>,
    /// Semantic group annotations.
    pub annotations: Annotations,
}

impl Group {
    /// Creates an empty semantic group.
    pub fn new(id: GroupId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            nodes: BTreeSet::new(),
            annotations: BTreeMap::new(),
        }
    }
}

/// Direction of a graph port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortDirection {
    /// Input endpoint.
    Input,
    /// Output endpoint.
    Output,
}

/// Structural connection count constraints for a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortArity {
    /// Minimum required connection count.
    pub min_connections: u32,
    /// Maximum allowed connection count. `None` means unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_connections: Option<u32>,
}

impl PortArity {
    /// Returns an arity that allows at most one connection.
    pub fn optional_single() -> Self {
        Self {
            min_connections: 0,
            max_connections: Some(1),
        }
    }

    /// Returns an arity that allows any number of connections.
    pub fn many() -> Self {
        Self {
            min_connections: 0,
            max_connections: None,
        }
    }
}

/// Schema for one graph port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortSchema {
    /// Stable port identifier.
    pub id: PortId,
    /// AI-searchable port slug.
    pub name: String,
    /// Human-readable display label.
    pub display_name: String,
    /// Human-readable description and AI context.
    pub description: String,
    /// Whether this port accepts incoming or outgoing edges.
    pub direction: PortDirection,
    /// Opaque domain-owned value type identifier.
    pub value_type: PortValueTypeId,
    /// Structural connection count constraints.
    pub arity: PortArity,
}

/// Schema for one graph node type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeSchema {
    /// Stable node type identifier.
    pub node_type: NodeTypeId,
    /// Graph kinds that may use this node type.
    pub compatible_graph_kinds: BTreeSet<GraphKind>,
    /// Human-readable display label.
    pub display_name: String,
    /// Human-readable description and AI context.
    pub description: String,
    /// Node picker category.
    pub category: String,
    /// Search tags used by authoring tools.
    pub search_tags: Vec<String>,
    /// Domain-owned node property schema placeholder.
    pub property_schema: Value,
    /// Port schemas keyed by stable port ID.
    pub ports: BTreeMap<PortId, PortSchema>,
    /// Schema version.
    pub version: u32,
}

/// Read-only schema lookup used by structural graph validation.
pub trait GraphSchemaRegistry {
    /// Returns the schema for `node_type`, or `None` when it is unknown.
    fn node_schema(&self, node_type: &NodeTypeId) -> Option<&NodeSchema>;
}

fn validate_graph(graph: &Graph, schemas: &dyn GraphSchemaRegistry) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut connection_counts: BTreeMap<PortRef, u32> = BTreeMap::new();
    let mut endpoint_pairs: BTreeMap<(PortRef, PortRef), EdgeId> = BTreeMap::new();

    validate_annotations(
        &graph.id,
        "graph",
        &graph.annotations,
        DiagnosticTarget::Graph {
            id: graph.id.clone(),
        },
        &mut diagnostics,
    );

    for (node_id, node) in &graph.nodes {
        if node_id != &node.id {
            diagnostics.push(
                Diagnostic::error(
                    "graph.node_id_mismatch",
                    format!(
                        "node map key `{}` does not match node id `{}`",
                        node_id.as_str(),
                        node.id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: graph.id.clone(),
                    node: node_id.clone(),
                }),
            );
        }
        let Some(schema) = schemas.node_schema(&node.node_type) else {
            diagnostics.push(
                Diagnostic::error(
                    "graph.unknown_node_type",
                    format!(
                        "node `{}` uses unknown node type `{}`",
                        node_id.as_str(),
                        node.node_type.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: graph.id.clone(),
                    node: node_id.clone(),
                }),
            );
            continue;
        };
        if !schema.compatible_graph_kinds.is_empty()
            && !schema.compatible_graph_kinds.contains(&graph.kind)
        {
            diagnostics.push(
                Diagnostic::error(
                    "graph.incompatible_node_kind",
                    format!(
                        "node `{}` uses node type `{}` that is not compatible with graph kind `{}`",
                        node_id.as_str(),
                        node.node_type.as_str(),
                        graph.kind.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: graph.id.clone(),
                    node: node_id.clone(),
                }),
            );
        }
        for (port_id, port) in &schema.ports {
            if port_id != &port.id {
                diagnostics.push(
                    Diagnostic::error(
                        "graph.port_schema_id_mismatch",
                        format!(
                            "port schema map key `{}` does not match port id `{}`",
                            port_id.as_str(),
                            port.id.as_str()
                        ),
                    )
                    .with_target(DiagnosticTarget::Port {
                        graph: graph.id.clone(),
                        node: node_id.clone(),
                        port: port_id.clone(),
                    }),
                );
            }
        }
        if !matches!(node.properties, Value::Object(_)) {
            diagnostics.push(
                Diagnostic::error(
                    "graph.invalid_node_properties",
                    format!("node `{}` properties must be an object", node_id.as_str()),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: graph.id.clone(),
                    node: node_id.clone(),
                }),
            );
        }
        validate_value(
            &graph.id,
            &format!("node `{}` properties", node_id.as_str()),
            "properties",
            &node.properties,
            DiagnosticTarget::Node {
                graph: graph.id.clone(),
                node: node_id.clone(),
            },
            &mut diagnostics,
        );
        validate_annotations(
            &graph.id,
            &format!("node `{}`", node_id.as_str()),
            &node.annotations,
            DiagnosticTarget::Node {
                graph: graph.id.clone(),
                node: node_id.clone(),
            },
            &mut diagnostics,
        );
    }

    for (edge_id, edge) in &graph.edges {
        if edge_id != &edge.id {
            diagnostics.push(
                Diagnostic::error(
                    "graph.edge_id_mismatch",
                    format!(
                        "edge map key `{}` does not match edge id `{}`",
                        edge_id.as_str(),
                        edge.id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Edge {
                    graph: graph.id.clone(),
                    edge: edge_id.clone(),
                }),
            );
        }
        validate_annotations(
            &graph.id,
            &format!("edge `{}`", edge_id.as_str()),
            &edge.annotations,
            DiagnosticTarget::Edge {
                graph: graph.id.clone(),
                edge: edge_id.clone(),
            },
            &mut diagnostics,
        );
        validate_endpoint(
            graph,
            schemas,
            edge_id,
            &edge.from,
            PortDirection::Output,
            &mut diagnostics,
        );
        validate_endpoint(
            graph,
            schemas,
            edge_id,
            &edge.to,
            PortDirection::Input,
            &mut diagnostics,
        );
        *connection_counts.entry(edge.from.clone()).or_default() += 1;
        *connection_counts.entry(edge.to.clone()).or_default() += 1;
        let pair = (edge.from.clone(), edge.to.clone());
        if let Some(first_edge) = endpoint_pairs.insert(pair, edge_id.clone()) {
            diagnostics.push(
                Diagnostic::error(
                    "graph.duplicate_edge",
                    format!(
                        "edge `{}` duplicates connection already used by edge `{}`",
                        edge_id.as_str(),
                        first_edge.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Edge {
                    graph: graph.id.clone(),
                    edge: edge_id.clone(),
                }),
            );
        }
    }

    validate_arities(graph, schemas, &connection_counts, &mut diagnostics);

    for (group_id, group) in &graph.groups {
        if group_id != &group.id {
            diagnostics.push(
                Diagnostic::error(
                    "graph.group_id_mismatch",
                    format!(
                        "group map key `{}` does not match group id `{}`",
                        group_id.as_str(),
                        group.id.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Group {
                    graph: graph.id.clone(),
                    group: group_id.clone(),
                }),
            );
        }
        for node in &group.nodes {
            if !graph.nodes.contains_key(node) {
                diagnostics.push(
                    Diagnostic::error(
                        "graph.group_missing_node",
                        format!(
                            "group `{}` references missing node `{}`",
                            group_id.as_str(),
                            node.as_str()
                        ),
                    )
                    .with_target(DiagnosticTarget::Group {
                        graph: graph.id.clone(),
                        group: group_id.clone(),
                    }),
                );
            }
        }
        validate_annotations(
            &graph.id,
            &format!("group `{}`", group_id.as_str()),
            &group.annotations,
            DiagnosticTarget::Group {
                graph: graph.id.clone(),
                group: group_id.clone(),
            },
            &mut diagnostics,
        );
    }

    diagnostics
}

fn validate_annotations(
    graph: &GraphId,
    owner: &str,
    annotations: &Annotations,
    target: DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (key, value) in annotations {
        validate_value(
            graph,
            owner,
            &format!("annotations.{key}"),
            value,
            target.clone(),
            diagnostics,
        );
    }
}

fn validate_value(
    graph: &GraphId,
    owner: &str,
    path: &str,
    value: &Value,
    target: DiagnosticTarget,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match value {
        Value::F64(number) if !number.is_finite() => {
            diagnostics.push(
                Diagnostic::error(
                    "graph.non_finite_number",
                    format!(
                        "graph `{}` {} contains a non-finite number at `{}`",
                        graph.as_str(),
                        owner,
                        path
                    ),
                )
                .with_target(target),
            );
        }
        Value::Array(values) => {
            for (index, nested) in values.iter().enumerate() {
                validate_value(
                    graph,
                    owner,
                    &format!("{path}[{index}]"),
                    nested,
                    target.clone(),
                    diagnostics,
                );
            }
        }
        Value::Object(values) => {
            for (key, nested) in values {
                validate_value(
                    graph,
                    owner,
                    &format!("{path}.{key}"),
                    nested,
                    target.clone(),
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

fn validate_endpoint(
    graph: &Graph,
    schemas: &dyn GraphSchemaRegistry,
    edge_id: &EdgeId,
    endpoint: &PortRef,
    expected_direction: PortDirection,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(node) = graph.nodes.get(&endpoint.node) else {
        diagnostics.push(
            Diagnostic::error(
                "graph.missing_endpoint_node",
                format!(
                    "edge `{}` references missing node `{}`",
                    edge_id.as_str(),
                    endpoint.node.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Edge {
                graph: graph.id.clone(),
                edge: edge_id.clone(),
            }),
        );
        return;
    };
    let Some(schema) = schemas.node_schema(&node.node_type) else {
        return;
    };
    let Some(port) = schema.ports.get(&endpoint.port) else {
        diagnostics.push(
            Diagnostic::error(
                "graph.missing_endpoint_port",
                format!(
                    "edge `{}` references missing port `{}` on node `{}`",
                    edge_id.as_str(),
                    endpoint.port.as_str(),
                    endpoint.node.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Port {
                graph: graph.id.clone(),
                node: endpoint.node.clone(),
                port: endpoint.port.clone(),
            }),
        );
        return;
    };
    if port.direction != expected_direction {
        diagnostics.push(
            Diagnostic::error(
                "graph.invalid_endpoint_direction",
                format!(
                    "edge `{}` references port `{}` with the wrong direction",
                    edge_id.as_str(),
                    endpoint.port.as_str()
                ),
            )
            .with_target(DiagnosticTarget::Port {
                graph: graph.id.clone(),
                node: endpoint.node.clone(),
                port: endpoint.port.clone(),
            }),
        );
    }
}

fn validate_arities(
    graph: &Graph,
    schemas: &dyn GraphSchemaRegistry,
    connection_counts: &BTreeMap<PortRef, u32>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (node_id, node) in &graph.nodes {
        let Some(schema) = schemas.node_schema(&node.node_type) else {
            continue;
        };
        for (port_id, port) in &schema.ports {
            let endpoint = PortRef::new(node_id.clone(), port_id.clone());
            let count = connection_counts.get(&endpoint).copied().unwrap_or(0);
            if count < port.arity.min_connections {
                diagnostics.push(
                    Diagnostic::error(
                        "graph.port_arity_violation",
                        format!(
                            "port `{}` on node `{}` has {} connection(s), fewer than required {}",
                            port_id.as_str(),
                            node_id.as_str(),
                            count,
                            port.arity.min_connections
                        ),
                    )
                    .with_target(DiagnosticTarget::Port {
                        graph: graph.id.clone(),
                        node: node_id.clone(),
                        port: port_id.clone(),
                    }),
                );
            }
            if port
                .arity
                .max_connections
                .is_some_and(|maximum| count > maximum)
            {
                diagnostics.push(
                    Diagnostic::error(
                        "graph.port_arity_violation",
                        format!(
                            "port `{}` on node `{}` has too many connections",
                            port_id.as_str(),
                            node_id.as_str()
                        ),
                    )
                    .with_target(DiagnosticTarget::Port {
                        graph: graph.id.clone(),
                        node: node_id.clone(),
                        port: port_id.clone(),
                    }),
                );
            }
        }
    }
}

/// A deterministic mutation of one semantic graph document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphCommand {
    /// Replaces or removes one semantic graph annotation.
    SetGraphAnnotation {
        /// Stable, domain-owned annotation key.
        key: String,
        /// New value, or `None` to remove the annotation.
        value: Option<Value>,
    },
    /// Adds a semantic node.
    AddNode {
        /// The node to add.
        node: Node,
    },
    /// Deletes a node, its incident edges, and its group memberships.
    DeleteNode {
        /// The node to delete.
        node: NodeId,
    },
    /// Replaces a node's full property value.
    SetNodeProperty {
        /// The node to modify.
        node: NodeId,
        /// The new property value.
        value: Value,
    },
    /// Replaces a node's optional human-readable name.
    SetNodeName {
        /// The node to modify.
        node: NodeId,
        /// The new name, or `None` to clear the name.
        name: Option<String>,
    },
    /// Adds a semantic edge.
    AddEdge {
        /// The edge to add.
        edge: Edge,
    },
    /// Deletes a semantic edge.
    DeleteEdge {
        /// The edge to delete.
        edge: EdgeId,
    },
    /// Adds a semantic group.
    AddGroup {
        /// The group to add.
        group: Group,
    },
    /// Deletes a semantic group.
    DeleteGroup {
        /// The group to delete.
        group: GroupId,
    },
    /// Adds an existing node to an existing group.
    AddNodeToGroup {
        /// The target group.
        group: GroupId,
        /// The node to add.
        node: NodeId,
    },
    /// Removes a node from a group.
    RemoveNodeFromGroup {
        /// The target group.
        group: GroupId,
        /// The node to remove.
        node: NodeId,
    },
}

/// Semantic graph change record used for preview diffs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphChange {
    /// One semantic graph annotation was replaced or removed.
    GraphAnnotationChanged {
        /// Domain-owned annotation key.
        key: String,
        /// Previous value, when the annotation existed.
        old_value: Option<Value>,
        /// New value, when the annotation remains present.
        new_value: Option<Value>,
    },
    /// A node was added.
    NodeAdded {
        /// The added node.
        node: Node,
    },
    /// A node was deleted.
    NodeDeleted {
        /// The deleted node.
        node: Node,
        /// Edges removed with the node.
        removed_edges: Vec<Edge>,
        /// Group memberships removed with the node.
        removed_group_memberships: Vec<GroupId>,
    },
    /// A node property value was replaced.
    NodePropertyChanged {
        /// The affected node.
        node: NodeId,
        /// Previous property value.
        old_value: Value,
        /// New property value.
        new_value: Value,
    },
    /// A node's optional human-readable name was replaced.
    NodeNameChanged {
        /// The affected node.
        node: NodeId,
        /// Previous optional name.
        old_name: Option<String>,
        /// New optional name.
        new_name: Option<String>,
    },
    /// An edge was added.
    EdgeAdded {
        /// The added edge.
        edge: Edge,
    },
    /// An edge was deleted.
    EdgeDeleted {
        /// The deleted edge.
        edge: Edge,
    },
    /// A group was added.
    GroupAdded {
        /// The added group.
        group: Group,
    },
    /// A group was deleted.
    GroupDeleted {
        /// The deleted group.
        group: Group,
    },
    /// A node was added to a group.
    NodeAddedToGroup {
        /// The affected group.
        group: GroupId,
        /// The affected node.
        node: NodeId,
    },
    /// A node was removed from a group.
    NodeRemovedFromGroup {
        /// The affected group.
        group: GroupId,
        /// The affected node.
        node: NodeId,
    },
}

/// Result of applying a graph command.
#[derive(Debug)]
pub struct GraphCommandResult {
    /// Semantic graph changes produced by this command.
    pub changes: Vec<GraphChange>,
    /// Diagnostics produced by this command.
    pub diagnostics: Vec<Diagnostic>,
    /// Optional inverse graph command.
    pub inverse: Option<GraphCommand>,
}

impl GraphCommandResult {
    /// Returns `true` when this command produced no blocking diagnostics.
    pub fn is_ok(&self) -> bool {
        !self.diagnostics.iter().any(Diagnostic::is_blocking)
    }
}

/// Successful graph transaction commit result.
#[derive(Debug)]
pub struct GraphTransactionCommit {
    /// Semantic graph changes committed by the transaction.
    pub changes: Vec<GraphChange>,
    /// Non-blocking diagnostics accumulated during command and structural validation.
    pub diagnostics: Vec<Diagnostic>,
}

/// Successful private graph transaction commit result.
#[derive(Debug)]
pub struct GraphTransactionPrivateCommit {
    /// The validated graph after applying the transaction.
    pub graph: Graph,
    /// Semantic graph changes committed by the transaction.
    pub changes: Vec<GraphChange>,
    /// Non-blocking diagnostics accumulated during command and structural validation.
    pub diagnostics: Vec<Diagnostic>,
}

/// Reports validation diagnostics that blocked a private graph transaction commit.
#[derive(Debug)]
pub struct GraphTransactionValidationError {
    /// Diagnostics accumulated during the transaction.
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for GraphTransactionValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "graph transaction blocked by {} diagnostic(s)",
            self.diagnostics.len()
        )
    }
}

impl std::error::Error for GraphTransactionValidationError {}

/// Reports why a graph transaction could not be committed.
#[derive(Debug)]
pub enum GraphTransactionError {
    /// One or more diagnostics blocked commit.
    ValidationFailed {
        /// Diagnostics accumulated during the transaction.
        diagnostics: Vec<Diagnostic>,
    },
    /// The destination graph is not the same revision used to begin the
    /// transaction.
    Conflict {
        /// The identity of the graph used to begin the transaction.
        expected_identity: u64,
        /// The revision used to begin the transaction.
        expected_revision: u64,
        /// The identity of the graph supplied to commit.
        actual_identity: u64,
        /// The current revision of the graph supplied to commit.
        actual_revision: u64,
    },
}

impl fmt::Display for GraphTransactionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed { diagnostics } => write!(
                formatter,
                "graph transaction blocked by {} diagnostic(s)",
                diagnostics.len()
            ),
            Self::Conflict {
                expected_identity,
                expected_revision,
                actual_identity,
                actual_revision,
            } => write!(
                formatter,
                "graph transaction began from graph {expected_identity} revision {expected_revision}, but commit target is graph {actual_identity} revision {actual_revision}"
            ),
        }
    }
}

impl std::error::Error for GraphTransactionError {}

/// Isolated transaction over one semantic graph document.
pub struct GraphTransaction {
    working: Graph,
    changes: Vec<GraphChange>,
    diagnostics: Vec<Diagnostic>,
    base_identity: u64,
    base_revision: u64,
}

impl GraphTransaction {
    /// Begins a new transaction over `graph`.
    pub fn begin(graph: &Graph) -> Self {
        Self {
            working: graph.clone_for_transaction(),
            changes: Vec::new(),
            diagnostics: Vec::new(),
            base_identity: graph.identity(),
            base_revision: graph.revision(),
        }
    }

    /// Applies `command` to the working graph.
    pub fn apply(&mut self, command: GraphCommand) -> GraphCommandResult {
        match command {
            GraphCommand::SetGraphAnnotation { key, value } => {
                self.set_graph_annotation(key, value)
            }
            GraphCommand::AddNode { node } => self.add_node(node),
            GraphCommand::DeleteNode { node } => self.delete_node(node),
            GraphCommand::SetNodeProperty { node, value } => self.set_node_property(node, value),
            GraphCommand::SetNodeName { node, name } => self.set_node_name(node, name),
            GraphCommand::AddEdge { edge } => self.add_edge(edge),
            GraphCommand::DeleteEdge { edge } => self.delete_edge(edge),
            GraphCommand::AddGroup { group } => self.add_group(group),
            GraphCommand::DeleteGroup { group } => self.delete_group(group),
            GraphCommand::AddNodeToGroup { group, node } => self.add_node_to_group(group, node),
            GraphCommand::RemoveNodeFromGroup { group, node } => {
                self.remove_node_from_group(group, node)
            }
        }
    }

    /// Returns the semantic changes accumulated by this transaction.
    pub fn preview_diff(&self) -> &[GraphChange] {
        &self.changes
    }

    /// Returns diagnostics accumulated by this transaction.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Commits this transaction to `graph` if validation passes.
    ///
    /// # Errors
    ///
    /// Returns a validation error when any command or final structural
    /// validation produces blocking diagnostics. Returns a conflict error when
    /// `graph` is not the revision used to begin the transaction.
    pub fn commit(
        self,
        graph: &mut Graph,
        schemas: &dyn GraphSchemaRegistry,
    ) -> Result<Vec<GraphChange>, GraphTransactionError> {
        Ok(self.commit_with_diagnostics(graph, schemas)?.changes)
    }

    /// Commits this transaction and returns committed changes plus non-blocking diagnostics.
    ///
    /// # Errors
    ///
    /// Returns a validation error when any command or final structural
    /// validation produces blocking diagnostics. Returns a conflict error when
    /// `graph` is not the revision used to begin the transaction.
    pub fn commit_with_diagnostics(
        mut self,
        graph: &mut Graph,
        schemas: &dyn GraphSchemaRegistry,
    ) -> Result<GraphTransactionCommit, GraphTransactionError> {
        if graph.identity() != self.base_identity || graph.revision() != self.base_revision {
            return Err(GraphTransactionError::Conflict {
                expected_identity: self.base_identity,
                expected_revision: self.base_revision,
                actual_identity: graph.identity(),
                actual_revision: graph.revision(),
            });
        }
        self.diagnostics.extend(self.working.validate(schemas));
        if self.diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(GraphTransactionError::ValidationFailed {
                diagnostics: self.diagnostics,
            });
        }
        graph.commit_from(self.working);
        Ok(GraphTransactionCommit {
            changes: self.changes,
            diagnostics: self.diagnostics,
        })
    }

    /// Commits this transaction into its private working graph.
    ///
    /// This method never mutates the source graph used to begin the
    /// transaction, and therefore has no stale-target conflict state. It is
    /// intended for adapters that need preview or apply semantics over a
    /// private graph copy before deciding whether to persist it.
    ///
    /// # Errors
    ///
    /// Returns a validation error when any command or final structural
    /// validation produces blocking diagnostics.
    pub fn commit_private(
        mut self,
        schemas: &dyn GraphSchemaRegistry,
    ) -> Result<GraphTransactionPrivateCommit, GraphTransactionValidationError> {
        self.diagnostics.extend(self.working.validate(schemas));
        if self.diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(GraphTransactionValidationError {
                diagnostics: self.diagnostics,
            });
        }
        self.working.identity = self.base_identity;
        self.working.revision = self.base_revision.saturating_add(1);
        Ok(GraphTransactionPrivateCommit {
            graph: self.working,
            changes: self.changes,
            diagnostics: self.diagnostics,
        })
    }

    /// Discards this transaction without modifying the source graph.
    pub fn rollback(self) {}

    fn fail(&mut self, diagnostic: Diagnostic) -> GraphCommandResult {
        self.diagnostics.push(diagnostic.clone());
        GraphCommandResult {
            changes: Vec::new(),
            diagnostics: vec![diagnostic],
            inverse: None,
        }
    }

    fn set_graph_annotation(&mut self, key: String, value: Option<Value>) -> GraphCommandResult {
        if key.trim().is_empty() {
            return self.fail(Diagnostic::error(
                "graph.annotation_key_blank",
                "graph annotation keys must not be blank",
            ));
        }
        let old_value = match &value {
            Some(value) => self.working.annotations.insert(key.clone(), value.clone()),
            None => self.working.annotations.remove(&key),
        };
        let change = GraphChange::GraphAnnotationChanged {
            key: key.clone(),
            old_value: old_value.clone(),
            new_value: value,
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::SetGraphAnnotation {
                key,
                value: old_value,
            }),
        }
    }

    fn add_node(&mut self, node: Node) -> GraphCommandResult {
        if self.working.nodes.contains_key(&node.id) {
            return self.fail(
                Diagnostic::error(
                    "graph.node_already_exists",
                    format!("node `{}` already exists", node.id.as_str()),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: self.working.id.clone(),
                    node: node.id.clone(),
                }),
            );
        }
        self.working.nodes.insert(node.id.clone(), node.clone());
        let change = GraphChange::NodeAdded { node: node.clone() };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::DeleteNode { node: node.id }),
        }
    }

    fn delete_node(&mut self, node: NodeId) -> GraphCommandResult {
        let Some(removed) = self.working.nodes.remove(&node) else {
            return self.fail(node_not_found(&self.working.id, &node));
        };
        let removed_edge_entries: Vec<(EdgeId, Edge)> = self
            .working
            .edges
            .iter()
            .filter(|(_, edge)| edge.from.node == node || edge.to.node == node)
            .map(|(edge_id, edge)| (edge_id.clone(), edge.clone()))
            .collect();
        for (edge_id, _) in &removed_edge_entries {
            self.working.edges.remove(edge_id);
        }
        let removed_edges = removed_edge_entries
            .into_iter()
            .map(|(_, edge)| edge)
            .collect();
        let mut removed_group_memberships = Vec::new();
        for (group_id, group) in &mut self.working.groups {
            if group.nodes.remove(&node) {
                removed_group_memberships.push(group_id.clone());
            }
        }
        let change = GraphChange::NodeDeleted {
            node: removed,
            removed_edges,
            removed_group_memberships,
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: None,
        }
    }

    fn set_node_property(&mut self, node: NodeId, value: Value) -> GraphCommandResult {
        let Some(existing) = self.working.nodes.get_mut(&node) else {
            return self.fail(node_not_found(&self.working.id, &node));
        };
        let old_value = existing.properties.clone();
        existing.properties = value.clone();
        let change = GraphChange::NodePropertyChanged {
            node: node.clone(),
            old_value: old_value.clone(),
            new_value: value,
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::SetNodeProperty {
                node,
                value: old_value,
            }),
        }
    }

    fn set_node_name(&mut self, node: NodeId, name: Option<String>) -> GraphCommandResult {
        let Some(existing) = self.working.nodes.get_mut(&node) else {
            return self.fail(node_not_found(&self.working.id, &node));
        };
        let old_name = existing.name.clone();
        existing.name = name.clone();
        let change = GraphChange::NodeNameChanged {
            node: node.clone(),
            old_name: old_name.clone(),
            new_name: name,
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::SetNodeName {
                node,
                name: old_name,
            }),
        }
    }

    fn add_edge(&mut self, edge: Edge) -> GraphCommandResult {
        if self.working.edges.contains_key(&edge.id) {
            return self.fail(
                Diagnostic::error(
                    "graph.edge_already_exists",
                    format!("edge `{}` already exists", edge.id.as_str()),
                )
                .with_target(DiagnosticTarget::Edge {
                    graph: self.working.id.clone(),
                    edge: edge.id.clone(),
                }),
            );
        }
        self.working.edges.insert(edge.id.clone(), edge.clone());
        let change = GraphChange::EdgeAdded { edge: edge.clone() };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::DeleteEdge { edge: edge.id }),
        }
    }

    fn delete_edge(&mut self, edge: EdgeId) -> GraphCommandResult {
        let Some(removed) = self.working.edges.remove(&edge) else {
            return self.fail(
                Diagnostic::error(
                    "graph.edge_not_found",
                    format!("edge `{}` not found", edge.as_str()),
                )
                .with_target(DiagnosticTarget::Edge {
                    graph: self.working.id.clone(),
                    edge,
                }),
            );
        };
        let change = GraphChange::EdgeDeleted {
            edge: removed.clone(),
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::AddEdge { edge: removed }),
        }
    }

    fn add_group(&mut self, group: Group) -> GraphCommandResult {
        if self.working.groups.contains_key(&group.id) {
            return self.fail(
                Diagnostic::error(
                    "graph.group_already_exists",
                    format!("group `{}` already exists", group.id.as_str()),
                )
                .with_target(DiagnosticTarget::Group {
                    graph: self.working.id.clone(),
                    group: group.id.clone(),
                }),
            );
        }
        self.working.groups.insert(group.id.clone(), group.clone());
        let change = GraphChange::GroupAdded {
            group: group.clone(),
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::DeleteGroup { group: group.id }),
        }
    }

    fn delete_group(&mut self, group: GroupId) -> GraphCommandResult {
        let Some(removed) = self.working.groups.remove(&group) else {
            return self.fail(group_not_found(&self.working.id, &group));
        };
        let change = GraphChange::GroupDeleted {
            group: removed.clone(),
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::AddGroup { group: removed }),
        }
    }

    fn add_node_to_group(&mut self, group: GroupId, node: NodeId) -> GraphCommandResult {
        if !self.working.nodes.contains_key(&node) {
            return self.fail(node_not_found(&self.working.id, &node));
        }
        let Some(existing_group) = self.working.groups.get_mut(&group) else {
            return self.fail(group_not_found(&self.working.id, &group));
        };
        if !existing_group.nodes.insert(node.clone()) {
            return self.fail(
                Diagnostic::error(
                    "graph.group_member_already_exists",
                    format!(
                        "node `{}` is already a member of group `{}`",
                        node.as_str(),
                        group.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Group {
                    graph: self.working.id.clone(),
                    group,
                }),
            );
        }
        let change = GraphChange::NodeAddedToGroup {
            group: group.clone(),
            node: node.clone(),
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::RemoveNodeFromGroup { group, node }),
        }
    }

    fn remove_node_from_group(&mut self, group: GroupId, node: NodeId) -> GraphCommandResult {
        let Some(existing_group) = self.working.groups.get_mut(&group) else {
            return self.fail(group_not_found(&self.working.id, &group));
        };
        if !existing_group.nodes.remove(&node) {
            return self.fail(
                Diagnostic::error(
                    "graph.group_member_not_found",
                    format!(
                        "node `{}` is not a member of group `{}`",
                        node.as_str(),
                        group.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Group {
                    graph: self.working.id.clone(),
                    group,
                }),
            );
        }
        let change = GraphChange::NodeRemovedFromGroup {
            group: group.clone(),
            node: node.clone(),
        };
        self.changes.push(change.clone());
        GraphCommandResult {
            changes: vec![change],
            diagnostics: Vec::new(),
            inverse: Some(GraphCommand::AddNodeToGroup { group, node }),
        }
    }
}

fn node_not_found(graph: &GraphId, node: &NodeId) -> Diagnostic {
    Diagnostic::error(
        "graph.node_not_found",
        format!("node `{}` not found", node.as_str()),
    )
    .with_target(DiagnosticTarget::Node {
        graph: graph.clone(),
        node: node.clone(),
    })
}

fn group_not_found(graph: &GraphId, group: &GroupId) -> Diagnostic {
    Diagnostic::error(
        "graph.group_not_found",
        format!("group `{}` not found", group.as_str()),
    )
    .with_target(DiagnosticTarget::Group {
        graph: graph.clone(),
        group: group.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_ULID_SUFFIX: &str = "01234567890ABCDEFGHJKMNPQR";

    struct TestSchemas {
        schemas: BTreeMap<NodeTypeId, NodeSchema>,
        input: PortId,
        output: PortId,
    }

    impl GraphSchemaRegistry for TestSchemas {
        fn node_schema(&self, node_type: &NodeTypeId) -> Option<&NodeSchema> {
            self.schemas.get(node_type)
        }
    }

    fn test_schemas() -> TestSchemas {
        let input = PortId::generate();
        let output = PortId::generate();
        let node_type = NodeTypeId::new("test.node");
        let value_type = PortValueTypeId::new("core.flow");
        let mut ports = BTreeMap::new();
        ports.insert(
            input.clone(),
            PortSchema {
                id: input.clone(),
                name: "in".into(),
                display_name: "In".into(),
                description: "Input".into(),
                direction: PortDirection::Input,
                value_type: value_type.clone(),
                arity: PortArity::optional_single(),
            },
        );
        ports.insert(
            output.clone(),
            PortSchema {
                id: output.clone(),
                name: "out".into(),
                display_name: "Out".into(),
                description: "Output".into(),
                direction: PortDirection::Output,
                value_type,
                arity: PortArity::many(),
            },
        );
        let schema = NodeSchema {
            node_type: node_type.clone(),
            compatible_graph_kinds: BTreeSet::from([GraphKind::new("test.graph")]),
            display_name: "Test Node".into(),
            description: "A test node.".into(),
            category: "Test".into(),
            search_tags: vec!["test".into()],
            property_schema: Value::Object(BTreeMap::new()),
            ports,
            version: 1,
        };
        TestSchemas {
            schemas: BTreeMap::from([(node_type, schema)]),
            input,
            output,
        }
    }

    fn graph() -> Graph {
        Graph::new(
            GraphId::generate(),
            GraphKind::new("test.graph"),
            "test_graph",
        )
    }

    fn node(id: NodeId) -> Node {
        Node::new(
            id,
            NodeTypeId::new("test.node"),
            Value::Object(BTreeMap::new()),
        )
    }

    fn graph_with_two_nodes() -> (Graph, TestSchemas, NodeId, NodeId) {
        let mut graph = graph();
        let schemas = test_schemas();
        let first = NodeId::generate();
        let second = NodeId::generate();
        graph.nodes.insert(first.clone(), node(first.clone()));
        graph.nodes.insert(second.clone(), node(second.clone()));
        (graph, schemas, first, second)
    }

    #[test]
    fn graph_can_be_constructed() {
        let graph = graph();
        assert_eq!(graph.nodes.len(), 0);
        assert_eq!(graph.kind.as_str(), "test.graph");
    }

    #[test]
    fn node_can_be_added_without_coordinates() {
        let mut graph = graph();
        let schemas = test_schemas();
        let id = NodeId::generate();
        let mut tx = GraphTransaction::begin(&graph);
        tx.apply(GraphCommand::AddNode {
            node: node(id.clone()),
        });
        tx.commit(&mut graph, &schemas)
            .expect("valid graph command must commit");
        assert!(graph.nodes.contains_key(&id));
    }

    #[test]
    fn graph_annotation_command_is_transactional_and_invertible() {
        let mut graph = graph();
        let schemas = test_schemas();
        let mut transaction = GraphTransaction::begin(&graph);
        let result = transaction.apply(GraphCommand::SetGraphAnnotation {
            key: "test.settings".to_owned(),
            value: Some(Value::String("enabled".to_owned())),
        });
        let inverse = result
            .inverse
            .expect("annotation edit must have an inverse");
        transaction
            .commit(&mut graph, &schemas)
            .expect("annotation edit must commit");
        assert_eq!(
            graph.annotations.get("test.settings"),
            Some(&Value::String("enabled".to_owned()))
        );

        let mut inverse_transaction = GraphTransaction::begin(&graph);
        inverse_transaction.apply(inverse);
        inverse_transaction
            .commit(&mut graph, &schemas)
            .expect("inverse annotation edit must commit");

        assert!(!graph.annotations.contains_key("test.settings"));
    }

    #[test]
    fn valid_ports_can_be_connected() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        let edge = Edge::new(
            EdgeId::generate(),
            PortRef::new(first, schemas.output.clone()),
            PortRef::new(second, schemas.input.clone()),
        );
        graph.edges.insert(edge.id.clone(), edge);
        assert!(graph.validate(&schemas).is_empty());
    }

    #[test]
    fn missing_node_reports_stable_diagnostic() {
        let (mut graph, schemas, first, _) = graph_with_two_nodes();
        let edge_id = EdgeId::generate();
        graph.edges.insert(
            edge_id.clone(),
            Edge::new(
                edge_id,
                PortRef::new(first, schemas.output.clone()),
                PortRef::new(NodeId::generate(), schemas.input.clone()),
            ),
        );
        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.missing_endpoint_node"));
    }

    #[test]
    fn unknown_node_type_reports_stable_diagnostic() {
        let mut graph = graph();
        let schemas = test_schemas();
        let id = NodeId::generate();
        graph.nodes.insert(
            id.clone(),
            Node::new(
                id,
                NodeTypeId::new("test.unknown"),
                Value::Object(BTreeMap::new()),
            ),
        );

        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.unknown_node_type"));
    }

    #[test]
    fn missing_port_reports_stable_diagnostic() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        let edge_id = EdgeId::generate();
        graph.edges.insert(
            edge_id.clone(),
            Edge::new(
                edge_id,
                PortRef::new(first, schemas.output.clone()),
                PortRef::new(second, PortId::generate()),
            ),
        );
        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.missing_endpoint_port"));
    }

    #[test]
    fn missing_group_member_node_reports_stable_diagnostic() {
        let mut graph = graph();
        let schemas = test_schemas();
        let group_id = GroupId::generate();
        let mut group = Group::new(group_id.clone(), "group");
        group.nodes.insert(NodeId::generate());
        graph.groups.insert(group_id, group);

        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.group_missing_node"));
    }

    #[test]
    fn invalid_endpoint_reports_stable_diagnostic() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        let edge_id = EdgeId::generate();
        graph.edges.insert(
            edge_id.clone(),
            Edge::new(
                edge_id,
                PortRef::new(first, schemas.input.clone()),
                PortRef::new(second, schemas.output.clone()),
            ),
        );
        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.invalid_endpoint_direction"));
    }

    #[test]
    fn duplicate_edge_is_rejected() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        let from = PortRef::new(first, schemas.output.clone());
        let to = PortRef::new(second, schemas.input.clone());
        let edge_a = Edge::new(EdgeId::generate(), from.clone(), to.clone());
        let edge_b = Edge::new(EdgeId::generate(), from, to);
        graph.edges.insert(edge_a.id.clone(), edge_a);
        graph.edges.insert(edge_b.id.clone(), edge_b);
        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.duplicate_edge"));
    }

    #[test]
    fn arity_violation_is_rejected() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        let third = NodeId::generate();
        graph.nodes.insert(third.clone(), node(third.clone()));
        let first_edge_id = EdgeId::generate();
        graph.edges.insert(
            first_edge_id.clone(),
            Edge::new(
                first_edge_id,
                PortRef::new(first.clone(), schemas.output.clone()),
                PortRef::new(second.clone(), schemas.input.clone()),
            ),
        );
        let second_edge_id = EdgeId::generate();
        graph.edges.insert(
            second_edge_id.clone(),
            Edge::new(
                second_edge_id,
                PortRef::new(third, schemas.output.clone()),
                PortRef::new(second, schemas.input.clone()),
            ),
        );
        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.port_arity_violation"));
    }

    #[test]
    fn deleting_node_removes_incident_edges_and_group_membership() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        let group_id = GroupId::generate();
        let mut group = Group::new(group_id.clone(), "group");
        group.nodes.insert(first.clone());
        graph.groups.insert(group_id.clone(), group);
        let edge = Edge::new(
            EdgeId::generate(),
            PortRef::new(first.clone(), schemas.output.clone()),
            PortRef::new(second, schemas.input.clone()),
        );
        graph.edges.insert(edge.id.clone(), edge);
        let mut tx = GraphTransaction::begin(&graph);
        tx.apply(GraphCommand::DeleteNode {
            node: first.clone(),
        });
        tx.commit(&mut graph, &schemas)
            .expect("delete node command must commit");
        assert!(!graph.nodes.contains_key(&first));
        assert!(graph.edges.is_empty());
        assert!(!graph
            .groups
            .get(&group_id)
            .expect("group must remain")
            .nodes
            .contains(&first));
    }

    #[test]
    fn graph_is_valid_without_a_view_file() {
        let graph = graph();
        let schemas = test_schemas();
        assert!(graph.validate(&schemas).is_empty());
    }

    #[test]
    fn graph_serialization_is_deterministic() {
        let (graph, schemas, _, _) = graph_with_two_nodes();
        let first = graph
            .to_canonical_json(&schemas)
            .expect("valid graph must serialize");
        let second = graph
            .to_canonical_json(&schemas)
            .expect("valid graph must serialize");
        assert_eq!(first, second);
    }

    #[test]
    fn graph_roundtrip_preserves_semantics() {
        let (graph, schemas, _, _) = graph_with_two_nodes();
        let json = graph
            .to_canonical_json(&schemas)
            .expect("valid graph must serialize");
        let loaded: Graph = serde_json::from_str(&json).expect("graph JSON must load");
        assert_eq!(graph.id, loaded.id);
        assert_eq!(graph.nodes, loaded.nodes);
        assert!(loaded.validate(&schemas).is_empty());
    }

    #[test]
    fn serde_roundtrip_assigns_new_in_memory_identity() {
        let (graph, schemas, _, _) = graph_with_two_nodes();
        let json = graph
            .to_canonical_json(&schemas)
            .expect("valid graph must serialize");
        let loaded: Graph = serde_json::from_str(&json).expect("graph JSON must load");

        assert_eq!(graph.id, loaded.id);
        assert_ne!(graph.identity(), loaded.identity());
        assert_eq!(loaded.revision(), 0);
    }

    #[test]
    fn graph_save_rejects_non_finite_semantic_values() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        let edge = Edge::new(
            EdgeId::generate(),
            PortRef::new(first.clone(), schemas.output.clone()),
            PortRef::new(second, schemas.input.clone()),
        );
        let edge_id = edge.id.clone();
        graph.edges.insert(edge_id.clone(), edge);
        let group_id = GroupId::generate();
        graph
            .groups
            .insert(group_id.clone(), Group::new(group_id.clone(), "group"));

        graph
            .annotations
            .insert("bad_graph".into(), Value::F64(f64::NAN));
        graph.nodes.get_mut(&first).unwrap().properties = Value::Object(BTreeMap::from([(
            "nested".into(),
            Value::Array(vec![Value::F64(f64::INFINITY)]),
        )]));
        graph
            .nodes
            .get_mut(&first)
            .unwrap()
            .annotations
            .insert("bad_node".into(), Value::F64(f64::NEG_INFINITY));
        graph
            .edges
            .get_mut(&edge_id)
            .unwrap()
            .annotations
            .insert("bad_edge".into(), Value::F64(f64::NAN));
        graph
            .groups
            .get_mut(&group_id)
            .unwrap()
            .annotations
            .insert("bad_group".into(), Value::F64(f64::INFINITY));

        let diagnostics = graph.validate(&schemas);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "graph.non_finite_number")
                .count(),
            5
        );
        assert!(matches!(
            graph.to_canonical_json(&schemas),
            Err(GraphSaveError::ValidationFailed { .. })
        ));
    }

    #[test]
    fn graph_deserialize_rejects_unsupported_schema_versions() {
        let (graph, schemas, _, _) = graph_with_two_nodes();
        let json = graph
            .to_canonical_json(&schemas)
            .expect("valid graph must serialize");
        for version in [2_u32, 99] {
            let mut document: serde_json::Value =
                serde_json::from_str(&json).expect("graph JSON must parse");
            document["schema_version"] = serde_json::Value::from(version);
            assert!(
                serde_json::from_value::<Graph>(document).is_err(),
                "schema_version {version} must be rejected"
            );
        }
    }

    #[test]
    fn graph_deserialize_rejects_unknown_top_level_fields() {
        let (graph, schemas, _, _) = graph_with_two_nodes();
        let json = graph
            .to_canonical_json(&schemas)
            .expect("valid graph must serialize");
        let mut document: serde_json::Value =
            serde_json::from_str(&json).expect("graph JSON must parse");
        document
            .as_object_mut()
            .expect("graph JSON must be an object")
            .insert("future_field".into(), serde_json::Value::Bool(true));

        assert!(serde_json::from_value::<Graph>(document).is_err());
    }

    #[test]
    fn typed_ids_reject_malformed_json() {
        let json = format!(
            r#"{{
                "id": "entity_{VALID_ULID_SUFFIX}",
                "kind": "test.graph",
                "schema_version": 1,
                "name": "bad",
                "display_name": "",
                "description": "",
                "nodes": {{}},
                "edges": {{}},
                "groups": {{}},
                "annotations": {{}}
            }}"#
        );
        assert!(serde_json::from_str::<Graph>(&json).is_err());
        assert!(serde_json::from_str::<NodeTypeId>(r#""bad""#).is_err());
        assert!(serde_json::from_str::<PortValueTypeId>(r#""Bad.Type""#).is_err());
    }

    #[test]
    fn graph_validation_rejects_node_key_id_mismatch() {
        let mut graph = graph();
        let schemas = test_schemas();
        graph
            .nodes
            .insert(NodeId::generate(), node(NodeId::generate()));

        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.node_id_mismatch"));
    }

    #[test]
    fn graph_validation_rejects_edge_key_id_mismatch() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        graph.edges.insert(
            EdgeId::generate(),
            Edge::new(
                EdgeId::generate(),
                PortRef::new(first, schemas.output.clone()),
                PortRef::new(second, schemas.input.clone()),
            ),
        );

        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.edge_id_mismatch"));
    }

    #[test]
    fn graph_validation_rejects_group_key_id_mismatch() {
        let mut graph = graph();
        let schemas = test_schemas();
        graph.groups.insert(
            GroupId::generate(),
            Group::new(GroupId::generate(), "group"),
        );

        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.group_id_mismatch"));
    }

    #[test]
    fn graph_validation_rejects_port_schema_key_id_mismatch() {
        let mut graph = graph();
        let mut schemas = test_schemas();
        let node_id = NodeId::generate();
        graph.nodes.insert(node_id.clone(), node(node_id));
        let schema = schemas
            .schemas
            .get_mut(&NodeTypeId::new("test.node"))
            .expect("test schema must exist");
        let key = schemas.input.clone();
        let mut port = schema
            .ports
            .remove(&key)
            .expect("test input port must exist");
        port.id = PortId::generate();
        schema.ports.insert(key, port);

        assert!(graph
            .validate(&schemas)
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.port_schema_id_mismatch"));
    }

    #[test]
    fn graph_command_commit_succeeds() {
        let mut graph = graph();
        let schemas = test_schemas();
        let id = NodeId::generate();
        let mut tx = GraphTransaction::begin(&graph);
        let result = tx.apply(GraphCommand::AddNode {
            node: node(id.clone()),
        });
        assert!(result.is_ok());
        assert_eq!(tx.preview_diff().len(), 1);
        let changes = tx
            .commit(&mut graph, &schemas)
            .expect("valid graph command must commit");
        assert_eq!(changes.len(), 1);
        assert!(graph.nodes.contains_key(&id));
    }

    #[test]
    fn graph_command_rollback_leaves_graph_unchanged() {
        let graph = graph();
        let id = NodeId::generate();
        let mut tx = GraphTransaction::begin(&graph);
        tx.apply(GraphCommand::AddNode { node: node(id) });
        tx.rollback();
        assert!(graph.nodes.is_empty());
    }

    #[test]
    fn graph_transaction_uses_private_working_snapshot() {
        let mut graph = graph();
        let schemas = test_schemas();
        let base_identity = graph.identity();
        let mut tx = GraphTransaction::begin(&graph);
        tx.apply(GraphCommand::AddNode {
            node: node(NodeId::generate()),
        });

        tx.commit(&mut graph, &schemas)
            .expect("transaction must commit through its private snapshot");
        assert_eq!(graph.identity(), base_identity);
        assert_eq!(graph.revision(), 1);
    }

    #[test]
    fn failed_commit_leaves_graph_unchanged() {
        let mut graph = graph();
        let schemas = test_schemas();
        let id = NodeId::generate();
        let mut tx = GraphTransaction::begin(&graph);
        tx.apply(GraphCommand::AddNode {
            node: Node::new(
                id,
                NodeTypeId::new("test.unknown"),
                Value::Object(BTreeMap::new()),
            ),
        });
        assert!(matches!(
            tx.commit(&mut graph, &schemas),
            Err(GraphTransactionError::ValidationFailed { .. })
        ));
        assert!(graph.nodes.is_empty());
    }

    #[test]
    fn commit_rejects_unrelated_graph_identity() {
        let source = graph();
        let mut unrelated = graph();
        let schemas = test_schemas();
        let mut tx = GraphTransaction::begin(&source);
        tx.apply(GraphCommand::AddNode {
            node: node(NodeId::generate()),
        });

        assert!(matches!(
            tx.commit(&mut unrelated, &schemas),
            Err(GraphTransactionError::Conflict { .. })
        ));
        assert!(unrelated.nodes.is_empty());
    }

    #[test]
    fn stale_revision_commit_is_rejected() {
        let mut graph = graph();
        let schemas = test_schemas();
        let stale = GraphTransaction::begin(&graph);
        let mut current = GraphTransaction::begin(&graph);
        current.apply(GraphCommand::AddNode {
            node: node(NodeId::generate()),
        });
        current
            .commit(&mut graph, &schemas)
            .expect("current transaction must commit");
        assert!(matches!(
            stale.commit(&mut graph, &schemas),
            Err(GraphTransactionError::Conflict { .. })
        ));
    }

    #[test]
    fn delete_node_removes_malformed_incident_edge_by_map_key() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        graph.edges.insert(
            EdgeId::generate(),
            Edge::new(
                EdgeId::generate(),
                PortRef::new(first.clone(), schemas.output.clone()),
                PortRef::new(second, schemas.input.clone()),
            ),
        );
        let mut tx = GraphTransaction::begin(&graph);
        tx.apply(GraphCommand::DeleteNode { node: first });

        tx.commit(&mut graph, &schemas)
            .expect("deleting the malformed incident edge should restore validity");
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn delete_edge_removes_malformed_edge_by_map_key() {
        let (mut graph, schemas, first, second) = graph_with_two_nodes();
        let edge_key = EdgeId::generate();
        graph.edges.insert(
            edge_key.clone(),
            Edge::new(
                EdgeId::generate(),
                PortRef::new(first, schemas.output.clone()),
                PortRef::new(second, schemas.input.clone()),
            ),
        );
        let mut tx = GraphTransaction::begin(&graph);
        tx.apply(GraphCommand::DeleteEdge { edge: edge_key });

        tx.commit(&mut graph, &schemas)
            .expect("deleting the malformed edge should restore validity");
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn diagnostic_codes_remain_stable() {
        let codes = [
            "graph.unknown_node_type",
            "graph.incompatible_node_kind",
            "graph.invalid_node_properties",
            "graph.node_id_mismatch",
            "graph.edge_id_mismatch",
            "graph.group_id_mismatch",
            "graph.port_schema_id_mismatch",
            "graph.missing_endpoint_node",
            "graph.missing_endpoint_port",
            "graph.invalid_endpoint_direction",
            "graph.duplicate_edge",
            "graph.port_arity_violation",
            "graph.group_missing_node",
            "graph.node_not_found",
            "graph.node_already_exists",
            "graph.edge_not_found",
            "graph.edge_already_exists",
            "graph.group_not_found",
            "graph.group_already_exists",
            "graph.group_member_already_exists",
            "graph.group_member_not_found",
        ];
        assert_eq!(codes.len(), 21);
    }
}
