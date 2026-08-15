//! Behavior Tree graph domain for Phase 4.
//!
//! This module implements the first concrete graph domain on top of the
//! domain-neutral graph foundation. It owns Behavior Tree schemas,
//! domain-specific validation, deterministic compilation, and a simple
//! top-down layout policy.

use crate::diagnostic::{Diagnostic, DiagnosticTarget};
use crate::graph::{
    Edge, Graph, GraphChange, GraphCommand, GraphKind, GraphSaveError, GraphSchemaRegistry, Node,
    NodeSchema, NodeTypeId, PortArity, PortDirection, PortRef, PortSchema, PortValueTypeId,
};
use crate::graph_domain::{
    apply_graph_commands_with_domain, validate_graph_with_domain, GraphCommandApplication,
    GraphDomain,
};
use crate::graph_view::{GraphView, LayoutPolicyId, NodeLayout, Vec2};
use crate::id::{EdgeId, GraphId, NodeId, PortId, StableId};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

const CHILD_ORDER_KEY: &str = "behavior_tree.order";
const BEHAVIOR_KEY: &str = "behavior";
const MAX_BEHAVIOR_TREE_DEPTH: u32 = 1024;
const NODE_HORIZONTAL_SPACING: f64 = 180.0;
const NODE_VERTICAL_SPACING: f64 = 120.0;

/// Behavior Tree node kind used by compiled runtime representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorTreeNodeKind {
    /// The single semantic root node.
    Root,
    /// A composite that evaluates children in order until one fails.
    Sequence,
    /// A composite that evaluates children in order until one succeeds.
    Selector,
    /// A leaf condition node.
    Condition,
    /// A leaf action node.
    Action,
    /// A single-child decorator node.
    Decorator,
}

/// A compiled Behavior Tree node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledBehaviorNode {
    /// Source authoring node identifier.
    pub source: NodeId,
    /// Runtime node kind.
    pub kind: BehaviorTreeNodeKind,
    /// Optional stable behavior identifier for action, condition, or decorator nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior: Option<String>,
    /// Ordered child nodes.
    pub children: Vec<CompiledBehaviorNode>,
}

/// A deterministic compiled Behavior Tree representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledBehaviorTree {
    /// Source authoring graph identifier.
    pub source: GraphId,
    /// Root runtime node.
    pub root: CompiledBehaviorNode,
}

/// Schema discovery output for Behavior Tree authoring adapters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorTreeSchemaCatalog {
    /// Graph kind supported by this domain.
    pub graph_kind: GraphKind,
    /// Default layout policy for generated graph views.
    pub layout_policy: LayoutPolicyId,
    /// Node schemas in deterministic authoring-tool order.
    pub nodes: Vec<NodeSchema>,
}

/// Query summary for a Behavior Tree node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorTreeNodeSummary {
    /// Stable graph-local node identifier.
    pub id: NodeId,
    /// Behavior Tree node type identifier.
    pub node_type: NodeTypeId,
    /// Optional AI-searchable node slug.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Domain-owned node properties.
    pub properties: Value,
}

/// Query summary for a Behavior Tree edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorTreeEdgeSummary {
    /// Stable graph-local edge identifier.
    pub id: EdgeId,
    /// Output endpoint.
    pub from: PortRef,
    /// Input endpoint.
    pub to: PortRef,
}

/// Validation result returned by the Behavior Tree authoring service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorTreeValidation {
    /// `true` when no blocking diagnostics were produced.
    pub success: bool,
    /// Structural and domain diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Compilation result returned by the Behavior Tree authoring service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorTreeCompilation {
    /// `true` when compilation produced a runtime artifact.
    pub success: bool,
    /// Diagnostics produced before or during compilation.
    pub diagnostics: Vec<Diagnostic>,
    /// Compiled runtime tree when `success` is `true`.
    pub compiled_tree: Option<CompiledBehaviorTree>,
}

/// Layout result returned by the Behavior Tree authoring service.
#[derive(Debug, Serialize, Deserialize)]
pub struct BehaviorTreeLayout {
    /// `true` when layout produced a graph view.
    pub success: bool,
    /// Diagnostics produced before or during layout.
    pub diagnostics: Vec<Diagnostic>,
    /// Generated graph view when `success` is `true`.
    pub graph_view: Option<GraphView>,
}

/// Result of applying Behavior Tree graph commands.
#[derive(Debug, Serialize)]
pub struct BehaviorTreeApply {
    /// `true` when commands passed structural and domain validation.
    pub success: bool,
    /// Diagnostics produced during command application and validation.
    pub diagnostics: Vec<Diagnostic>,
    /// Semantic changes produced by the command transaction.
    pub diff: Vec<GraphChange>,
    /// Validated graph after applying commands when `success` is `true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph: Option<Box<Graph>>,
}

impl BehaviorTreeApply {
    /// Converts the domain-neutral graph command application result.
    pub fn from_application(application: GraphCommandApplication) -> Self {
        match application {
            GraphCommandApplication::Applied {
                diagnostics,
                diff,
                graph,
            } => Self {
                success: true,
                diagnostics,
                diff,
                graph: Some(graph),
            },
            GraphCommandApplication::Rejected { diagnostics, diff } => Self {
                success: false,
                diagnostics,
                diff,
                graph: None,
            },
        }
    }

    /// Returns the validated graph when command application succeeded.
    pub fn graph(&self) -> Option<&Graph> {
        self.graph.as_deref()
    }
}

/// Reference Behavior Tree scenario used by CLI, MCP, and tests.
#[derive(Debug, Serialize)]
pub struct BehaviorTreeExample {
    /// Built semantic graph.
    pub graph: Graph,
    /// Diff observed before commit.
    pub preview_diff: Vec<GraphChange>,
    /// Diff committed to the graph.
    pub commit_diff: Vec<GraphChange>,
    /// Validation diagnostics for the committed graph.
    pub diagnostics: Vec<Diagnostic>,
    /// Compiled runtime tree.
    pub compiled: CompiledBehaviorTree,
    /// Generated graph view.
    pub view: GraphView,
}

/// Reports why a Behavior Tree authoring service operation failed.
#[derive(Debug)]
pub enum BehaviorTreeServiceError {
    /// Input JSON could not be deserialized.
    Json {
        /// Source JSON error.
        source: serde_json::Error,
    },
    /// The input graph belongs to another graph domain.
    WrongDomain {
        /// Expected graph kind.
        expected: GraphKind,
        /// Actual graph kind.
        actual: GraphKind,
    },
    /// Structured diagnostics blocked the operation.
    Diagnostics {
        /// Diagnostics produced by the authoring operation.
        diagnostics: Vec<Diagnostic>,
    },
    /// A graph transaction failed.
    Transaction {
        /// Source transaction error.
        source: crate::graph::GraphTransactionError,
    },
    /// Graph serialization failed.
    Save {
        /// Source graph save error.
        source: GraphSaveError,
    },
}

