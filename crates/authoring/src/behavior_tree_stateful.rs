//! Stateful Behavior Tree authoring extensions from ADR 0123.
//!
//! The legacy Behavior Tree domain remains the compatibility implementation for
//! existing node types and compiled DTOs. This wrapper adds schema-driven
//! stateful decorator nodes while normalizing them to deterministic compiled
//! decorator behavior identifiers, so serialized runtime DTO shapes and stable
//! graph identities do not change.

use crate::behavior_tree_legacy as legacy;
use crate::diagnostic::{Diagnostic, DiagnosticTarget};
use crate::graph::{
    Edge, Graph, GraphCommand, GraphKind, GraphSchemaRegistry, Node, NodeSchema,
    NodeTypeId,
};
use crate::graph_domain::{
    apply_graph_commands_with_domain, validate_graph_with_domain, GraphDomain,
};
use crate::graph_view::{GraphView, LayoutPolicyId};
use crate::id::{EdgeId, NodeId};
use crate::value::Value;
use std::collections::BTreeMap;

pub use legacy::{
    BehaviorTreeApply, BehaviorTreeCompilation, BehaviorTreeEdgeSummary, BehaviorTreeExample,
    BehaviorTreeLayout, BehaviorTreeNodeKind, BehaviorTreeNodeSummary, BehaviorTreeSchemaCatalog,
    BehaviorTreeServiceError, BehaviorTreeValidation, CompiledBehaviorNode, CompiledBehaviorTree,
};

const DURATION_SECONDS_KEY: &str = "duration_seconds";
const INVERTER_TYPE: &str = "behavior_tree.inverter";
const WAIT_TYPE: &str = "behavior_tree.wait";
const TIMEOUT_TYPE: &str = "behavior_tree.timeout";
const COOLDOWN_TYPE: &str = "behavior_tree.cooldown";
const INVERTER_BEHAVIOR: &str = "engine.inverter";
const WAIT_BEHAVIOR_PREFIX: &str = "engine.wait:";
const TIMEOUT_BEHAVIOR_PREFIX: &str = "engine.timeout:";
const COOLDOWN_BEHAVIOR_PREFIX: &str = "engine.cooldown:";

/// Behavior Tree graph domain extended with typed stateful decorators.
pub struct BehaviorTreeDomain {
    inner: legacy::BehaviorTreeDomain,
    inverter_type: NodeTypeId,
    wait_type: NodeTypeId,
    timeout_type: NodeTypeId,
    cooldown_type: NodeTypeId,
    extra_schemas: BTreeMap<NodeTypeId, NodeSchema>,
}

impl BehaviorTreeDomain {
    /// Creates the production Behavior Tree domain and ADR 0123 decorator schemas.
    #[must_use]
    pub fn new() -> Self {
        let inner = legacy::BehaviorTreeDomain::new();
        let inverter_type = NodeTypeId::new(INVERTER_TYPE);
        let wait_type = NodeTypeId::new(WAIT_TYPE);
        let timeout_type = NodeTypeId::new(TIMEOUT_TYPE);
        let cooldown_type = NodeTypeId::new(COOLDOWN_TYPE);
        let decorator_schema = inner
            .node_schema(inner.decorator_type())
            .expect("legacy Behavior Tree domain must define decorator schema")
            .clone();
        let extra_schemas = BTreeMap::from([
            (
                inverter_type.clone(),
                stateful_decorator_schema(
                    &decorator_schema,
                    inverter_type.clone(),
                    "Inverter",
                    "Inverts Success and Failure while preserving Running.",
                    vec!["inverter".into(), "not".into(), "decorator".into()],
                    Value::Object(BTreeMap::new()),
                ),
            ),
            (
                wait_type.clone(),
                stateful_decorator_schema(
                    &decorator_schema,
                    wait_type.clone(),
                    "Wait",
                    "Waits for a duration before ticking its child.",
                    vec!["wait".into(), "delay".into(), "decorator".into()],
                    duration_property_schema("non-negative duration in seconds"),
                ),
            ),
            (
                timeout_type.clone(),
                stateful_decorator_schema(
                    &decorator_schema,
                    timeout_type.clone(),
                    "Timeout",
                    "Fails and aborts its running child when the duration expires.",
                    vec!["timeout".into(), "deadline".into(), "decorator".into()],
                    duration_property_schema("positive timeout duration in seconds"),
                ),
            ),
            (
                cooldown_type.clone(),
                stateful_decorator_schema(
                    &decorator_schema,
                    cooldown_type.clone(),
                    "Cooldown",
                    "Blocks a successful child until its cooldown duration expires.",
                    vec!["cooldown".into(), "rate".into(), "decorator".into()],
                    duration_property_schema("positive cooldown duration in seconds"),
                ),
            ),
        ]);
        Self {
            inner,
            inverter_type,
            wait_type,
            timeout_type,
            cooldown_type,
            extra_schemas,
        }
    }

