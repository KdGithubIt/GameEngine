//! MCP tool handlers for GameEngine authoring data.
//!
//! This crate intentionally owns only tool-shaped inputs and outputs. It does
//! not own MCP transport lifecycle, process management, or editing rules. Each
//! tool delegates structured work to the shared domain service that owns it.

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

/// AI Agent Bridge tool handlers (Phase 40, ADR 0035).
pub mod ai_agent;
/// Asset discovery and inspection tool handlers backed by the shared asset catalog.
pub mod asset;
/// Registry-driven generic authoring capability surface (ADR 0132).
pub mod capability;
/// Generic Graph, GraphView, and declarative UI authoring tool handlers.
pub mod generic_authoring;
/// Prefab creation and instantiation tool handlers backed by shared services.
pub mod prefab;
/// Scene/project tool handlers backed by the shared authoring services.
pub mod scene;
/// VFX semantic authoring tool handlers backed by the shared VFX service.
pub mod vfx;

pub use ai_agent::{
    AiAgentInput, AiAgentOutput, ai_agent_tool_descriptors, describe_session,
    handle_describe_session, handle_validate_input, validate_ai_agent_input,
};
pub use asset::{AssetInspectInput, AssetMcpTools, AssetSearchInput};
pub use capability::{
    AUTHORING_APPLY_TOOL, AUTHORING_CAPABILITIES_TOOL, AUTHORING_DESCRIBE_TOOL,
    AUTHORING_INSPECT_TOOL, AUTHORING_LIST_TOOL, AUTHORING_PREVIEW_TOOL, AUTHORING_VALIDATE_TOOL,
    AuthoringCapabilityMcpTools, AuthoringInvocationPlan, AuthoringVerb, CapabilityCoverage,
    CapabilityDescribeInput, CapabilityDescribeOutput, CapabilityInvokeInput, CapabilityListOutput,
    CapabilityMcpError, CapabilitySummaryListOutput, capability_coverage, uncovered_capabilities,
};
pub use generic_authoring::{
    GenericAuthoringMcpError, GenericAuthoringMcpTools, GraphMutationInput, GraphViewMutationInput,
    TypedDocumentMutationInput, UiMutationInput,
};
pub use prefab::{PrefabCreateInput, PrefabInstantiateInput, PrefabMcpTools};
pub use scene::{
    ComponentSchemasOutput, EntityFindInput, EntityFindOutput, EntityInspectInput,
    EntityInspectOutput, ProjectDescribeOutput, SceneMcpTools, SceneMutationInput,
};
pub use vfx::{VfxEffectInput, VfxInspectOutput, VfxMcpTools, VfxMutationInput, VfxTemplateInput};

use capability::{authorize_capability, domain_tool_descriptors};
use engine_assets::catalog::AssetCatalogError;
use engine_assets::prefab::PrefabAssetError;
use engine_authoring::{
    AuthoringCapabilityRegistry, AuthoringDomain, AuthoringPermissions, BehaviorTreeApply,
    BehaviorTreeAuthoringService, BehaviorTreeCompilation, BehaviorTreeEdgeSummary,
    BehaviorTreeLayout, BehaviorTreeNodeSummary, BehaviorTreeSchemaCatalog,
    BehaviorTreeServiceError, BehaviorTreeValidation, Graph, GraphCommand, PrefabAuthoringError,
    SceneAuthoringError,
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Returns every structured authoring tool this crate advertises.
///
/// The transport layer and ADR 0132 parity coverage both read this inventory,
/// so a registered capability cannot lose its adapter binding without failing
/// the coverage guardrail.
pub fn authoring_tool_descriptors() -> Vec<McpToolDescriptor> {
    let mut descriptors = AuthoringCapabilityMcpTools::new().tool_descriptors();
    descriptors.extend(SceneMcpTools::new().tool_descriptors());
    descriptors.extend(GenericAuthoringMcpTools::new().tool_descriptors());
    descriptors.extend(AssetMcpTools::new().tool_descriptors());
    descriptors.extend(PrefabMcpTools::new().tool_descriptors());
    descriptors.extend(VfxMcpTools::new().tool_descriptors());
    descriptors.extend(BehaviorTreeMcpTools::new().tool_descriptors());
    descriptors
}

/// Returns whether invoking an advertised tool can mutate authoritative
/// authoring state.
///
/// Transports use this shared classifier to keep read-only credentials and
/// work claims aligned with the actual tool surface. Unknown tools are treated
/// as mutating so a newly added operation cannot accidentally bypass either
/// boundary before its classification is reviewed.
pub fn tool_is_mutating(tool: &str) -> bool {
    ![
        ".describe",
        ".inspect",
        ".find",
        ".list",
        ".search",
        ".validate",
        ".preview",
        ".schemas",
        ".capabilities",
    ]
    .iter()
    .any(|suffix| tool.ends_with(suffix))
        && !matches!(tool, "project.describe" | "component.schemas")
}

/// Describes one available MCP tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    /// Tool name exposed to the MCP server layer.
    pub name: String,
    /// Human-readable tool description.
    pub description: String,
    /// JSON schema for the tool input object.
    pub input_schema: serde_json::Value,
}

