//! Domain validation boundary for semantic graphs.
//!
//! This module contains the Phase 3-C domain validation stub. It proves that
//! graph domains can provide schema registries and domain-owned validation
//! without moving domain semantics into the graph foundation. It does not
//! contain runtime graph execution, compiled graph artifacts, concrete
//! production graph domains, renderer integration, ECS integration, bridge
//! integration, or graph view validation.

use crate::diagnostic::{Diagnostic, DiagnosticTarget};
use crate::graph::{
    Edge, Graph, GraphChange, GraphCommand, GraphKind, GraphSchemaRegistry, GraphTransaction, Node,
    NodeSchema, NodeTypeId, PortArity, PortDirection, PortRef, PortSchema, PortValueTypeId,
};
use crate::id::{EdgeId, NodeId, PortId, StableId};
use crate::value::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Domain-owned graph validation contract.
///
/// A graph domain supplies the schemas needed by foundation structural
/// validation and owns semantic checks such as type compatibility, root rules,
/// cycle rules, and node property semantics.
pub trait GraphDomain {
    /// Returns the single graph kind supported by this Phase 3-C domain.
    fn graph_kind(&self) -> &GraphKind;

    /// Returns the schema registry used for foundation structural validation.
    fn schema_registry(&self) -> &dyn GraphSchemaRegistry;

    /// Returns domain-specific diagnostics for `graph`.
    fn validate_domain(&self, graph: &Graph) -> Vec<Diagnostic>;
}

/// Runs foundation structural validation and then explicit domain validation.
///
/// Domain validation is skipped when structural validation produces blocking
/// diagnostics. Domain validation assumes the graph has valid structure.
pub fn validate_graph_with_domain(graph: &Graph, domain: &dyn GraphDomain) -> Vec<Diagnostic> {
    let mut diagnostics = graph.validate(domain.schema_registry());
    if diagnostics.iter().any(Diagnostic::is_blocking) {
        return diagnostics;
    }
    diagnostics.extend(domain.validate_domain(graph));
    diagnostics
}

/// Result of applying semantic graph commands with a graph domain.
#[derive(Debug)]
pub enum GraphCommandApplication {
    /// Commands were committed to a private graph copy and passed domain validation.
    Applied {
        /// Non-blocking diagnostics produced during command, structural, and domain validation.
        diagnostics: Vec<Diagnostic>,
        /// Semantic graph diff produced by the transaction.
        diff: Vec<GraphChange>,
        /// The validated graph after applying the commands.
        graph: Box<Graph>,
    },
    /// Commands were rejected by structural or domain validation.
    Rejected {
        /// Blocking diagnostics that caused rejection.
        diagnostics: Vec<Diagnostic>,
        /// Semantic graph diff accumulated before rejection.
        diff: Vec<GraphChange>,
    },
}

impl GraphCommandApplication {
    /// Returns `true` when commands were applied and domain validation passed.
    pub fn success(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }

    /// Returns diagnostics produced while applying commands.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            Self::Applied { diagnostics, .. } | Self::Rejected { diagnostics, .. } => diagnostics,
        }
    }

    /// Returns the semantic diff produced while applying commands.
    pub fn diff(&self) -> &[GraphChange] {
        match self {
            Self::Applied { diff, .. } | Self::Rejected { diff, .. } => diff,
        }
    }

    /// Returns the committed graph when application succeeded.
    pub fn graph(&self) -> Option<&Graph> {
        match self {
            Self::Applied { graph, .. } => Some(graph.as_ref()),
            Self::Rejected { .. } => None,
        }
    }
}

/// Applies graph commands to a private graph copy and validates with `domain`.
///
/// The input graph is never mutated. This is the shared command application
/// path for CLI, MCP, and editor adapters.
pub fn apply_graph_commands_with_domain(
    graph: &Graph,
    domain: &dyn GraphDomain,
    commands: impl IntoIterator<Item = GraphCommand>,
) -> GraphCommandApplication {
    let mut transaction = GraphTransaction::begin(graph);
    for command in commands {
        transaction.apply(command);
    }
    let diff = transaction.preview_diff().to_vec();

    let commit = match transaction.commit_private(domain.schema_registry()) {
        Ok(commit) => commit,
        Err(error) => {
            return GraphCommandApplication::Rejected {
                diagnostics: error.diagnostics,
                diff,
            };
        }
    };
    let mut diagnostics = commit.diagnostics;
    let graph = commit.graph;

    diagnostics.extend(domain.validate_domain(&graph));
    if diagnostics.iter().any(Diagnostic::is_blocking) {
        GraphCommandApplication::Rejected { diagnostics, diff }
    } else {
        GraphCommandApplication::Applied {
            diagnostics,
            diff,
            graph: Box::new(graph),
        }
    }
}

