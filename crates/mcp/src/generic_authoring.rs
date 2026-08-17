//! Generic Graph, GraphView, and declarative UI MCP adapters.
//!
//! These handlers own only tool-shaped DTOs and delegate all authoring meaning
//! to `engine-authoring` shared services.

use crate::capability::domain_tool_descriptors;
use crate::McpToolDescriptor;
use engine_authoring::{
    AuthoringCapabilityRegistry, AuthoringDomain,
    AuthoringGraphDomain, AuthoringPermissions, Graph, GraphAuthoringError,
    GraphAuthoringMutation, GraphAuthoringService, GraphAuthoringSnapshot,
    GraphAuthoringValidation, GraphCommand, GraphView, GraphViewAuthoringError,
    GraphViewAuthoringMutation, GraphViewAuthoringService, GraphViewAuthoringSnapshot,
    GraphViewAuthoringValidation, GraphViewCommand, TimelineAuthoringCommand,
    TimelineAuthoringError, TimelineAuthoringMutation, TimelineAuthoringService,
    TimelineAuthoringSnapshot, TimelineAuthoringValidation, UiAuthoringError, UiAuthoringMutation,
    UiAuthoringService, UiAuthoringSession, UiAuthoringSnapshot, UiAuthoringValidation,
    UiDocumentCommand, UnsupportedGraphKind,
};
use serde::Deserialize;
use std::fmt;

/// Mutation request shared by generic Graph preview and apply tools.
#[derive(Debug, Deserialize)]
pub struct GraphMutationInput {
    /// Authoritative Graph revision observed by the caller.
    pub expected_revision: u64,
    /// Authoritative in-memory Graph generation observed by the caller.
    pub expected_generation: u64,
    /// Semantic commands to apply atomically.
    pub commands: Vec<GraphCommand>,
}

/// Mutation request shared by GraphView preview and apply tools.
#[derive(Debug, Deserialize)]
pub struct GraphViewMutationInput {
    /// Authoritative GraphView revision observed by the caller.
    pub expected_revision: u64,
    /// Authoritative in-memory GraphView generation observed by the caller.
    pub expected_generation: u64,
    /// Presentation commands to apply atomically.
    pub commands: Vec<GraphViewCommand>,
}

/// Mutation request shared by declarative UI preview and apply tools.
#[derive(Debug, Deserialize)]
pub struct UiMutationInput {
    /// Authoritative UI document revision observed by the caller.
    pub expected_revision: u64,
    /// Authoritative in-memory UI generation observed by the caller.
    pub expected_generation: u64,
    /// UI commands to apply atomically.
    pub commands: Vec<UiDocumentCommand>,
}

/// Mutation request shared by Timeline preview and apply tools.
#[derive(Debug, Deserialize)]
pub struct TimelineMutationInput {
    /// Authoritative Timeline revision observed by the caller.
    pub expected_revision: u64,
    /// Authoritative live-session generation observed by the caller.
    pub expected_generation: u64,
    /// Timeline commands to apply atomically.
    pub commands: Vec<TimelineAuthoringCommand>,
}

/// Whole-document replacement request shared by persisted typed-document tools.
///
/// The active Editor supplies the authoritative document and revision state;
/// this DTO carries only the adapter-neutral semantic intent.
#[derive(Debug, Deserialize)]
pub struct TypedDocumentMutationInput<T> {
    /// Authoritative document revision observed by the caller.
    pub expected_revision: u64,
    /// Authoritative in-memory generation observed by the caller.
    pub expected_generation: u64,
    /// Complete typed replacement evaluated by the shared authoring service.
    pub replacement: T,
}

/// Failure returned by generic structured authoring MCP handlers.
#[derive(Debug)]
pub enum GenericAuthoringMcpError {
    /// No built-in Graph domain owns the supplied graph kind.
    UnsupportedGraphKind(UnsupportedGraphKind),
    /// Shared semantic Graph authoring rejected the request.
    Graph(GraphAuthoringError),
    /// Shared GraphView authoring rejected the request.
    GraphView(GraphViewAuthoringError),
    /// Shared declarative UI authoring rejected the request.
    Ui(UiAuthoringError),
    /// Shared Timeline authoring rejected the request.
    Timeline(TimelineAuthoringError),
}

impl GenericAuthoringMcpError {
    /// Returns the stable diagnostic-style code exposed to MCP clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedGraphKind(error) => error.code(),
            Self::Graph(error) => error.code(),
            Self::GraphView(error) => error.code(),
            Self::Ui(error) => error.code(),
            Self::Timeline(error) => error.code(),
        }
    }
}

impl fmt::Display for GenericAuthoringMcpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedGraphKind(error) => error.fmt(formatter),
            Self::Graph(error) => error.fmt(formatter),
            Self::GraphView(error) => error.fmt(formatter),
            Self::Ui(error) => error.fmt(formatter),
            Self::Timeline(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for GenericAuthoringMcpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnsupportedGraphKind(error) => Some(error),
            Self::Graph(error) => Some(error),
            Self::GraphView(error) => Some(error),
            Self::Ui(error) => Some(error),
            Self::Timeline(error) => Some(error),
        }
    }
}