/// A graph-bearing input used by read-only Behavior Tree tools.
#[derive(Debug, Deserialize)]
pub struct BehaviorTreeGraphInput {
    /// Semantic graph document to inspect.
    pub graph: Graph,
}

/// Command-application input for `behavior_tree.apply`.
#[derive(Debug, Deserialize)]
pub struct BehaviorTreeApplyInput {
    /// Semantic graph document used as the transaction source.
    pub graph: Graph,
    /// Commands to apply as one transaction.
    pub commands: Vec<GraphCommand>,
}

/// Node query output for MCP tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorTreeNodesOutput {
    /// `true` when the query completed.
    pub success: bool,
    /// Deterministic node summaries.
    pub nodes: Vec<BehaviorTreeNodeSummary>,
}

/// Edge query output for MCP tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorTreeEdgesOutput {
    /// `true` when the query completed.
    pub success: bool,
    /// Deterministic edge summaries.
    pub edges: Vec<BehaviorTreeEdgeSummary>,
}

/// Reports why an MCP tool handler could not complete.
#[derive(Debug)]
pub enum McpToolError {
    /// The authoring service rejected the request.
    Authoring {
        /// Source authoring error.
        source: BehaviorTreeServiceError,
    },
    /// The shared Scene authoring service rejected the request.
    SceneAuthoring {
        /// Source Scene authoring error.
        source: SceneAuthoringError,
    },
    /// The shared asset catalog service rejected the request.
    AssetCatalog {
        /// Source asset catalog error.
        source: AssetCatalogError,
    },
    /// The shared prefab asset service rejected the request.
    PrefabAsset {
        /// Source prefab asset error.
        source: PrefabAssetError,
    },
    /// The shared prefab Scene authoring service rejected the request.
    PrefabAuthoring {
        /// Source prefab authoring error.
        source: PrefabAuthoringError,
    },
    /// The registry-driven generic authoring surface rejected the request.
    Capability {
        /// Source capability error.
        source: CapabilityMcpError,
    },
}

impl fmt::Display for McpToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authoring { source } => source.fmt(formatter),
            Self::SceneAuthoring { source } => source.fmt(formatter),
            Self::AssetCatalog { source } => source.fmt(formatter),
            Self::PrefabAsset { source } => source.fmt(formatter),
            Self::PrefabAuthoring { source } => source.fmt(formatter),
            Self::Capability { source } => source.fmt(formatter),
        }
    }
}

impl std::error::Error for McpToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Authoring { source } => Some(source),
            Self::SceneAuthoring { source } => Some(source),
            Self::AssetCatalog { source } => Some(source),
            Self::PrefabAsset { source } => Some(source),
            Self::PrefabAuthoring { source } => Some(source),
            Self::Capability { source } => Some(source),
        }
    }
}

impl From<BehaviorTreeServiceError> for McpToolError {
    fn from(source: BehaviorTreeServiceError) -> Self {
        Self::Authoring { source }
    }
}

impl From<SceneAuthoringError> for McpToolError {
    fn from(source: SceneAuthoringError) -> Self {
        Self::SceneAuthoring { source }
    }
}

impl From<AssetCatalogError> for McpToolError {
    fn from(source: AssetCatalogError) -> Self {
        Self::AssetCatalog { source }
    }
}

impl From<PrefabAssetError> for McpToolError {
    fn from(source: PrefabAssetError) -> Self {
        Self::PrefabAsset { source }
    }
}

impl From<PrefabAuthoringError> for McpToolError {
    fn from(source: PrefabAuthoringError) -> Self {
        Self::PrefabAuthoring { source }
    }
}

impl From<CapabilityMcpError> for McpToolError {
    fn from(source: CapabilityMcpError) -> Self {
        Self::Capability { source }
    }
}