/// Phase 3-C fixture graph domain used to prove the domain validation boundary.
///
/// `TestGraphDomain` is not a production Behavior Tree, Shader Graph,
/// Animation Graph, or visual scripting domain.
pub struct TestGraphDomain {
    graph_kind: GraphKind,
    root_type: NodeTypeId,
    action_type: NodeTypeId,
    number_source_type: NodeTypeId,
    bool_source_type: NodeTypeId,
    root_out: PortId,
    action_in: PortId,
    action_out: PortId,
    action_number: PortId,
    number_out: PortId,
    bool_out: PortId,
    schemas: BTreeMap<NodeTypeId, NodeSchema>,
}

fn fixed_port_id(suffix: &str) -> PortId {
    PortId::from_stable_id(StableId::new(format!("port_{suffix}")))
        .expect("fixture port ID must use a valid stable ID")
}

impl TestGraphDomain {
    /// Creates the fixture domain with stable in-memory schemas.
    pub fn new() -> Self {
        let graph_kind = GraphKind::new("test_domain.graph");
        let root_type = NodeTypeId::new("test_domain.root");
        let action_type = NodeTypeId::new("test_domain.action");
        let number_source_type = NodeTypeId::new("test_domain.number_source");
        let bool_source_type = NodeTypeId::new("test_domain.bool_source");
        let root_out = fixed_port_id("01234567890ABCDEFGHJKMNPQR");
        let action_in = fixed_port_id("01234567890ABCDEFGHJKMNPRS");
        let action_out = fixed_port_id("01234567890ABCDEFGHJKMNPRT");
        let action_number = fixed_port_id("01234567890ABCDEFGHJKMNPRV");
        let number_out = fixed_port_id("01234567890ABCDEFGHJKMNPRW");
        let bool_out = fixed_port_id("01234567890ABCDEFGHJKMNPRX");
        let flow_type = PortValueTypeId::new("test_domain.flow");
        let number_type = PortValueTypeId::new("test_domain.number");
        let bool_type = PortValueTypeId::new("test_domain.bool");
        let compatible_graph_kinds = BTreeSet::from([graph_kind.clone()]);
        let schemas = BTreeMap::from([
            (
                root_type.clone(),
                NodeSchema {
                    node_type: root_type.clone(),
                    compatible_graph_kinds: compatible_graph_kinds.clone(),
                    display_name: "Test Root".into(),
                    description: "Fixture root node.".into(),
                    category: "Test".into(),
                    search_tags: vec!["root".into()],
                    property_schema: Value::Object(BTreeMap::new()),
                    ports: BTreeMap::from([(
                        root_out.clone(),
                        PortSchema {
                            id: root_out.clone(),
                            name: "out".into(),
                            display_name: "Out".into(),
                            description: "Flow output.".into(),
                            direction: PortDirection::Output,
                            value_type: flow_type.clone(),
                            arity: PortArity::many(),
                        },
                    )]),
                    version: 1,
                },
            ),
            (
                action_type.clone(),
                NodeSchema {
                    node_type: action_type.clone(),
                    compatible_graph_kinds: compatible_graph_kinds.clone(),
                    display_name: "Test Action".into(),
                    description: "Fixture action node.".into(),
                    category: "Test".into(),
                    search_tags: vec!["action".into()],
                    property_schema: Value::Object(BTreeMap::new()),
                    ports: BTreeMap::from([
                        (
                            action_in.clone(),
                            PortSchema {
                                id: action_in.clone(),
                                name: "in".into(),
                                display_name: "In".into(),
                                description: "Flow input.".into(),
                                direction: PortDirection::Input,
                                value_type: flow_type.clone(),
                                arity: PortArity::optional_single(),
                            },
                        ),
                        (
                            action_out.clone(),
                            PortSchema {
                                id: action_out.clone(),
                                name: "out".into(),
                                display_name: "Out".into(),
                                description: "Flow output.".into(),
                                direction: PortDirection::Output,
                                value_type: flow_type.clone(),
                                arity: PortArity::many(),
                            },
                        ),
                        (
                            action_number.clone(),
                            PortSchema {
                                id: action_number.clone(),
                                name: "number".into(),
                                display_name: "Number".into(),
                                description: "Number input.".into(),
                                direction: PortDirection::Input,
                                value_type: number_type.clone(),
                                arity: PortArity::optional_single(),
                            },
                        ),
                    ]),
                    version: 1,
                },
            ),
            (
                number_source_type.clone(),
                NodeSchema {
                    node_type: number_source_type.clone(),
                    compatible_graph_kinds: compatible_graph_kinds.clone(),
                    display_name: "Number Source".into(),
                    description: "Fixture number source node.".into(),
                    category: "Test".into(),
                    search_tags: vec!["number".into()],
                    property_schema: Value::Object(BTreeMap::new()),
                    ports: BTreeMap::from([(
                        number_out.clone(),
                        PortSchema {
                            id: number_out.clone(),
                            name: "out".into(),
                            display_name: "Out".into(),
                            description: "Number output.".into(),
                            direction: PortDirection::Output,
                            value_type: number_type.clone(),
                            arity: PortArity::many(),
                        },
                    )]),
                    version: 1,
                },
            ),
            (
                bool_source_type.clone(),
                NodeSchema {
                    node_type: bool_source_type.clone(),
                    compatible_graph_kinds,
                    display_name: "Bool Source".into(),
                    description: "Fixture bool source node.".into(),
                    category: "Test".into(),
                    search_tags: vec!["bool".into()],
                    property_schema: Value::Object(BTreeMap::new()),
                    ports: BTreeMap::from([(
                        bool_out.clone(),
                        PortSchema {
                            id: bool_out.clone(),
                            name: "out".into(),
                            display_name: "Out".into(),
                            description: "Bool output.".into(),
                            direction: PortDirection::Output,
                            value_type: bool_type,
                            arity: PortArity::many(),
                        },
                    )]),
                    version: 1,
                },
            ),
        ]);
        Self {
            graph_kind,
            root_type,
            action_type,
            number_source_type,
            bool_source_type,
            root_out,
            action_in,
            action_out,
            action_number,
            number_out,
            bool_out,
            schemas,
        }
    }