impl fmt::Display for BehaviorTreeServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json { source } => write!(formatter, "behavior tree JSON failed: {source}"),
            Self::WrongDomain { expected, actual } => write!(
                formatter,
                "expected behavior tree graph kind `{expected}`, found `{actual}`"
            ),
            Self::Diagnostics { diagnostics } => write!(
                formatter,
                "behavior tree operation produced {} blocking diagnostic(s)",
                diagnostics.len()
            ),
            Self::Transaction { source } => {
                write!(formatter, "behavior tree transaction failed: {source}")
            }
            Self::Save { source } => write!(formatter, "behavior tree graph save failed: {source}"),
        }
    }
}

impl std::error::Error for BehaviorTreeServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json { source } => Some(source),
            Self::Transaction { source } => Some(source),
            Self::Save { source } => Some(source),
            Self::WrongDomain { .. } | Self::Diagnostics { .. } => None,
        }
    }
}

/// Shared Behavior Tree authoring service used by CLI, MCP, editor, and tests.
///
/// The service owns no transport or persistence policy. It delegates schema
/// lookup, transactions, validation, compilation, and layout to the
/// Behavior Tree domain and graph foundation.
pub struct BehaviorTreeAuthoringService {
    domain: BehaviorTreeDomain,
}

impl BehaviorTreeAuthoringService {
    /// Creates a service with the production Behavior Tree domain.
    pub fn new() -> Self {
        Self {
            domain: BehaviorTreeDomain::new(),
        }
    }

    /// Returns the underlying Behavior Tree domain.
    pub fn domain(&self) -> &BehaviorTreeDomain {
        &self.domain
    }

    /// Returns schemas needed by authoring tools to create valid nodes.
    pub fn schemas(&self) -> BehaviorTreeSchemaCatalog {
        let node_types = [
            self.domain.root_type(),
            self.domain.sequence_type(),
            self.domain.selector_type(),
            self.domain.condition_type(),
            self.domain.action_type(),
            self.domain.decorator_type(),
        ];
        let nodes = node_types
            .into_iter()
            .filter_map(|node_type| {
                self.domain
                    .schema_registry()
                    .node_schema(node_type)
                    .cloned()
            })
            .collect();

        BehaviorTreeSchemaCatalog {
            graph_kind: self.domain.graph_kind().clone(),
            layout_policy: self.domain.layout_policy(),
            nodes,
        }
    }