impl McpToolError {
    /// Returns a stable diagnostic-style code when the source exposes one.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Authoring { .. } => "mcp.authoring_error",
            Self::SceneAuthoring { source } => source.code(),
            Self::AssetCatalog { source } => source.code(),
            Self::PrefabAsset { source } => source.code(),
            Self::PrefabAuthoring { source } => source.code(),
            Self::Capability { source } => source.code(),
        }
    }
}

/// Behavior Tree MCP tool handler collection.
///
/// The shared Behavior Tree service operates on a graph supplied in the request
/// rather than on a permissioned session, so this adapter authorizes each call
/// against the permission its capability declares in the canonical registry
/// (ADR 0132 sections 5 and 6).
pub struct BehaviorTreeMcpTools {
    service: BehaviorTreeAuthoringService,
    registry: AuthoringCapabilityRegistry,
}

impl BehaviorTreeMcpTools {
    /// Creates Behavior Tree tool handlers backed by the shared authoring service.
    pub fn new() -> Self {
        Self {
            service: BehaviorTreeAuthoringService::new(),
            registry: AuthoringCapabilityRegistry::builtin(),
        }
    }

    /// Returns tool descriptors for registration by an MCP transport layer.
    ///
    /// Names, descriptions, and argument schemas come from the canonical
    /// authoring capability registry (ADR 0132).
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        domain_tool_descriptors(&self.registry, &[AuthoringDomain::BehaviorTree])
    }

    /// Returns Behavior Tree schema discovery data.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the permission declared
    /// for `behavior_tree.schemas`.
    pub fn behavior_tree_schemas(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<BehaviorTreeSchemaCatalog, McpToolError> {
        authorize_capability(&self.registry, "behavior_tree.schemas", permissions)?;
        Ok(self.service.schemas())
    }

    /// Validates a Behavior Tree graph.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the declared permission
    /// or the input graph belongs to another graph domain.
    pub fn behavior_tree_validate(
        &self,
        permissions: &AuthoringPermissions,
        input: BehaviorTreeGraphInput,
    ) -> Result<BehaviorTreeValidation, McpToolError> {
        authorize_capability(&self.registry, "behavior_tree.validate", permissions)?;
        self.service.ensure_behavior_tree_graph(&input.graph)?;
        Ok(self.service.validate(&input.graph))
    }

    /// Compiles a Behavior Tree graph.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the declared permission
    /// or the input graph belongs to another graph domain.
    pub fn behavior_tree_compile(
        &self,
        permissions: &AuthoringPermissions,
        input: BehaviorTreeGraphInput,
    ) -> Result<BehaviorTreeCompilation, McpToolError> {
        authorize_capability(&self.registry, "behavior_tree.compile", permissions)?;
        self.service.ensure_behavior_tree_graph(&input.graph)?;
        Ok(self.service.compile(&input.graph))
    }

    /// Generates a Behavior Tree graph view.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the declared permission
    /// or the input graph belongs to another graph domain.
    pub fn behavior_tree_layout(
        &self,
        permissions: &AuthoringPermissions,
        input: BehaviorTreeGraphInput,
    ) -> Result<BehaviorTreeLayout, McpToolError> {
        authorize_capability(&self.registry, "behavior_tree.layout", permissions)?;
        self.service.ensure_behavior_tree_graph(&input.graph)?;
        Ok(self.service.layout(&input.graph))
    }

    /// Lists Behavior Tree nodes.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the declared permission
    /// or the input graph belongs to another graph domain.
    pub fn behavior_tree_nodes(
        &self,
        permissions: &AuthoringPermissions,
        input: BehaviorTreeGraphInput,
    ) -> Result<BehaviorTreeNodesOutput, McpToolError> {
        authorize_capability(&self.registry, "behavior_tree.nodes", permissions)?;
        self.service.ensure_behavior_tree_graph(&input.graph)?;
        Ok(BehaviorTreeNodesOutput {
            success: true,
            nodes: self.service.nodes(&input.graph),
        })
    }

    /// Lists Behavior Tree edges.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the declared permission
    /// or the input graph belongs to another graph domain.
    pub fn behavior_tree_edges(
        &self,
        permissions: &AuthoringPermissions,
        input: BehaviorTreeGraphInput,
    ) -> Result<BehaviorTreeEdgesOutput, McpToolError> {
        authorize_capability(&self.registry, "behavior_tree.edges", permissions)?;
        self.service.ensure_behavior_tree_graph(&input.graph)?;
        Ok(BehaviorTreeEdgesOutput {
            success: true,
            edges: self.service.edges(&input.graph),
        })
    }

    /// Applies Behavior Tree graph commands as one transaction.
    ///
    /// The source graph is not mutated. A successful result includes the
    /// updated graph so the MCP transport layer can return it to the caller or
    /// persist it through a separate policy.
    ///
    /// # Errors
    ///
    /// Returns [`McpToolError`] when the session lacks the declared permission
    /// or the input graph belongs to another graph domain.
    pub fn behavior_tree_apply(
        &self,
        permissions: &AuthoringPermissions,
        input: BehaviorTreeApplyInput,
    ) -> Result<BehaviorTreeApply, McpToolError> {
        authorize_capability(&self.registry, "behavior_tree.apply", permissions)?;
        self.service.ensure_behavior_tree_graph(&input.graph)?;
        Ok(self.service.apply(&input.graph, input.commands))
    }
}