    /// Returns the fixture root node type.
    pub fn root_type(&self) -> &NodeTypeId {
        &self.root_type
    }

    /// Returns the fixture action node type.
    pub fn action_type(&self) -> &NodeTypeId {
        &self.action_type
    }

    /// Returns the fixture number source node type.
    pub fn number_source_type(&self) -> &NodeTypeId {
        &self.number_source_type
    }

    /// Returns the fixture bool source node type.
    pub fn bool_source_type(&self) -> &NodeTypeId {
        &self.bool_source_type
    }

    /// Creates a root node.
    pub fn root_node(&self, id: NodeId) -> Node {
        Node::new(id, self.root_type.clone(), Value::Object(BTreeMap::new()))
    }

    /// Creates an action node with the required `label` property.
    pub fn action_node(&self, id: NodeId, label: impl Into<String>) -> Node {
        Node::new(
            id,
            self.action_type.clone(),
            Value::Object(BTreeMap::from([(
                "label".into(),
                Value::String(label.into()),
            )])),
        )
    }

    /// Creates an action node without the required `label` property.
    pub fn action_node_without_label(&self, id: NodeId) -> Node {
        Node::new(id, self.action_type.clone(), Value::Object(BTreeMap::new()))
    }

    /// Creates a number source node.
    pub fn number_source_node(&self, id: NodeId) -> Node {
        Node::new(
            id,
            self.number_source_type.clone(),
            Value::Object(BTreeMap::new()),
        )
    }