    /// Returns the Behavior Tree graph kind.
    #[must_use]
    pub fn graph_kind(&self) -> &GraphKind {
        self.inner.graph_kind()
    }

    /// Returns the root node type.
    #[must_use]
    pub fn root_type(&self) -> &NodeTypeId {
        self.inner.root_type()
    }

    /// Returns the sequence node type.
    #[must_use]
    pub fn sequence_type(&self) -> &NodeTypeId {
        self.inner.sequence_type()
    }

    /// Returns the selector node type.
    #[must_use]
    pub fn selector_type(&self) -> &NodeTypeId {
        self.inner.selector_type()
    }

    /// Returns the condition node type.
    #[must_use]
    pub fn condition_type(&self) -> &NodeTypeId {
        self.inner.condition_type()
    }

    /// Returns the action node type.
    #[must_use]
    pub fn action_type(&self) -> &NodeTypeId {
        self.inner.action_type()
    }

    /// Returns the generic decorator node type.
    #[must_use]
    pub fn decorator_type(&self) -> &NodeTypeId {
        self.inner.decorator_type()
    }

    /// Returns the typed Inverter decorator node type.
    #[must_use]
    pub fn inverter_type(&self) -> &NodeTypeId {
        &self.inverter_type
    }

    /// Returns the typed Wait decorator node type.
    #[must_use]
    pub fn wait_type(&self) -> &NodeTypeId {
        &self.wait_type
    }

    /// Returns the typed Timeout decorator node type.
    #[must_use]
    pub fn timeout_type(&self) -> &NodeTypeId {
        &self.timeout_type
    }

    /// Returns the typed Cooldown decorator node type.
    #[must_use]
    pub fn cooldown_type(&self) -> &NodeTypeId {
        &self.cooldown_type
    }

    /// Returns the default Behavior Tree layout policy.
    #[must_use]
    pub fn layout_policy(&self) -> LayoutPolicyId {
        self.inner.layout_policy()
    }

    /// Creates a root node.
    #[must_use]
    pub fn root_node(&self, id: NodeId) -> Node {
        self.inner.root_node(id)
    }

    /// Creates a sequence node.
    #[must_use]
    pub fn sequence_node(&self, id: NodeId) -> Node {
        self.inner.sequence_node(id)
    }

    /// Creates a selector node.
    #[must_use]
    pub fn selector_node(&self, id: NodeId) -> Node {
        self.inner.selector_node(id)
    }

    /// Creates a condition node with a stable behavior identifier.
    #[must_use]
    pub fn condition_node(&self, id: NodeId, behavior: impl Into<String>) -> Node {
        self.inner.condition_node(id, behavior)
    }

    /// Creates an action node with a stable behavior identifier.
    #[must_use]
    pub fn action_node(&self, id: NodeId, behavior: impl Into<String>) -> Node {
        self.inner.action_node(id, behavior)
    }

    /// Creates a generic decorator node with a stable behavior identifier.
    #[must_use]
    pub fn decorator_node(&self, id: NodeId, behavior: impl Into<String>) -> Node {
        self.inner.decorator_node(id, behavior)
    }

    /// Creates a typed Inverter decorator node.
    #[must_use]
    pub fn inverter_node(&self, id: NodeId) -> Node {
        Node::new(id, self.inverter_type.clone(), Value::Object(BTreeMap::new()))
    }

