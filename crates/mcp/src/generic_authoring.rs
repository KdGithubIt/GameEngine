//! Generic Graph, GraphView, and declarative UI MCP adapters.
//!
//! These handlers own only tool-shaped DTOs and delegate all authoring meaning
//! to `engine-authoring` shared services.

use crate::McpToolDescriptor;
use engine_authoring::{
    AuthoringGraphDomain, AuthoringPermissions, Graph, GraphAuthoringError,
    GraphAuthoringMutation, GraphAuthoringService, GraphAuthoringSnapshot,
    GraphAuthoringValidation, GraphCommand, GraphView, GraphViewAuthoringError,
    GraphViewAuthoringMutation, GraphViewAuthoringService, GraphViewAuthoringSnapshot,
    GraphViewAuthoringValidation, GraphViewCommand, UiAuthoringError, UiAuthoringMutation,
    UiAuthoringService, UiAuthoringSession, UiAuthoringSnapshot, UiAuthoringValidation,
    UiDocumentCommand, UnsupportedGraphKind,
};
use serde::Deserialize;
use serde_json::json;
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
}

impl GenericAuthoringMcpError {
    /// Returns the stable diagnostic-style code exposed to MCP clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedGraphKind(error) => error.code(),
            Self::Graph(error) => error.code(),
            Self::GraphView(error) => error.code(),
            Self::Ui(error) => error.code(),
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

/// Transport-neutral MCP handlers for generic Graph, GraphView, and UI authoring.
#[derive(Debug, Default, Clone, Copy)]
pub struct GenericAuthoringMcpTools;

impl GenericAuthoringMcpTools {
    /// Creates the stateless generic authoring MCP adapter.
    pub fn new() -> Self {
        Self
    }

    /// Returns descriptors for generic structured authoring tools.
    pub fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        vec![
            descriptor("graph.inspect", "Inspect the active semantic Graph.", empty_schema()),
            descriptor("graph.validate", "Validate the active semantic Graph with its built-in domain.", empty_schema()),
            descriptor("graph.preview", "Preview one atomic semantic Graph command batch.", graph_mutation_schema()),
            descriptor("graph.apply", "Apply one atomic semantic Graph command batch.", graph_mutation_schema()),
            descriptor("graph.layout.inspect", "Inspect the active GraphView presentation document.", empty_schema()),
            descriptor("graph.layout.validate", "Validate the active GraphView against its semantic Graph.", empty_schema()),
            descriptor("graph.layout.preview", "Preview one atomic GraphView presentation command batch.", graph_view_mutation_schema()),
            descriptor("graph.layout.apply", "Apply one atomic GraphView presentation command batch.", graph_view_mutation_schema()),
            descriptor("ui.inspect", "Inspect the active declarative UI document.", empty_schema()),
            descriptor("ui.validate", "Validate the active declarative UI document.", empty_schema()),
            descriptor("ui.preview", "Preview one atomic declarative UI command batch.", ui_mutation_schema()),
            descriptor("ui.apply", "Apply one atomic declarative UI command batch.", ui_mutation_schema()),
        ]
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
}

fn descriptor(name: &'static str, description: &'static str, input_schema: serde_json::Value) -> McpToolDescriptor {
    McpToolDescriptor {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

fn empty_schema() -> serde_json::Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}

fn graph_mutation_schema() -> serde_json::Value {
    mutation_schema("GraphCommand")
}

fn graph_view_mutation_schema() -> serde_json::Value {
    mutation_schema("GraphViewCommand")
}

fn ui_mutation_schema() -> serde_json::Value {
    mutation_schema("UiDocumentCommand")
}

fn mutation_schema(command_title: &'static str) -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["expected_revision", "expected_generation", "commands"],
        "properties": {
            "expected_revision": {"type": "integer", "minimum": 0},
            "expected_generation": {"type": "integer", "minimum": 0},
            "commands": {
                "type": "array",
                "items": {"type": "object", "title": command_title}
            }
        },
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_cover_generic_graph_layout_and_ui_surfaces() {
        let names = GenericAuthoringMcpTools::new()
            .tool_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"graph.inspect".to_owned()));
        assert!(names.contains(&"graph.layout.apply".to_owned()));
        assert!(names.contains(&"ui.apply".to_owned()));
        assert_eq!(names.len(), 12);
    }
}