    /// Creates a bool source node.
    pub fn bool_source_node(&self, id: NodeId) -> Node {
        Node::new(
            id,
            self.bool_source_type.clone(),
            Value::Object(BTreeMap::new()),
        )
    }

    /// Creates a flow edge from root output to action input.
    pub fn root_to_action_edge(&self, id: EdgeId, root: NodeId, action: NodeId) -> Edge {
        Edge::new(
            id,
            PortRef::new(root, self.root_out.clone()),
            PortRef::new(action, self.action_in.clone()),
        )
    }

    /// Creates a number edge from number source output to action number input.
    pub fn number_to_action_edge(&self, id: EdgeId, number: NodeId, action: NodeId) -> Edge {
        Edge::new(
            id,
            PortRef::new(number, self.number_out.clone()),
            PortRef::new(action, self.action_number.clone()),
        )
    }

    /// Creates a type-mismatched edge from bool output to action number input.
    pub fn bool_to_action_number_edge(
        &self,
        id: EdgeId,
        bool_node: NodeId,
        action: NodeId,
    ) -> Edge {
        Edge::new(
            id,
            PortRef::new(bool_node, self.bool_out.clone()),
            PortRef::new(action, self.action_number.clone()),
        )
    }

    /// Creates a flow edge from action output to action input.
    pub fn action_to_action_edge(&self, id: EdgeId, from: NodeId, to: NodeId) -> Edge {
        Edge::new(
            id,
            PortRef::new(from, self.action_out.clone()),
            PortRef::new(to, self.action_in.clone()),
        )
    }
}

impl Default for TestGraphDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphSchemaRegistry for TestGraphDomain {
    fn node_schema(&self, node_type: &NodeTypeId) -> Option<&NodeSchema> {
        self.schemas.get(node_type)
    }
}

impl GraphDomain for TestGraphDomain {
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
                    "test_domain.unsupported_graph_kind",
                    format!(
                        "test domain supports graph kind `{}`, not `{}`",
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
        validate_required_properties(self, graph, &mut diagnostics);
        validate_port_value_compatibility(self, graph, &mut diagnostics);
        validate_no_cycles(graph, &mut diagnostics);
        diagnostics
    }
}

fn validate_root_count(domain: &TestGraphDomain, graph: &Graph, diagnostics: &mut Vec<Diagnostic>) {
    let roots: Vec<_> = graph
        .nodes
        .iter()
        .filter_map(|(node_id, node)| {
            (node.node_type == domain.root_type).then_some(node_id.clone())
        })
        .collect();
    match roots.len() {
        0 => diagnostics.push(
            Diagnostic::error(
                "test_domain.missing_root",
                "test domain graph requires one root",
            )
            .with_target(DiagnosticTarget::Graph {
                id: graph.id.clone(),
            }),
        ),
        1 => {}
        _ => diagnostics.push(
            Diagnostic::error(
                "test_domain.multiple_roots",
                format!("test domain graph has {} roots, expected one", roots.len()),
            )
            .with_target(DiagnosticTarget::Graph {
                id: graph.id.clone(),
            }),
        ),
    }
}