    /// Creates a typed Wait decorator node.
    #[must_use]
    pub fn wait_node(&self, id: NodeId, duration_seconds: f64) -> Node {
        duration_node(id, self.wait_type.clone(), duration_seconds)
    }

    /// Creates a typed Timeout decorator node.
    #[must_use]
    pub fn timeout_node(&self, id: NodeId, duration_seconds: f64) -> Node {
        duration_node(id, self.timeout_type.clone(), duration_seconds)
    }

    /// Creates a typed Cooldown decorator node.
    #[must_use]
    pub fn cooldown_node(&self, id: NodeId, duration_seconds: f64) -> Node {
        duration_node(id, self.cooldown_type.clone(), duration_seconds)
    }

    /// Creates a parent-to-child edge with explicit child order.
    #[must_use]
    pub fn child_edge(&self, id: EdgeId, parent: NodeId, child: NodeId, order: u32) -> Edge {
        self.inner.child_edge(id, parent, child, order)
    }

    /// Compiles `graph` into the unchanged runtime Behavior Tree DTO.
    pub fn compile(&self, graph: &Graph) -> Result<CompiledBehaviorTree, Vec<Diagnostic>> {
        let diagnostics = validate_graph_with_domain(graph, self);
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(diagnostics);
        }
        self.inner.compile(&self.normalized_graph(graph))
    }

    /// Produces a deterministic top-down graph view for `graph`.
    pub fn auto_layout(&self, graph: &Graph) -> Result<GraphView, Vec<Diagnostic>> {
        let diagnostics = validate_graph_with_domain(graph, self);
        if diagnostics.iter().any(Diagnostic::is_blocking) {
            return Err(diagnostics);
        }
        self.inner.auto_layout(&self.normalized_graph(graph))
    }

    fn normalized_graph(&self, graph: &Graph) -> Graph {
        let mut normalized = Graph::new(graph.id.clone(), graph.kind.clone(), graph.name.clone());
        normalized.schema_version = graph.schema_version;
        normalized.display_name = graph.display_name.clone();
        normalized.description = graph.description.clone();
        normalized.nodes = graph.nodes.clone();
        normalized.edges = graph.edges.clone();
        normalized.groups = graph.groups.clone();
        normalized.annotations = graph.annotations.clone();
        for node in normalized.nodes.values_mut() {
            let behavior = if node.node_type == self.inverter_type {
                Some(INVERTER_BEHAVIOR.to_owned())
            } else if node.node_type == self.wait_type {
                duration_behavior(node, WAIT_BEHAVIOR_PREFIX)
            } else if node.node_type == self.timeout_type {
                duration_behavior(node, TIMEOUT_BEHAVIOR_PREFIX)
            } else if node.node_type == self.cooldown_type {
                duration_behavior(node, COOLDOWN_BEHAVIOR_PREFIX)
            } else {
                None
            };
            if let Some(behavior) = behavior {
                node.node_type = self.inner.decorator_type().clone();
                node.properties = behavior_properties(behavior);
            }
        }
        normalized
    }
}

impl Default for BehaviorTreeDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphSchemaRegistry for BehaviorTreeDomain {
    fn node_schema(&self, node_type: &NodeTypeId) -> Option<&NodeSchema> {
        self.extra_schemas
            .get(node_type)
            .or_else(|| self.inner.node_schema(node_type))
    }
}

impl GraphDomain for BehaviorTreeDomain {
    fn graph_kind(&self) -> &GraphKind {
        self.inner.graph_kind()
    }

    fn schema_registry(&self) -> &dyn GraphSchemaRegistry {
        self
    }

    fn validate_domain(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diagnostics = validate_stateful_decorator_properties(self, graph);
        diagnostics.extend(self.inner.validate_domain(&self.normalized_graph(graph)));
        diagnostics
    }
}

/// Shared Behavior Tree authoring service used by Editor, CLI, MCP, and tests.
pub struct BehaviorTreeAuthoringService {
    domain: BehaviorTreeDomain,
}

impl BehaviorTreeAuthoringService {
    /// Creates a service with the production Behavior Tree domain.
    #[must_use]
    pub fn new() -> Self {
        Self {
            domain: BehaviorTreeDomain::new(),
        }
    }