impl From<UnsupportedGraphKind> for GenericAuthoringMcpError {
    fn from(error: UnsupportedGraphKind) -> Self {
        Self::UnsupportedGraphKind(error)
    }
}

impl From<GraphAuthoringError> for GenericAuthoringMcpError {
    fn from(error: GraphAuthoringError) -> Self {
        Self::Graph(error)
    }
}

impl From<GraphViewAuthoringError> for GenericAuthoringMcpError {
    fn from(error: GraphViewAuthoringError) -> Self {
        Self::GraphView(error)
    }
}

impl From<UiAuthoringError> for GenericAuthoringMcpError {
    fn from(error: UiAuthoringError) -> Self {
        Self::Ui(error)
    }
}

impl From<TimelineAuthoringError> for GenericAuthoringMcpError {
    fn from(error: TimelineAuthoringError) -> Self {
        Self::Timeline(error)
    }
}

/// Transport-neutral MCP handlers for generic Graph, GraphView, UI, and Timeline authoring.
#[derive(Debug, Default, Clone, Copy)]
pub struct GenericAuthoringMcpTools;

impl GenericAuthoringMcpTools {
    /// Creates the stateless generic authoring MCP adapter.
    pub fn new() -> Self {
        Self
    }

    /// Returns descriptors for generic structured authoring tools.
    ///
    /// Names, descriptions, and argument schemas are derived from the canonical
    /// authoring capability registry (ADR 0132) rather than maintained here.
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        domain_tool_descriptors(
            &AuthoringCapabilityRegistry::builtin(),
            &[
                AuthoringDomain::Graph,
                AuthoringDomain::GraphView,
                AuthoringDomain::Ui,
                AuthoringDomain::Material,
                AuthoringDomain::ProjectSettings,
                AuthoringDomain::AnimationSet,
                AuthoringDomain::Timeline,
            ],
        )
    }

    /// Inspects a semantic Graph through the shared service.
    pub fn graph_inspect(
        &self,
        graph: &Graph,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphAuthoringSnapshot, GenericAuthoringMcpError> {
        Ok(GraphAuthoringService::new().inspect(graph, permissions)?)
    }

    /// Validates a semantic Graph through its shared built-in domain.
    pub fn graph_validate(
        &self,
        graph: &Graph,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphAuthoringValidation, GenericAuthoringMcpError> {
        let domain = AuthoringGraphDomain::for_graph(graph)?;
        Ok(GraphAuthoringService::new().validate(graph, &domain, permissions)?)
    }

    /// Previews semantic Graph commands without mutation.
    pub fn graph_preview(
        &self,
        graph: &Graph,
        permissions: &AuthoringPermissions,
        input: GraphMutationInput,
    ) -> Result<GraphAuthoringMutation, GenericAuthoringMcpError> {
        let domain = AuthoringGraphDomain::for_graph(graph)?;
        Ok(GraphAuthoringService::new().preview(
            graph,
            &domain,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Applies semantic Graph commands to the authoritative live Graph.
    pub fn graph_apply(
        &self,
        graph: &mut Graph,
        permissions: &AuthoringPermissions,
        input: GraphMutationInput,
    ) -> Result<GraphAuthoringMutation, GenericAuthoringMcpError> {
        let domain = AuthoringGraphDomain::for_graph(graph)?;
        Ok(GraphAuthoringService::new().apply(
            graph,
            &domain,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Inspects GraphView presentation state through the shared service.
    pub fn graph_view_inspect(
        &self,
        view: &GraphView,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphViewAuthoringSnapshot, GenericAuthoringMcpError> {
        Ok(GraphViewAuthoringService::new().inspect(view, permissions)?)
    }

    /// Validates GraphView presentation state against its semantic Graph.
    pub fn graph_view_validate(
        &self,
        graph: &Graph,
        view: &GraphView,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphViewAuthoringValidation, GenericAuthoringMcpError> {
        Ok(GraphViewAuthoringService::new().validate(graph, view, permissions)?)
    }

    /// Previews GraphView commands without changing presentation state.
    pub fn graph_view_preview(
        &self,
        graph: &Graph,
        view: &GraphView,
        permissions: &AuthoringPermissions,
        input: GraphViewMutationInput,
    ) -> Result<GraphViewAuthoringMutation, GenericAuthoringMcpError> {
        Ok(GraphViewAuthoringService::new().preview(
            graph,
            view,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Applies GraphView commands to the authoritative presentation document.
    pub fn graph_view_apply(
        &self,
        graph: &Graph,
        view: &mut GraphView,
        permissions: &AuthoringPermissions,
        input: GraphViewMutationInput,
    ) -> Result<GraphViewAuthoringMutation, GenericAuthoringMcpError> {
        Ok(GraphViewAuthoringService::new().apply(
            graph,
            view,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Inspects a live declarative UI authoring session.
    pub fn ui_inspect(
        &self,
        session: &UiAuthoringSession,
        permissions: &AuthoringPermissions,
    ) -> Result<UiAuthoringSnapshot, GenericAuthoringMcpError> {
        Ok(UiAuthoringService::new().inspect(session, permissions)?)
    }

    /// Validates a live declarative UI authoring session.
    pub fn ui_validate(
        &self,
        session: &UiAuthoringSession,
        permissions: &AuthoringPermissions,
    ) -> Result<UiAuthoringValidation, GenericAuthoringMcpError> {
        Ok(UiAuthoringService::new().validate(session, permissions)?)
    }

    /// Previews declarative UI commands without mutation.
    pub fn ui_preview(
        &self,
        session: &UiAuthoringSession,
        permissions: &AuthoringPermissions,
        input: UiMutationInput,
    ) -> Result<UiAuthoringMutation, GenericAuthoringMcpError> {
        Ok(UiAuthoringService::new().preview(
            session,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Applies declarative UI commands to the authoritative live UI session.
    pub fn ui_apply(
        &self,
        session: &mut UiAuthoringSession,
        permissions: &AuthoringPermissions,
        input: UiMutationInput,
    ) -> Result<UiAuthoringMutation, GenericAuthoringMcpError> {
        Ok(UiAuthoringService::new().apply(
            session,
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Inspects a live Timeline authoring session.
    pub fn timeline_inspect(
        &self,
        session: &TimelineAuthoringService,
        permissions: &AuthoringPermissions,
    ) -> Result<TimelineAuthoringSnapshot, GenericAuthoringMcpError> {
        Ok(session.inspect(permissions)?)
    }

    /// Validates a live Timeline authoring session.
    pub fn timeline_validate(
        &self,
        session: &TimelineAuthoringService,
        permissions: &AuthoringPermissions,
    ) -> Result<TimelineAuthoringValidation, GenericAuthoringMcpError> {
        Ok(session.validate(permissions)?)
    }

    /// Previews Timeline commands without mutation.
    pub fn timeline_preview(
        &self,
        session: &TimelineAuthoringService,
        permissions: &AuthoringPermissions,
        input: TimelineMutationInput,
    ) -> Result<TimelineAuthoringMutation, GenericAuthoringMcpError> {
        Ok(session.preview_commands(
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }

    /// Applies Timeline commands to the authoritative live session.
    pub fn timeline_apply(
        &self,
        session: &mut TimelineAuthoringService,
        permissions: &AuthoringPermissions,
        input: TimelineMutationInput,
    ) -> Result<TimelineAuthoringMutation, GenericAuthoringMcpError> {
        Ok(session.apply_commands(
            permissions,
            input.expected_revision,
            input.expected_generation,
            input.commands,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::AuthoringCapabilityId;
    use serde_json::json;

    #[test]
    fn descriptors_cover_generic_graph_ui_and_typed_document_surfaces() {
        let names = GenericAuthoringMcpTools::new()
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"graph.inspect".to_owned()));
        assert!(names.contains(&"graph.layout.apply".to_owned()));
        assert!(names.contains(&"ui.apply".to_owned()));
        assert!(names.contains(&"material.inspect".to_owned()));
        assert!(names.contains(&"project_settings.apply".to_owned()));
        assert!(names.contains(&"animation_set.validate".to_owned()));
        assert!(names.contains(&"timeline.inspect".to_owned()));
        assert!(names.contains(&"timeline.apply".to_owned()));
        assert_eq!(names.len(), 28);
    }

    #[test]
    fn command_batch_schemas_name_their_shared_command_type() {
        let registry = AuthoringCapabilityRegistry::builtin();
        let descriptors = GenericAuthoringMcpTools::new().tool_descriptors();
        let apply = descriptors
            .iter()
            .find(|descriptor| descriptor.name == "ui.apply")
            .expect("ui.apply must be advertised");
        let capability = registry
            .require(&AuthoringCapabilityId::new("ui.apply"))
            .expect("ui.apply must be registered");

        assert_eq!(apply.input_schema, capability.input.json_schema);
        assert_eq!(
            apply.input_schema["properties"]["commands"]["items"]["title"],
            json!("UiDocumentCommand")
        );

        let timeline_apply = descriptors
            .iter()
            .find(|descriptor| descriptor.name == "timeline.apply")
            .expect("timeline.apply must be advertised");
        let timeline_capability = registry
            .require(&AuthoringCapabilityId::new("timeline.apply"))
            .expect("timeline.apply must be registered");
        assert_eq!(timeline_apply.input_schema, timeline_capability.input.json_schema);
        assert_eq!(
            timeline_apply.input_schema["properties"]["commands"]["items"]["title"],
            json!("TimelineAuthoringCommand")
        );
    }
}