fn validate_required_properties(
    domain: &TestGraphDomain,
    graph: &Graph,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (node_id, node) in &graph.nodes {
        if node.node_type != domain.action_type {
            continue;
        }
        let has_label = match &node.properties {
            Value::Object(properties) => matches!(
                properties.get("label"),
                Some(Value::String(label)) if !label.is_empty()
            ),
            _ => false,
        };
        if !has_label {
            diagnostics.push(
                Diagnostic::error(
                    "test_domain.missing_required_property",
                    format!(
                        "action node `{}` requires a non-empty `label` property",
                        node_id
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
    domain: &TestGraphDomain,
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
                    "test_domain.port_type_mismatch",
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
    domain: &'a TestGraphDomain,
    graph: &Graph,
    endpoint: &PortRef,
) -> Option<&'a PortValueTypeId> {
    let node = graph.nodes.get(&endpoint.node)?;
    let schema = domain.node_schema(&node.node_type)?;
    let port = schema.ports.get(&endpoint.port)?;
    Some(&port.value_type)
}

fn validate_no_cycles(graph: &Graph, diagnostics: &mut Vec<Diagnostic>) {
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for node in graph.nodes.keys() {
        if has_cycle_from(node, graph, &mut visiting, &mut visited) {
            diagnostics.push(
                Diagnostic::error(
                    "test_domain.cycle_not_allowed",
                    "test domain graph must not contain directed cycles",
                )
                .with_target(DiagnosticTarget::Graph {
                    id: graph.id.clone(),
                }),
            );
            return;
        }
    }
}

fn has_cycle_from(
    node: &NodeId,
    graph: &Graph,
    visiting: &mut BTreeSet<NodeId>,
    visited: &mut BTreeSet<NodeId>,
) -> bool {
    if visited.contains(node) {
        return false;
    }
    if !visiting.insert(node.clone()) {
        return true;
    }
    for edge in graph.edges.values().filter(|edge| edge.from.node == *node) {
        if has_cycle_from(&edge.to.node, graph, visiting, visited) {
            return true;
        }
    }
    visiting.remove(node);
    visited.insert(node.clone());
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphCommand, GraphTransaction};
    use crate::id::GraphId;

    fn valid_graph(domain: &TestGraphDomain) -> (Graph, NodeId, NodeId, NodeId) {
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "test_domain_graph",
        );
        let root = NodeId::generate();
        let action = NodeId::generate();
        let number = NodeId::generate();
        graph
            .nodes
            .insert(root.clone(), domain.root_node(root.clone()));
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action.clone(), "act"));
        graph
            .nodes
            .insert(number.clone(), domain.number_source_node(number.clone()));
        let flow = EdgeId::generate();
        graph.edges.insert(
            flow.clone(),
            domain.root_to_action_edge(flow, root.clone(), action.clone()),
        );
        (graph, root, action, number)
    }

    #[test]
    fn test_domain_provides_schema_registry() {
        let domain = TestGraphDomain::new();
        assert!(domain
            .schema_registry()
            .node_schema(domain.root_type())
            .is_some());
        assert!(domain
            .schema_registry()
            .node_schema(domain.action_type())
            .is_some());
    }

    #[test]
    fn valid_domain_graph_has_no_diagnostics() {
        let domain = TestGraphDomain::new();
        let (graph, _, _, _) = valid_graph(&domain);
        assert!(validate_graph_with_domain(&graph, &domain).is_empty());
    }

    #[test]
    fn fixed_schema_port_ids_survive_roundtrip_across_domain_instances() {
        let first_domain = TestGraphDomain::new();
        let (graph, _, _, _) = valid_graph(&first_domain);
        let json = graph
            .to_canonical_json(first_domain.schema_registry())
            .expect("valid fixture graph must serialize");
        let loaded: Graph = serde_json::from_str(&json).expect("graph JSON must deserialize");
        let second_domain = TestGraphDomain::new();

        let diagnostics = validate_graph_with_domain(&loaded, &second_domain);
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "graph.missing_endpoint_port"),
            "fixed schema port IDs must validate across domain instances: {diagnostics:?}"
        );
        assert!(
            diagnostics.is_empty(),
            "expected valid graph: {diagnostics:?}"
        );
    }

    #[test]
    fn domain_validation_catches_port_type_mismatch() {
        let domain = TestGraphDomain::new();
        let (mut graph, _, action, _) = valid_graph(&domain);
        let bool_node = NodeId::generate();
        graph.nodes.insert(
            bool_node.clone(),
            domain.bool_source_node(bool_node.clone()),
        );
        let edge = EdgeId::generate();
        graph.edges.insert(
            edge.clone(),
            domain.bool_to_action_number_edge(edge, bool_node, action),
        );

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "test_domain.port_type_mismatch"));
    }

    #[test]
    fn foundation_does_not_emit_port_type_mismatch() {
        let domain = TestGraphDomain::new();
        let (mut graph, _, action, _) = valid_graph(&domain);
        let bool_node = NodeId::generate();
        graph.nodes.insert(
            bool_node.clone(),
            domain.bool_source_node(bool_node.clone()),
        );
        let edge = EdgeId::generate();
        graph.edges.insert(
            edge.clone(),
            domain.bool_to_action_number_edge(edge, bool_node, action),
        );

        assert!(!graph
            .validate(domain.schema_registry())
            .iter()
            .any(|diagnostic| diagnostic.code.contains("type_mismatch")));
    }

    #[test]
    fn helper_skips_domain_validation_when_structure_is_invalid() {
        let domain = TestGraphDomain::new();
        let (mut graph, root, action, _) = valid_graph(&domain);
        graph.nodes.remove(&root);
        let edge = EdgeId::generate();
        graph
            .edges
            .insert(edge.clone(), domain.root_to_action_edge(edge, root, action));

        let diagnostics = validate_graph_with_domain(&graph, &domain);
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "graph.missing_endpoint_node"));
        assert!(!diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.starts_with("test_domain.")));
    }

    #[test]
    fn unsupported_graph_kind_is_domain_diagnostic() {
        let domain = TestGraphDomain::new();
        let graph = Graph::new(GraphId::generate(), GraphKind::new("other.graph"), "other");

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "test_domain.unsupported_graph_kind"));
    }

    #[test]
    fn missing_root_is_domain_diagnostic() {
        let domain = TestGraphDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "test_domain_graph",
        );
        let action = NodeId::generate();
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action, "act"));

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "test_domain.missing_root"));
    }

    #[test]
    fn multiple_roots_are_domain_diagnostic() {
        let domain = TestGraphDomain::new();
        let (mut graph, _, _, _) = valid_graph(&domain);
        let extra_root = NodeId::generate();
        graph
            .nodes
            .insert(extra_root.clone(), domain.root_node(extra_root));

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "test_domain.multiple_roots"));
    }

    #[test]
    fn cycle_is_domain_diagnostic() {
        let domain = TestGraphDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "test_domain_graph",
        );
        let root = NodeId::generate();
        graph.nodes.insert(root.clone(), domain.root_node(root));
        let first_action = NodeId::generate();
        graph.nodes.insert(
            first_action.clone(),
            domain.action_node(first_action.clone(), "first"),
        );
        let second_action = NodeId::generate();
        graph.nodes.insert(
            second_action.clone(),
            domain.action_node(second_action.clone(), "second"),
        );
        let forward = EdgeId::generate();
        graph.edges.insert(
            forward.clone(),
            domain.action_to_action_edge(forward, first_action.clone(), second_action.clone()),
        );
        let back = EdgeId::generate();
        graph.edges.insert(
            back.clone(),
            domain.action_to_action_edge(back, second_action, first_action),
        );

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "test_domain.cycle_not_allowed"));
    }

    #[test]
    fn required_property_is_domain_diagnostic() {
        let domain = TestGraphDomain::new();
        let (mut graph, _, action, _) = valid_graph(&domain);
        graph
            .nodes
            .insert(action.clone(), domain.action_node_without_label(action));

        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "test_domain.missing_required_property"));
    }

    #[test]
    fn graph_transaction_commit_remains_domain_agnostic() {
        let domain = TestGraphDomain::new();
        let (mut graph, _, action, _) = valid_graph(&domain);
        let bool_node = NodeId::generate();
        let edge = EdgeId::generate();
        let mut transaction = GraphTransaction::begin(&graph);
        transaction.apply(GraphCommand::AddNode {
            node: domain.bool_source_node(bool_node.clone()),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.bool_to_action_number_edge(edge, bool_node, action),
        });

        transaction
            .commit(&mut graph, domain.schema_registry())
            .expect("structurally valid but domain-invalid graph must commit");
        assert!(validate_graph_with_domain(&graph, &domain)
            .iter()
            .any(|diagnostic| diagnostic.code == "test_domain.port_type_mismatch"));
    }

    #[test]
    fn domain_validation_does_not_require_graph_view() {
        let domain = TestGraphDomain::new();
        let (graph, _, _, _) = valid_graph(&domain);
        assert!(validate_graph_with_domain(&graph, &domain).is_empty());
    }

    #[test]
    fn stable_domain_diagnostic_codes() {
        let codes = [
            "test_domain.unsupported_graph_kind",
            "test_domain.missing_root",
            "test_domain.multiple_roots",
            "test_domain.missing_required_property",
            "test_domain.port_type_mismatch",
            "test_domain.cycle_not_allowed",
        ];
        assert_eq!(codes.len(), 6);
    }
}