    /// Returns the underlying Behavior Tree domain.
    #[must_use]
    pub fn domain(&self) -> &BehaviorTreeDomain {
        &self.domain
    }

    /// Returns schemas needed by authoring tools to create valid nodes.
    #[must_use]
    pub fn schemas(&self) -> BehaviorTreeSchemaCatalog {
        let node_types = [
            self.domain.root_type(),
            self.domain.sequence_type(),
            self.domain.selector_type(),
            self.domain.condition_type(),
            self.domain.action_type(),
            self.domain.decorator_type(),
            self.domain.inverter_type(),
            self.domain.wait_type(),
            self.domain.timeout_type(),
            self.domain.cooldown_type(),
        ];
        let nodes = node_types
            .into_iter()
            .filter_map(|node_type| self.domain.node_schema(node_type).cloned())
            .collect();
        BehaviorTreeSchemaCatalog {
            graph_kind: self.domain.graph_kind().clone(),
            layout_policy: self.domain.layout_policy(),
            nodes,
        }
    }

    /// Builds the existing reference chase-or-patrol scenario.
    pub fn example(&self) -> Result<BehaviorTreeExample, BehaviorTreeServiceError> {
        legacy::BehaviorTreeAuthoringService::new().example()
    }

    /// Parses a Behavior Tree graph JSON document.
    pub fn graph_from_json(&self, json: &str) -> Result<Graph, BehaviorTreeServiceError> {
        let graph = serde_json::from_str::<Graph>(json)
            .map_err(|source| BehaviorTreeServiceError::Json { source })?;
        self.ensure_behavior_tree_graph(&graph)?;
        Ok(graph)
    }

    /// Parses a JSON array of graph commands.
    pub fn commands_from_json(
        &self,
        json: &str,
    ) -> Result<Vec<GraphCommand>, BehaviorTreeServiceError> {
        serde_json::from_str(json).map_err(|source| BehaviorTreeServiceError::Json { source })
    }

    /// Serializes a Behavior Tree graph to canonical JSON.
    pub fn graph_to_canonical_json(
        &self,
        graph: &Graph,
    ) -> Result<String, BehaviorTreeServiceError> {
        self.ensure_behavior_tree_graph(graph)?;
        graph
            .to_canonical_json(&self.domain)
            .map_err(|source| BehaviorTreeServiceError::Save { source })
    }

    /// Verifies that `graph` belongs to the Behavior Tree domain.
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
    #[must_use]
    pub fn validate(&self, graph: &Graph) -> BehaviorTreeValidation {
        let diagnostics = validate_graph_with_domain(graph, &self.domain);
        BehaviorTreeValidation {
            success: !diagnostics.iter().any(Diagnostic::is_blocking),
            diagnostics,
        }
    }