    /// Builds the reference chase-or-patrol Behavior Tree scenario.
    ///
    /// # Errors
    ///
    /// Returns [`BehaviorTreeServiceError`] if the graph transaction,
    /// validation, compilation, or layout step fails.
    pub fn example(&self) -> Result<BehaviorTreeExample, BehaviorTreeServiceError> {
        let mut graph = Graph::new(
            GraphId::generate(),
            self.domain.graph_kind().clone(),
            "enemy_behavior",
        );
        graph.display_name = "Enemy Behavior".into();
        graph.description = "Chase the player when visible, otherwise patrol.".into();

        let root = NodeId::generate();
        let selector = NodeId::generate();
        let sequence = NodeId::generate();
        let visible = NodeId::generate();
        let chase = NodeId::generate();
        let patrol = NodeId::generate();

        let mut transaction = crate::graph::GraphTransaction::begin(&graph);
        transaction.apply(GraphCommand::AddNode {
            node: self.domain.root_node(root.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: self.domain.selector_node(selector.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: self.domain.sequence_node(sequence.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: self
                .domain
                .condition_node(visible.clone(), "player_visible"),
        });
        transaction.apply(GraphCommand::AddNode {
            node: self.domain.action_node(chase.clone(), "chase_player"),
        });
        transaction.apply(GraphCommand::AddNode {
            node: self.domain.action_node(patrol.clone(), "patrol"),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: self
                .domain
                .child_edge(EdgeId::generate(), root, selector.clone(), 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: self
                .domain
                .child_edge(EdgeId::generate(), selector.clone(), sequence.clone(), 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: self
                .domain
                .child_edge(EdgeId::generate(), selector, patrol, 1),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: self
                .domain
                .child_edge(EdgeId::generate(), sequence.clone(), visible, 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: self
                .domain
                .child_edge(EdgeId::generate(), sequence, chase, 1),
        });

        let preview_diff = transaction.preview_diff().to_vec();
        let commit_diff = transaction
            .commit(&mut graph, self.domain.schema_registry())
            .map_err(|source| BehaviorTreeServiceError::Transaction { source })?;
        let validation = self.validate(&graph);
        if validation.diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(BehaviorTreeServiceError::Diagnostics {
                diagnostics: validation.diagnostics,
            });
        }
        let compiled = self.compile(&graph);
        let Some(compiled_tree) = compiled.compiled_tree else {
            return Err(BehaviorTreeServiceError::Diagnostics {
                diagnostics: compiled.diagnostics,
            });
        };
        let layout = self.layout(&graph);
        let Some(view) = layout.graph_view else {
            return Err(BehaviorTreeServiceError::Diagnostics {
                diagnostics: layout.diagnostics,
            });
        };

        Ok(BehaviorTreeExample {
            graph,
            preview_diff,
            commit_diff,
            diagnostics: validation.diagnostics,
            compiled: compiled_tree,
            view,
        })
    }

    /// Parses a Behavior Tree graph JSON document.
    ///
    /// # Errors
    ///
    /// Returns [`BehaviorTreeServiceError::Json`] for invalid JSON or
    /// [`BehaviorTreeServiceError::WrongDomain`] when the graph kind does not
    /// match this domain.
    pub fn graph_from_json(&self, json: &str) -> Result<Graph, BehaviorTreeServiceError> {
        let graph = serde_json::from_str::<Graph>(json)
            .map_err(|source| BehaviorTreeServiceError::Json { source })?;
        self.ensure_behavior_tree_graph(&graph)?;
        Ok(graph)
    }

    /// Parses a JSON array of graph commands.
    ///
    /// # Errors
    ///
    /// Returns [`BehaviorTreeServiceError::Json`] when input is not a valid
    /// `Vec<GraphCommand>` document.
    pub fn commands_from_json(
        &self,
        json: &str,
    ) -> Result<Vec<GraphCommand>, BehaviorTreeServiceError> {
        serde_json::from_str(json).map_err(|source| BehaviorTreeServiceError::Json { source })
    }

    /// Serializes a Behavior Tree graph to canonical JSON.
    ///
    /// # Errors
    ///
    /// Returns [`BehaviorTreeServiceError`] when the graph belongs to another
    /// domain or cannot be serialized after structural validation.
    pub fn graph_to_canonical_json(
        &self,
        graph: &Graph,
    ) -> Result<String, BehaviorTreeServiceError> {
        self.ensure_behavior_tree_graph(graph)?;
        graph
            .to_canonical_json(self.domain.schema_registry())
            .map_err(|source| BehaviorTreeServiceError::Save { source })
    }

    /// Verifies that `graph` belongs to the Behavior Tree domain.
    ///
    /// # Errors
    ///
    /// Returns [`BehaviorTreeServiceError::WrongDomain`] when the graph kind
    /// does not match this service's domain.
    pub fn ensure_behavior_tree_graph(
        &self,
        graph: &Graph,
    ) -> Result<(), BehaviorTreeServiceError> {
        if graph.kind == *self.domain.graph_kind() {
            Ok(())
        } else {
            Err(BehaviorTreeServiceError::WrongDomain {
                expected: self.domain.graph_kind().clone(),
                actual: graph.kind.clone(),
            })
        }
    }

    /// Validates a Behavior Tree graph.
    pub fn validate(&self, graph: &Graph) -> BehaviorTreeValidation {
        let diagnostics = validate_graph_with_domain(graph, &self.domain);
        let success = !diagnostics.iter().any(Diagnostic::is_blocking);
        BehaviorTreeValidation {
            success,
            diagnostics,
        }
    }

    /// Compiles a Behavior Tree graph into a runtime tree artifact.
    pub fn compile(&self, graph: &Graph) -> BehaviorTreeCompilation {
        match self.domain.compile(graph) {
            Ok(compiled_tree) => BehaviorTreeCompilation {
                success: true,
                diagnostics: Vec::new(),
                compiled_tree: Some(compiled_tree),
            },
            Err(diagnostics) => BehaviorTreeCompilation {
                success: false,
                diagnostics,
                compiled_tree: None,
            },
        }
    }

    /// Generates a deterministic graph view for a Behavior Tree graph.
    pub fn layout(&self, graph: &Graph) -> BehaviorTreeLayout {
        match self.domain.auto_layout(graph) {
            Ok(graph_view) => BehaviorTreeLayout {
                success: true,
                diagnostics: Vec::new(),
                graph_view: Some(graph_view),
            },
            Err(diagnostics) => BehaviorTreeLayout {
                success: false,
                diagnostics,
                graph_view: None,
            },
        }
    }

    /// Returns deterministic node summaries for a Behavior Tree graph.
    pub fn nodes(&self, graph: &Graph) -> Vec<BehaviorTreeNodeSummary> {
        graph
            .nodes
            .values()
            .map(|node| BehaviorTreeNodeSummary {
                id: node.id.clone(),
                node_type: node.node_type.clone(),
                name: node.name.clone(),
                properties: node.properties.clone(),
            })
            .collect()
    }

    /// Returns deterministic edge summaries for a Behavior Tree graph.
    pub fn edges(&self, graph: &Graph) -> Vec<BehaviorTreeEdgeSummary> {
        graph
            .edges
            .values()
            .map(|edge| BehaviorTreeEdgeSummary {
                id: edge.id.clone(),
                from: edge.from.clone(),
                to: edge.to.clone(),
            })
            .collect()
    }

    /// Applies graph commands to a private copy and validates the result.
    pub fn apply(
        &self,
        graph: &Graph,
        commands: impl IntoIterator<Item = GraphCommand>,
    ) -> BehaviorTreeApply {
        BehaviorTreeApply::from_application(apply_graph_commands_with_domain(
            graph,
            &self.domain,
            commands,
        ))
    }
}

impl Default for BehaviorTreeAuthoringService {
    fn default() -> Self {
        Self::new()
    }
}

/// Phase 4 production Behavior Tree graph domain.
pub struct BehaviorTreeDomain {
    graph_kind: GraphKind,
    root_type: NodeTypeId,
    sequence_type: NodeTypeId,
    selector_type: NodeTypeId,
    condition_type: NodeTypeId,
    action_type: NodeTypeId,
    decorator_type: NodeTypeId,
    parent_in: PortId,
    child_out: PortId,
    schemas: BTreeMap<NodeTypeId, NodeSchema>,
}

fn fixed_port_id(suffix: &str) -> PortId {
    PortId::from_stable_id(StableId::new(format!("port_{suffix}")))
        .expect("behavior tree port ID must use a valid stable ID")
}

fn empty_object() -> Value {
    Value::Object(BTreeMap::new())
}

fn behavior_property_schema() -> Value {
    Value::Object(BTreeMap::from([(
        BEHAVIOR_KEY.into(),
        Value::String("stable behavior identifier".into()),
    )]))
}

impl BehaviorTreeDomain {
    /// Creates the Behavior Tree domain with stable in-memory schemas.
    pub fn new() -> Self {
        let graph_kind = GraphKind::new("behavior_tree.graph");
        let root_type = NodeTypeId::new("behavior_tree.root");
        let sequence_type = NodeTypeId::new("behavior_tree.sequence");
        let selector_type = NodeTypeId::new("behavior_tree.selector");
        let condition_type = NodeTypeId::new("behavior_tree.condition");
        let action_type = NodeTypeId::new("behavior_tree.action");
        let decorator_type = NodeTypeId::new("behavior_tree.decorator");
        let parent_in = fixed_port_id("01234567890ABCDEFGHJKMNS00");
        let child_out = fixed_port_id("01234567890ABCDEFGHJKMNS01");
        let node_type = PortValueTypeId::new("behavior_tree.node");
        let compatible_graph_kinds = BTreeSet::from([graph_kind.clone()]);

        let child_output = |id: &PortId| PortSchema {
            id: id.clone(),
            name: "children".into(),
            display_name: "Children".into(),
            description: "Ordered child behavior nodes.".into(),
            direction: PortDirection::Output,
            value_type: node_type.clone(),
            arity: PortArity::many(),
        };
        let parent_input = |id: &PortId| PortSchema {
            id: id.clone(),
            name: "parent".into(),
            display_name: "Parent".into(),
            description: "Parent behavior node.".into(),
            direction: PortDirection::Input,
            value_type: node_type.clone(),
            arity: PortArity::optional_single(),
        };
        let schemas = BTreeMap::from([
            (
                root_type.clone(),
                NodeSchema {
                    node_type: root_type.clone(),
                    compatible_graph_kinds: compatible_graph_kinds.clone(),
                    display_name: "Root".into(),
                    description: "Single Behavior Tree root.".into(),
                    category: "Behavior Tree".into(),
                    search_tags: vec!["root".into()],
                    property_schema: empty_object(),
                    ports: BTreeMap::from([(child_out.clone(), child_output(&child_out))]),
                    version: 1,
                },
            ),
            (
                sequence_type.clone(),
                NodeSchema {
                    node_type: sequence_type.clone(),
                    compatible_graph_kinds: compatible_graph_kinds.clone(),
                    display_name: "Sequence".into(),
                    description: "Runs children in order until one fails.".into(),
                    category: "Behavior Tree/Composite".into(),
                    search_tags: vec!["sequence".into(), "composite".into()],
                    property_schema: empty_object(),
                    ports: BTreeMap::from([
                        (parent_in.clone(), parent_input(&parent_in)),
                        (child_out.clone(), child_output(&child_out)),
                    ]),
                    version: 1,
                },
            ),
            (
                selector_type.clone(),
                NodeSchema {
                    node_type: selector_type.clone(),
                    compatible_graph_kinds: compatible_graph_kinds.clone(),
                    display_name: "Selector".into(),
                    description: "Runs children in order until one succeeds.".into(),
                    category: "Behavior Tree/Composite".into(),
                    search_tags: vec!["selector".into(), "composite".into()],
                    property_schema: empty_object(),
                    ports: BTreeMap::from([
                        (parent_in.clone(), parent_input(&parent_in)),
                        (child_out.clone(), child_output(&child_out)),
                    ]),
                    version: 1,
                },
            ),
            (
                condition_type.clone(),
                NodeSchema {
                    node_type: condition_type.clone(),
                    compatible_graph_kinds: compatible_graph_kinds.clone(),
                    display_name: "Condition".into(),
                    description: "Leaf condition behavior.".into(),
                    category: "Behavior Tree/Leaf".into(),
                    search_tags: vec!["condition".into()],
                    property_schema: behavior_property_schema(),
                    ports: BTreeMap::from([(parent_in.clone(), parent_input(&parent_in))]),
                    version: 1,
                },
            ),
            (
                action_type.clone(),
                NodeSchema {
                    node_type: action_type.clone(),
                    compatible_graph_kinds: compatible_graph_kinds.clone(),
                    display_name: "Action".into(),
                    description: "Leaf action behavior.".into(),
                    category: "Behavior Tree/Leaf".into(),
                    search_tags: vec!["action".into()],
                    property_schema: behavior_property_schema(),
                    ports: BTreeMap::from([(parent_in.clone(), parent_input(&parent_in))]),
                    version: 1,
                },
            ),
            (
                decorator_type.clone(),
                NodeSchema {
                    node_type: decorator_type.clone(),
                    compatible_graph_kinds,
                    display_name: "Decorator".into(),
                    description: "Single-child decorator behavior.".into(),
                    category: "Behavior Tree/Decorator".into(),
                    search_tags: vec!["decorator".into()],
                    property_schema: behavior_property_schema(),
                    ports: BTreeMap::from([
                        (parent_in.clone(), parent_input(&parent_in)),
                        (child_out.clone(), child_output(&child_out)),
                    ]),
                    version: 1,
                },
            ),
        ]);

        Self {
            graph_kind,
            root_type,
            sequence_type,
            selector_type,
            condition_type,
            action_type,
            decorator_type,
            parent_in,
            child_out,
            schemas,
        }
    }

    /// Returns the Behavior Tree graph kind.
    pub fn graph_kind(&self) -> &GraphKind {
        &self.graph_kind
    }

    /// Returns the root node type.
    pub fn root_type(&self) -> &NodeTypeId {
        &self.root_type
    }

    /// Returns the sequence node type.
    pub fn sequence_type(&self) -> &NodeTypeId {
        &self.sequence_type
    }

    /// Returns the selector node type.
    pub fn selector_type(&self) -> &NodeTypeId {
        &self.selector_type
    }

    /// Returns the condition node type.
    pub fn condition_type(&self) -> &NodeTypeId {
        &self.condition_type
    }

    /// Returns the action node type.
    pub fn action_type(&self) -> &NodeTypeId {
        &self.action_type
    }

    /// Returns the decorator node type.
    pub fn decorator_type(&self) -> &NodeTypeId {
        &self.decorator_type
    }

    /// Returns the default Behavior Tree layout policy.
    pub fn layout_policy(&self) -> LayoutPolicyId {
        LayoutPolicyId::new("behavior_tree.top_down")
    }

    /// Creates a root node.
    pub fn root_node(&self, id: NodeId) -> Node {
        Node::new(id, self.root_type.clone(), empty_object())
    }

    /// Creates a sequence node.
    pub fn sequence_node(&self, id: NodeId) -> Node {
        Node::new(id, self.sequence_type.clone(), empty_object())
    }

    /// Creates a selector node.
    pub fn selector_node(&self, id: NodeId) -> Node {
        Node::new(id, self.selector_type.clone(), empty_object())
    }

    /// Creates a condition node with a stable behavior identifier.
    pub fn condition_node(&self, id: NodeId, behavior: impl Into<String>) -> Node {
        behavior_node(id, self.condition_type.clone(), behavior)
    }

    /// Creates an action node with a stable behavior identifier.
    pub fn action_node(&self, id: NodeId, behavior: impl Into<String>) -> Node {
        behavior_node(id, self.action_type.clone(), behavior)
    }

    /// Creates a decorator node with a stable behavior identifier.
    pub fn decorator_node(&self, id: NodeId, behavior: impl Into<String>) -> Node {
        behavior_node(id, self.decorator_type.clone(), behavior)
    }

    /// Creates a parent-to-child edge with explicit child order.
    pub fn child_edge(&self, id: EdgeId, parent: NodeId, child: NodeId, order: u32) -> Edge {
        let mut edge = Edge::new(
            id,
            PortRef::new(parent, self.child_out.clone()),
            PortRef::new(child, self.parent_in.clone()),
        );
        edge.annotations
            .insert(CHILD_ORDER_KEY.into(), Value::U64(u64::from(order)));
        edge
    }

    /// Compiles `graph` into a deterministic Behavior Tree runtime representation.
    pub fn compile(&self, graph: &Graph) -> Result<CompiledBehaviorTree, Vec<Diagnostic>> {
        let diagnostics = validate_graph_with_domain(graph, self);
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(diagnostics);
        }
        let children = child_edges_by_parent(self, graph);
        let root = self
            .root_node_id(graph)
            .expect("validated behavior tree must have one root");
        Ok(CompiledBehaviorTree {
            source: graph.id.clone(),
            root: self.compile_node(graph, &children, &root),
        })
    }

    /// Produces a deterministic top-down graph view for `graph`.
    pub fn auto_layout(&self, graph: &Graph) -> Result<GraphView, Vec<Diagnostic>> {
        let compiled = self.compile(graph)?;
        let mut view = GraphView::new(graph.id.clone());
        view.layout_policy = self.layout_policy();
        let mut leaf_index = 0_u32;
        layout_compiled_node(&compiled.root, 0, &mut leaf_index, &mut view);
        Ok(view)
    }

    fn node_kind(&self, node: &Node) -> Option<BehaviorTreeNodeKind> {
        if node.node_type == self.root_type {
            Some(BehaviorTreeNodeKind::Root)
        } else if node.node_type == self.sequence_type {
            Some(BehaviorTreeNodeKind::Sequence)
        } else if node.node_type == self.selector_type {
            Some(BehaviorTreeNodeKind::Selector)
        } else if node.node_type == self.condition_type {
            Some(BehaviorTreeNodeKind::Condition)
        } else if node.node_type == self.action_type {
            Some(BehaviorTreeNodeKind::Action)
        } else if node.node_type == self.decorator_type {
            Some(BehaviorTreeNodeKind::Decorator)
        } else {
            None
        }
    }

    fn root_node_id(&self, graph: &Graph) -> Option<NodeId> {
        graph
            .nodes
            .iter()
            .find_map(|(id, node)| (node.node_type == self.root_type).then_some(id.clone()))
    }

    fn compile_node(
        &self,
        graph: &Graph,
        children: &BTreeMap<NodeId, Vec<ChildEdge>>,
        node_id: &NodeId,
    ) -> CompiledBehaviorNode {
        let node = graph
            .nodes
            .get(node_id)
            .expect("validated behavior tree node must exist");
        CompiledBehaviorNode {
            source: node_id.clone(),
            kind: self
                .node_kind(node)
                .expect("validated behavior tree node type must be known"),
            behavior: behavior_property(node),
            children: self
                .ordered_children(children, node_id)
                .into_iter()
                .map(|(_, child)| self.compile_node(graph, children, &child))
                .collect(),
        }
    }

    fn ordered_children(
        &self,
        children: &BTreeMap<NodeId, Vec<ChildEdge>>,
        parent: &NodeId,
    ) -> Vec<(u32, NodeId)> {
        let mut ordered: Vec<_> = children
            .get(parent)
            .into_iter()
            .flatten()
            .filter_map(|edge| edge.order.map(|order| (order, edge.child.clone())))
            .collect();
        ordered.sort();
        ordered
    }
}

impl Default for BehaviorTreeDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphSchemaRegistry for BehaviorTreeDomain {
    fn node_schema(&self, node_type: &NodeTypeId) -> Option<&NodeSchema> {
        self.schemas.get(node_type)
    }
}

impl GraphDomain for BehaviorTreeDomain {
    fn graph_kind(&self) -> &GraphKind {
        &self.graph_kind
    }

    fn schema_registry(&self) -> &dyn GraphSchemaRegistry {
        self
    }

    fn validate_domain(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        if graph.kind != self.graph_kind {
            diagnostics.push(
                Diagnostic::error(
                    "behavior_tree.unsupported_graph_kind",
                    format!(
                        "behavior tree supports graph kind `{}`, not `{}`",
                        self.graph_kind.as_str(),
                        graph.kind.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Graph {
                    id: graph.id.clone(),
                }),
            );
            return diagnostics;
        }

        validate_root_count(self, graph, &mut diagnostics);
        validate_known_node_types(self, graph, &mut diagnostics);
        validate_required_behavior_properties(self, graph, &mut diagnostics);
        validate_port_value_compatibility(self, graph, &mut diagnostics);
        let children = child_edges_by_parent(self, graph);
        validate_child_ordering(graph, &children, &mut diagnostics);
        if validate_no_cycles(graph, &children, &mut diagnostics) {
            return diagnostics;
        }
        validate_reachability_and_child_counts(self, graph, &children, &mut diagnostics);
        diagnostics
    }
}

#[derive(Debug, Clone)]
struct ChildEdge {
    edge: EdgeId,
    child: NodeId,
    order: Option<u32>,
    has_order_annotation: bool,
}

fn behavior_node(id: NodeId, node_type: NodeTypeId, behavior: impl Into<String>) -> Node {
    Node::new(
        id,
        node_type,
        Value::Object(BTreeMap::from([(
            BEHAVIOR_KEY.into(),
            Value::String(behavior.into()),
        )])),
    )
}

fn behavior_property(node: &Node) -> Option<String> {
    match &node.properties {
        Value::Object(properties) => match properties.get(BEHAVIOR_KEY) {
            Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn child_edges_by_parent(
    domain: &BehaviorTreeDomain,
    graph: &Graph,
) -> BTreeMap<NodeId, Vec<ChildEdge>> {
    let mut children: BTreeMap<NodeId, Vec<ChildEdge>> = BTreeMap::new();
    for (edge_id, edge) in &graph.edges {
        if edge.from.port != domain.child_out {
            continue;
        }
        children
            .entry(edge.from.node.clone())
            .or_default()
            .push(ChildEdge {
                edge: edge_id.clone(),
                child: edge.to.node.clone(),
                order: child_order(edge),
                has_order_annotation: edge.annotations.contains_key(CHILD_ORDER_KEY),
            });
    }
    children
}

fn child_order(edge: &Edge) -> Option<u32> {
    match edge.annotations.get(CHILD_ORDER_KEY) {
        Some(Value::U64(value)) => u32::try_from(*value).ok(),
        Some(Value::I64(value)) => u32::try_from(*value).ok(),
        _ => None,
    }
}

fn validate_root_count(
    domain: &BehaviorTreeDomain,
    graph: &Graph,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let root_count = graph
        .nodes
        .values()
        .filter(|node| node.node_type == domain.root_type)
        .count();
    match root_count {
        0 => diagnostics.push(
            Diagnostic::error(
                "behavior_tree.missing_root",
                "behavior tree graph requires one root",
            )
            .with_target(DiagnosticTarget::Graph {
                id: graph.id.clone(),
            }),
        ),
        1 => {}
        _ => diagnostics.push(
            Diagnostic::error(
                "behavior_tree.multiple_roots",
                format!("behavior tree graph has {} roots, expected one", root_count),
            )
            .with_target(DiagnosticTarget::Graph {
                id: graph.id.clone(),
            }),
        ),
    }
}

fn validate_known_node_types(
    domain: &BehaviorTreeDomain,
    graph: &Graph,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (node_id, node) in &graph.nodes {
        if domain.node_kind(node).is_none() {
            diagnostics.push(
                Diagnostic::error(
                    "behavior_tree.unknown_node_type",
                    format!(
                        "node `{}` has unsupported behavior tree node type `{}`",
                        node_id.as_str(),
                        node.node_type.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: graph.id.clone(),
                    node: node_id.clone(),
                }),
            );
        }
    }
}

fn validate_required_behavior_properties(
    domain: &BehaviorTreeDomain,
    graph: &Graph,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (node_id, node) in &graph.nodes {
        let requires_behavior = node.node_type == domain.condition_type
            || node.node_type == domain.action_type
            || node.node_type == domain.decorator_type;
        if requires_behavior && behavior_property(node).is_none() {
            diagnostics.push(
                Diagnostic::error(
                    "behavior_tree.missing_behavior",
                    format!(
                        "behavior tree node `{}` requires a non-empty `{}` property",
                        node_id.as_str(),
                        BEHAVIOR_KEY
                    ),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: graph.id.clone(),
                    node: node_id.clone(),
                }),
            );
        }
    }
}

fn validate_port_value_compatibility(
    domain: &BehaviorTreeDomain,
    graph: &Graph,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (edge_id, edge) in &graph.edges {
        let Some(from_type) = port_value_type(domain, graph, &edge.from) else {
            continue;
        };
        let Some(to_type) = port_value_type(domain, graph, &edge.to) else {
            continue;
        };
        if from_type != to_type {
            diagnostics.push(
                Diagnostic::error(
                    "behavior_tree.port_type_mismatch",
                    format!(
                        "edge `{}` connects `{}` to incompatible `{}`",
                        edge_id.as_str(),
                        from_type.as_str(),
                        to_type.as_str()
                    ),
                )
                .with_target(DiagnosticTarget::Edge {
                    graph: graph.id.clone(),
                    edge: edge_id.clone(),
                }),
            );
        }
    }
}

fn port_value_type<'a>(
    domain: &'a BehaviorTreeDomain,
    graph: &Graph,
    endpoint: &PortRef,
) -> Option<&'a PortValueTypeId> {
    let node = graph.nodes.get(&endpoint.node)?;
    let schema = domain.node_schema(&node.node_type)?;
    let port = schema.ports.get(&endpoint.port)?;
    Some(&port.value_type)
}

fn validate_child_ordering(
    graph: &Graph,
    children: &BTreeMap<NodeId, Vec<ChildEdge>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen_by_parent: BTreeMap<NodeId, BTreeMap<u32, EdgeId>> = BTreeMap::new();
    for (parent, child_edges) in children {
        for child_edge in child_edges {
            let Some(order) = child_edge.order else {
                let code = if child_edge.has_order_annotation {
                    "behavior_tree.invalid_child_order"
                } else {
                    "behavior_tree.missing_child_order"
                };
                let message = if child_edge.has_order_annotation {
                    format!(
                        "edge `{}` has an invalid `{}` annotation",
                        child_edge.edge.as_str(),
                        CHILD_ORDER_KEY
                    )
                } else {
                    format!(
                        "edge `{}` requires `{}` annotation",
                        child_edge.edge.as_str(),
                        CHILD_ORDER_KEY
                    )
                };
                diagnostics.push(Diagnostic::error(code, message).with_target(
                    DiagnosticTarget::Edge {
                        graph: graph.id.clone(),
                        edge: child_edge.edge.clone(),
                    },
                ));
                continue;
            };
            let parent_orders = seen_by_parent.entry(parent.clone()).or_default();
            if parent_orders
                .insert(order, child_edge.edge.clone())
                .is_some()
            {
                diagnostics.push(
                    Diagnostic::error(
                        "behavior_tree.duplicate_child_order",
                        format!(
                            "parent node `{}` has duplicate child order {}",
                            parent.as_str(),
                            order
                        ),
                    )
                    .with_target(DiagnosticTarget::Node {
                        graph: graph.id.clone(),
                        node: parent.clone(),
                    }),
                );
            }
        }
    }
}

fn validate_no_cycles(
    graph: &Graph,
    children: &BTreeMap<NodeId, Vec<ChildEdge>>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut visited = BTreeSet::new();
    for node in graph.nodes.keys() {
        if has_cycle_from(node, children, &mut visited) {
            diagnostics.push(
                Diagnostic::error(
                    "behavior_tree.cycle_not_allowed",
                    "behavior tree graph must not contain directed cycles",
                )
                .with_target(DiagnosticTarget::Graph {
                    id: graph.id.clone(),
                }),
            );
            return true;
        }
    }
    false
}

fn has_cycle_from(
    start: &NodeId,
    children: &BTreeMap<NodeId, Vec<ChildEdge>>,
    visited: &mut BTreeSet<NodeId>,
) -> bool {
    if visited.contains(start) {
        return false;
    }
    let mut stack = vec![(start.clone(), false)];
    let mut visiting = BTreeSet::new();
    while let Some((node, exiting)) = stack.pop() {
        if exiting {
            visiting.remove(&node);
            visited.insert(node);
            continue;
        }
        if visited.contains(&node) {
            continue;
        }
        if !visiting.insert(node.clone()) {
            return true;
        }
        stack.push((node.clone(), true));
        if let Some(child_edges) = children.get(&node) {
            for child_edge in child_edges.iter().rev() {
                if visiting.contains(&child_edge.child) {
                    return true;
                }
                if !visited.contains(&child_edge.child) {
                    stack.push((child_edge.child.clone(), false));
                }
            }
        }
    }
    false
}

fn validate_reachability_and_child_counts(
    domain: &BehaviorTreeDomain,
    graph: &Graph,
    children: &BTreeMap<NodeId, Vec<ChildEdge>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(root) = domain.root_node_id(graph) else {
        return;
    };
    let (reachable, max_depth_exceeded) = collect_reachable(children, &root);
    if max_depth_exceeded {
        diagnostics.push(
            Diagnostic::error(
                "behavior_tree.max_depth_exceeded",
                format!(
                    "behavior tree depth exceeds maximum supported depth {}",
                    MAX_BEHAVIOR_TREE_DEPTH
                ),
            )
            .with_target(DiagnosticTarget::Graph {
                id: graph.id.clone(),
            }),
        );
        return;
    }

    for (node_id, node) in &graph.nodes {
        if !reachable.contains(node_id) {
            diagnostics.push(
                Diagnostic::error(
                    "behavior_tree.unreachable_node",
                    format!("node `{}` is not reachable from the root", node_id.as_str()),
                )
                .with_target(DiagnosticTarget::Node {
                    graph: graph.id.clone(),
                    node: node_id.clone(),
                }),
            );
        }

        let child_count = children.get(node_id).map_or(0, Vec::len);
        match domain.node_kind(node) {
            Some(BehaviorTreeNodeKind::Root) if child_count != 1 => {
                diagnostics.push(invalid_child_count(
                    graph,
                    node_id,
                    "root",
                    child_count,
                    "one",
                ));
            }
            Some(BehaviorTreeNodeKind::Sequence | BehaviorTreeNodeKind::Selector)
                if child_count == 0 =>
            {
                diagnostics.push(invalid_child_count(
                    graph,
                    node_id,
                    "composite",
                    child_count,
                    "at least one",
                ));
            }
            Some(BehaviorTreeNodeKind::Decorator) if child_count != 1 => {
                diagnostics.push(invalid_child_count(
                    graph,
                    node_id,
                    "decorator",
                    child_count,
                    "one",
                ));
            }
            Some(BehaviorTreeNodeKind::Condition | BehaviorTreeNodeKind::Action)
                if child_count != 0 =>
            {
                diagnostics.push(invalid_child_count(
                    graph,
                    node_id,
                    "leaf",
                    child_count,
                    "zero",
                ));
            }
            _ => {}
        }
    }
}

fn collect_reachable(
    children: &BTreeMap<NodeId, Vec<ChildEdge>>,
    root: &NodeId,
) -> (BTreeSet<NodeId>, bool) {
    let mut reachable = BTreeSet::new();
    let mut max_depth_exceeded = false;
    let mut stack = vec![(root.clone(), 0_u32)];
    while let Some((node, depth)) = stack.pop() {
        if depth > MAX_BEHAVIOR_TREE_DEPTH {
            max_depth_exceeded = true;
            continue;
        }
        if !reachable.insert(node.clone()) {
            continue;
        }
        if let Some(child_edges) = children.get(&node) {
            for child_edge in child_edges.iter().rev() {
                stack.push((child_edge.child.clone(), depth.saturating_add(1)));
            }
        }
    }
    (reachable, max_depth_exceeded)
}

fn invalid_child_count(
    graph: &Graph,
    node: &NodeId,
    label: &str,
    actual: usize,
    expected: &str,
) -> Diagnostic {
    Diagnostic::error(
        "behavior_tree.invalid_child_count",
        format!(
            "{label} node `{}` has {} child node(s), expected {expected}",
            node.as_str(),
            actual
        ),
    )
    .with_target(DiagnosticTarget::Node {
        graph: graph.id.clone(),
        node: node.clone(),
    })
}

fn layout_compiled_node(
    node: &CompiledBehaviorNode,
    depth: u32,
    leaf_index: &mut u32,
    view: &mut GraphView,
) -> f64 {
    let y = f64::from(depth) * NODE_VERTICAL_SPACING;
    if node.children.is_empty() {
        let x = f64::from(*leaf_index) * NODE_HORIZONTAL_SPACING;
        *leaf_index = leaf_index.saturating_add(1);
        view.nodes
            .insert(node.source.clone(), NodeLayout::new(Vec2::new(x, y)));
        return x;
    }

    let mut child_positions = Vec::new();
    for child in &node.children {
        child_positions.push(layout_compiled_node(
            child,
            depth.saturating_add(1),
            leaf_index,
            view,
        ));
    }
    let x = (child_positions.first().copied().unwrap_or(0.0)
        + child_positions.last().copied().unwrap_or(0.0))
        / 2.0;
    view.nodes
        .insert(node.source.clone(), NodeLayout::new(Vec2::new(x, y)));
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphCommand, GraphTransaction};

    fn valid_graph(domain: &BehaviorTreeDomain) -> (Graph, NodeId, NodeId, NodeId, NodeId) {
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "enemy_behavior",
        );
        let root = NodeId::generate();
        let selector = NodeId::generate();
        let condition = NodeId::generate();
        let action = NodeId::generate();

        let mut tx = GraphTransaction::begin(&graph);
        tx.apply(GraphCommand::AddNode {
            node: domain.root_node(root.clone()),
        });
        tx.apply(GraphCommand::AddNode {
            node: domain.selector_node(selector.clone()),
        });
        tx.apply(GraphCommand::AddNode {
            node: domain.condition_node(condition.clone(), "player_visible"),
        });
        tx.apply(GraphCommand::AddNode {
            node: domain.action_node(action.clone(), "chase_player"),
        });
        tx.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), root.clone(), selector.clone(), 0),
        });
        tx.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), selector.clone(), condition.clone(), 0),
        });
        tx.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), selector.clone(), action.clone(), 1),
        });
        tx.commit(&mut graph, domain.schema_registry())
            .expect("graph commands must build a structurally valid behavior tree");

        (graph, root, selector, condition, action)
    }

    #[test]
    fn behavior_tree_domain_provides_expected_schemas() {
        let domain = BehaviorTreeDomain::new();
        assert_eq!(domain.graph_kind().as_str(), "behavior_tree.graph");
        for node_type in [
            domain.root_type(),
            domain.sequence_type(),
            domain.selector_type(),
            domain.condition_type(),
            domain.action_type(),
            domain.decorator_type(),
        ] {
            assert!(domain.schema_registry().node_schema(node_type).is_some());
        }
    }

    #[test]
    fn non_trivial_behavior_tree_can_be_built_with_authoring_commands() {
        let domain = BehaviorTreeDomain::new();
        let (graph, _, _, _, _) = valid_graph(&domain);

        assert!(validate_graph_with_domain(&graph, &domain).is_empty());
    }

    #[test]
    fn compile_produces_deterministic_ordered_runtime_tree() {
        let domain = BehaviorTreeDomain::new();
        let (graph, _, _, condition, action) = valid_graph(&domain);

        let compiled = domain
            .compile(&graph)
            .expect("valid behavior tree must compile");
        assert_eq!(compiled.root.kind, BehaviorTreeNodeKind::Root);
        assert_eq!(compiled.root.children.len(), 1);
        let selector = &compiled.root.children[0];
        assert_eq!(selector.kind, BehaviorTreeNodeKind::Selector);
        assert_eq!(selector.children.len(), 2);
        assert_eq!(selector.children[0].source, condition);
        assert_eq!(selector.children[1].source, action);
        assert_eq!(
            selector.children[0].behavior.as_deref(),
            Some("player_visible")
        );
        assert_eq!(
            selector.children[1].behavior.as_deref(),
            Some("chase_player")
        );

        let again = domain
            .compile(&graph)
            .expect("valid behavior tree must compile deterministically");
        assert_eq!(compiled, again);
    }

    #[test]
    fn auto_layout_is_top_down_and_left_to_right() {
        let domain = BehaviorTreeDomain::new();
        let (graph, root, selector, condition, action) = valid_graph(&domain);

        let view = domain.auto_layout(&graph).expect("valid graph must layout");
        assert_eq!(view.layout_policy, domain.layout_policy());
        assert_eq!(view.nodes.len(), graph.nodes.len());
        let root_position = view.nodes.get(&root).unwrap().position;
        let selector_position = view.nodes.get(&selector).unwrap().position;
        let condition_position = view.nodes.get(&condition).unwrap().position;
        let action_position = view.nodes.get(&action).unwrap().position;

        assert!(root_position.y < selector_position.y);
        assert!(selector_position.y < condition_position.y);
        assert_eq!(condition_position.y, action_position.y);
        assert!(condition_position.x < action_position.x);
        assert_eq!(
            selector_position.x,
            (condition_position.x + action_position.x) / 2.0
        );
    }

    #[test]
    fn domain_validation_reports_stable_behavior_tree_codes() {
        let codes = [
            "behavior_tree.unsupported_graph_kind",
            "behavior_tree.missing_root",
            "behavior_tree.multiple_roots",
            "behavior_tree.unknown_node_type",
            "behavior_tree.missing_behavior",
            "behavior_tree.port_type_mismatch",
            "behavior_tree.missing_child_order",
            "behavior_tree.invalid_child_order",
            "behavior_tree.duplicate_child_order",
            "behavior_tree.cycle_not_allowed",
            "behavior_tree.unreachable_node",
            "behavior_tree.invalid_child_count",
            "behavior_tree.max_depth_exceeded",
        ];
        assert_eq!(codes.len(), 13);
    }

    #[test]
    fn missing_child_order_blocks_compile() {
        let domain = BehaviorTreeDomain::new();
        let (mut graph, root, selector, _, _) = valid_graph(&domain);
        let edge = graph
            .edges
            .values_mut()
            .find(|edge| edge.from.node == root && edge.to.node == selector)
            .expect("test graph must contain root edge");
        edge.annotations.remove(CHILD_ORDER_KEY);

        let diagnostics = domain
            .compile(&graph)
            .expect_err("missing order must block");
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.missing_child_order"));
    }

    #[test]
    fn invalid_child_order_has_specific_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let (mut graph, root, selector, _, _) = valid_graph(&domain);
        let edge = graph
            .edges
            .values_mut()
            .find(|edge| edge.from.node == root && edge.to.node == selector)
            .expect("test graph must contain root edge");
        edge.annotations
            .insert(CHILD_ORDER_KEY.into(), Value::U64(u64::from(u32::MAX) + 1));

        let diagnostics = domain
            .compile(&graph)
            .expect_err("invalid order must block");
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.invalid_child_order"));
        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.missing_child_order"));
    }

    #[test]
    fn duplicate_child_order_is_domain_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let (mut graph, _, selector, _, action) = valid_graph(&domain);
        let extra = NodeId::generate();
        graph
            .nodes
            .insert(extra.clone(), domain.action_node(extra.clone(), "patrol"));
        let edge = domain.child_edge(EdgeId::generate(), selector, extra, 1);
        graph.edges.insert(edge.id.clone(), edge);

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.duplicate_child_order"));
        assert!(graph.nodes.contains_key(&action));
    }

    #[test]
    fn missing_behavior_property_is_domain_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let (mut graph, _, _, condition, _) = valid_graph(&domain);
        graph.nodes.get_mut(&condition).unwrap().properties = empty_object();

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.missing_behavior"));
    }

    #[test]
    fn unreachable_node_is_domain_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let (mut graph, _, _, _, _) = valid_graph(&domain);
        let orphan = NodeId::generate();
        graph
            .nodes
            .insert(orphan.clone(), domain.action_node(orphan, "idle"));

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.unreachable_node"));
    }

    #[test]
    fn invalid_child_count_is_domain_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "invalid_behavior",
        );
        let root = NodeId::generate();
        graph
            .nodes
            .insert(root.clone(), domain.root_node(root.clone()));

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.invalid_child_count"));
    }

    #[test]
    fn unsupported_graph_kind_is_domain_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let graph = Graph::new(GraphId::generate(), GraphKind::new("other.graph"), "other");

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.unsupported_graph_kind"));
    }

    #[test]
    fn missing_root_is_domain_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "missing_root",
        );
        let action = NodeId::generate();
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action, "idle"));

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.missing_root"));
    }

    #[test]
    fn multiple_roots_are_domain_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let (mut graph, _, _, _, _) = valid_graph(&domain);
        let extra = NodeId::generate();
        graph.nodes.insert(extra.clone(), domain.root_node(extra));

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.multiple_roots"));
    }

    #[test]
    fn unknown_node_type_is_domain_diagnostic_when_schema_exists() {
        let mut domain = BehaviorTreeDomain::new();
        let unknown_type = NodeTypeId::new("behavior_tree.experimental");
        let schema = NodeSchema {
            node_type: unknown_type.clone(),
            compatible_graph_kinds: BTreeSet::from([domain.graph_kind().clone()]),
            display_name: "Experimental".into(),
            description: "Schema-known but domain-unsupported node.".into(),
            category: "Behavior Tree".into(),
            search_tags: vec!["experimental".into()],
            property_schema: empty_object(),
            ports: BTreeMap::new(),
            version: 1,
        };
        domain.schemas.insert(unknown_type.clone(), schema);
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "unknown_node_type",
        );
        let root = NodeId::generate();
        let unknown = NodeId::generate();
        graph.nodes.insert(root.clone(), domain.root_node(root));
        graph.nodes.insert(
            unknown.clone(),
            Node::new(unknown, unknown_type, empty_object()),
        );

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.unknown_node_type"));
    }

    #[test]
    fn port_type_mismatch_is_domain_diagnostic() {
        let mut domain = BehaviorTreeDomain::new();
        let selector_type = domain.selector_type.clone();
        let parent_in = domain.parent_in.clone();
        domain
            .schemas
            .get_mut(&selector_type)
            .unwrap()
            .ports
            .get_mut(&parent_in)
            .unwrap()
            .value_type = PortValueTypeId::new("behavior_tree.other_node");
        let (graph, _, _, _, _) = valid_graph(&domain);

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.port_type_mismatch"));
    }

    #[test]
    fn cycle_is_domain_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let (mut graph, _, selector, condition, _) = valid_graph(&domain);
        let edge = domain.child_edge(EdgeId::generate(), condition, selector, 0);
        graph.edges.insert(edge.id.clone(), edge);

        let diagnostics = domain.validate_domain(&graph);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.cycle_not_allowed"));
        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.unreachable_node"));
    }

    #[test]
    fn unordered_extra_child_still_counts_for_child_count_validation() {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "unordered_child_count",
        );
        let root = NodeId::generate();
        let decorator = NodeId::generate();
        let condition = NodeId::generate();
        let action = NodeId::generate();
        graph
            .nodes
            .insert(root.clone(), domain.root_node(root.clone()));
        graph.nodes.insert(
            decorator.clone(),
            domain.decorator_node(decorator.clone(), "invert"),
        );
        graph.nodes.insert(
            condition.clone(),
            domain.condition_node(condition.clone(), "player_visible"),
        );
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action.clone(), "idle"));
        let root_edge = domain.child_edge(EdgeId::generate(), root, decorator.clone(), 0);
        graph.edges.insert(root_edge.id.clone(), root_edge);
        let condition_edge = domain.child_edge(EdgeId::generate(), decorator.clone(), condition, 0);
        graph
            .edges
            .insert(condition_edge.id.clone(), condition_edge);
        let mut unordered_edge = domain.child_edge(EdgeId::generate(), decorator, action, 1);
        unordered_edge.annotations.remove(CHILD_ORDER_KEY);
        graph
            .edges
            .insert(unordered_edge.id.clone(), unordered_edge);

        let diagnostics = validate_graph_with_domain(&graph, &domain);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.missing_child_order"));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.invalid_child_count"));
    }

    #[test]
    fn excessive_depth_is_domain_diagnostic_before_layout_recursion() {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "deep_behavior",
        );
        let root = NodeId::generate();
        graph
            .nodes
            .insert(root.clone(), domain.root_node(root.clone()));
        let mut parent = root;
        for _ in 0..=MAX_BEHAVIOR_TREE_DEPTH {
            let child = NodeId::generate();
            graph.nodes.insert(
                child.clone(),
                domain.decorator_node(child.clone(), "decorator"),
            );
            let edge = domain.child_edge(EdgeId::generate(), parent, child.clone(), 0);
            graph.edges.insert(edge.id.clone(), edge);
            parent = child;
        }
        let action = NodeId::generate();
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action.clone(), "idle"));
        let edge = domain.child_edge(EdgeId::generate(), parent, action, 0);
        graph.edges.insert(edge.id.clone(), edge);

        let diagnostics = domain
            .auto_layout(&graph)
            .expect_err("excessive depth must block before layout");
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.max_depth_exceeded"));
        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.unreachable_node"));
        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "behavior_tree.invalid_child_count"));
    }
}