impl Default for BehaviorTreeMcpTools {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{AuthoringPermission, BehaviorTreeDomain, EdgeId, GraphId, NodeId};

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    #[test]
    fn descriptors_include_bulk_apply_tool() {
        let tools = BehaviorTreeMcpTools::new();
        let names = tools
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"behavior_tree.schemas".to_owned()));
        assert!(names.contains(&"behavior_tree.apply".to_owned()));
        assert_eq!(names.len(), 7);
    }

    #[test]
    fn schema_tool_delegates_behavior_tree_schemas() {
        let tools = BehaviorTreeMcpTools::new();
        let schemas = tools
            .behavior_tree_schemas(&AuthoringPermissions::read_only())
            .expect("schema discovery is a read operation");

        let expected = BehaviorTreeAuthoringService::new().schemas();

        assert_eq!(schemas, expected);
    }

    #[test]
    fn apply_tool_returns_updated_graph() {
        let domain = BehaviorTreeDomain::new();
        let graph = valid_graph(&domain);
        let root_count_before = graph.nodes.len();
        let sequence_id = graph
            .nodes
            .iter()
            .find(|(_, node)| node.node_type == *domain.sequence_type())
            .map(|(id, _)| id.clone())
            .expect("fixture must contain a sequence");
        let new_action = NodeId::generate();
        let commands = vec![
            GraphCommand::AddNode {
                node: domain.action_node(new_action.clone(), "extra_action"),
            },
            GraphCommand::AddEdge {
                edge: domain.child_edge(EdgeId::generate(), sequence_id, new_action, 1),
            },
        ];

        let result = BehaviorTreeMcpTools::new()
            .behavior_tree_apply(&writable(), BehaviorTreeApplyInput { graph, commands })
            .expect("apply tool must run");

        assert!(result.success);
        assert_eq!(result.diff.len(), 2);
        assert_eq!(
            result
                .graph()
                .expect("successful apply must include graph")
                .nodes
                .len(),
            root_count_before + 1
        );
    }

    #[test]
    fn read_only_sessions_cannot_commit_behavior_tree_mutations() {
        let domain = BehaviorTreeDomain::new();
        let graph = valid_graph(&domain);

        let error = BehaviorTreeMcpTools::new()
            .behavior_tree_apply(
                &AuthoringPermissions::read_only(),
                BehaviorTreeApplyInput {
                    graph,
                    commands: Vec::new(),
                },
            )
            .expect_err("read-only sessions must not commit");

        assert_eq!(error.code(), "authoring.permission_denied");
    }

    #[test]
    fn wrong_domain_is_tool_error_not_successful_validation() {
        let graph = Graph::new(
            GraphId::generate(),
            engine_authoring::GraphKind::new("other.graph"),
            "wrong",
        );

        let result = BehaviorTreeMcpTools::new()
            .behavior_tree_validate(&writable(), BehaviorTreeGraphInput { graph });

        assert!(matches!(result, Err(McpToolError::Authoring { .. })));
    }

    fn valid_graph(domain: &BehaviorTreeDomain) -> Graph {
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "valid_behavior_tree",
        );
        let root = NodeId::generate();
        let sequence = NodeId::generate();
        let action = NodeId::generate();
        graph
            .nodes
            .insert(root.clone(), domain.root_node(root.clone()));
        graph
            .nodes
            .insert(sequence.clone(), domain.sequence_node(sequence.clone()));
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action.clone(), "idle"));
        let root_edge = domain.child_edge(EdgeId::generate(), root, sequence.clone(), 0);
        graph.edges.insert(root_edge.id.clone(), root_edge);
        let action_edge = domain.child_edge(EdgeId::generate(), sequence, action, 0);
        graph.edges.insert(action_edge.id.clone(), action_edge);
        graph
    }
}