    /// Compiles a Behavior Tree graph into a runtime tree artifact.
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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

fn stateful_decorator_schema(
    base: &NodeSchema,
    node_type: NodeTypeId,
    display_name: &str,
    description: &str,
    search_tags: Vec<String>,
    property_schema: Value,
) -> NodeSchema {
    let mut schema = base.clone();
    schema.node_type = node_type;
    schema.display_name = display_name.into();
    schema.description = description.into();
    schema.search_tags = search_tags;
    schema.property_schema = property_schema;
    schema
}

fn duration_property_schema(description: &str) -> Value {
    Value::Object(BTreeMap::from([(
        DURATION_SECONDS_KEY.into(),
        Value::String(description.into()),
    )]))
}

fn duration_node(id: NodeId, node_type: NodeTypeId, duration_seconds: f64) -> Node {
    Node::new(
        id,
        node_type,
        Value::Object(BTreeMap::from([(
            DURATION_SECONDS_KEY.into(),
            Value::F64(duration_seconds),
        )])),
    )
}

fn duration_value(node: &Node) -> Option<f64> {
    let Value::Object(properties) = &node.properties else {
        return None;
    };
    match properties.get(DURATION_SECONDS_KEY) {
        Some(Value::F64(value)) => Some(*value),
        Some(Value::I64(value)) => Some(*value as f64),
        Some(Value::U64(value)) => Some(*value as f64),
        _ => None,
    }
}

fn duration_behavior(node: &Node, prefix: &str) -> Option<String> {
    duration_value(node).map(|duration| format!("{prefix}{duration}"))
}

fn behavior_properties(behavior: String) -> Value {
    Value::Object(BTreeMap::from([(
        "behavior".into(),
        Value::String(behavior),
    )]))
}

fn validate_stateful_decorator_properties(
    domain: &BehaviorTreeDomain,
    graph: &Graph,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for (node_id, node) in &graph.nodes {
        let rule = if node.node_type == domain.wait_type {
            Some(false)
        } else if node.node_type == domain.timeout_type || node.node_type == domain.cooldown_type {
            Some(true)
        } else {
            None
        };
        let Some(require_positive) = rule else {
            continue;
        };
        let duration = duration_value(node);
        let valid = duration.is_some_and(|value| {
            value.is_finite() && if require_positive { value > 0.0 } else { value >= 0.0 }
        });
        if valid {
            continue;
        }
        diagnostics.push(
            Diagnostic::error(
                "behavior_tree.invalid_decorator_parameter",
                format!(
                    "stateful decorator node `{}` requires `{}` to be {}",
                    node_id.as_str(),
                    DURATION_SECONDS_KEY,
                    if require_positive {
                        "a positive finite number"
                    } else {
                        "a non-negative finite number"
                    }
                ),
            )
            .with_target(DiagnosticTarget::Node {
                graph: graph.id.clone(),
                node: node_id.clone(),
            }),
        );
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::GraphId;

    #[test]
    fn schema_catalog_exposes_typed_stateful_decorators() {
        let service = BehaviorTreeAuthoringService::new();
        let node_types = service
            .schemas()
            .nodes
            .into_iter()
            .map(|schema| schema.node_type)
            .collect::<Vec<_>>();

        assert!(node_types.contains(service.domain().inverter_type()));
        assert!(node_types.contains(service.domain().wait_type()));
        assert!(node_types.contains(service.domain().timeout_type()));
        assert!(node_types.contains(service.domain().cooldown_type()));
    }

    #[test]
    fn typed_wait_compiles_to_unchanged_decorator_dto() {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "wait_tree",
        );
        let root = NodeId::generate();
        let wait = NodeId::generate();
        let action = NodeId::generate();
        graph.nodes.insert(root.clone(), domain.root_node(root.clone()));
        graph
            .nodes
            .insert(wait.clone(), domain.wait_node(wait.clone(), 0.5));
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action.clone(), "work"));
        let root_edge = domain.child_edge(EdgeId::generate(), root, wait.clone(), 0);
        graph.edges.insert(root_edge.id.clone(), root_edge);
        let action_edge = domain.child_edge(EdgeId::generate(), wait, action, 0);
        graph.edges.insert(action_edge.id.clone(), action_edge);

        let compiled = domain.compile(&graph).expect("typed wait must compile");
        let wait = &compiled.root.children[0];
        assert_eq!(wait.kind, BehaviorTreeNodeKind::Decorator);
        assert_eq!(wait.behavior.as_deref(), Some("engine.wait:0.5"));
    }

    #[test]
    fn invalid_timeout_duration_is_blocking_diagnostic() {
        let domain = BehaviorTreeDomain::new();
        let root = NodeId::generate();
        let timeout = NodeId::generate();
        let action = NodeId::generate();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "invalid_timeout",
        );
        graph.nodes.insert(root.clone(), domain.root_node(root.clone()));
        graph
            .nodes
            .insert(timeout.clone(), domain.timeout_node(timeout.clone(), 0.0));
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action.clone(), "work"));
        let root_edge = domain.child_edge(EdgeId::generate(), root, timeout.clone(), 0);
        graph.edges.insert(root_edge.id.clone(), root_edge);
        let action_edge = domain.child_edge(EdgeId::generate(), timeout, action, 0);
        graph.edges.insert(action_edge.id.clone(), action_edge);

        assert!(validate_graph_with_domain(&graph, &domain).iter().any(|diagnostic| {
            diagnostic.code == "behavior_tree.invalid_decorator_parameter"
        }));
    }
}
