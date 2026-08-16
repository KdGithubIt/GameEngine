//! Structured bulk authoring entry points owned by the live Editor session.
//!
//! These methods are used by non-widget adapters such as project-scoped MCP.
//! Existing interactive micro-edits remain command-backed through their
//! dedicated Editor operations so temporary domain-invalid construction states
//! continue to be possible.

use super::EditorSession;
use crate::document::CurrentDocument;
use engine_authoring::{
    AuthoringGraphDomain, AuthoringPermissions, GraphAuthoringError, GraphAuthoringMutation,
    GraphAuthoringService, GraphAuthoringSnapshot, GraphAuthoringValidation, GraphCommand,
    GraphViewAuthoringError, GraphViewAuthoringMutation, GraphViewAuthoringService,
    GraphViewAuthoringSnapshot, GraphViewAuthoringValidation, GraphViewCommand, UiAuthoringError,
    UiAuthoringMutation, UiAuthoringService, UiAuthoringSnapshot, UiAuthoringValidation,
    UiDocumentCommand, UnsupportedGraphKind,
};
use std::fmt;

/// Failure returned by the Editor's structured authoring surface.
#[derive(Debug)]
pub enum StructuredAuthoringError {
    /// No built-in authoring domain owns the active graph kind.
    UnsupportedGraphKind(UnsupportedGraphKind),
    /// The active Editor tab is not a Graph document.
    NoGraphDocument,
    /// Shared semantic Graph authoring rejected the request.
    Graph(GraphAuthoringError),
    /// The active Graph has no GraphView presentation document.
    NoGraphView,
    /// Shared GraphView authoring rejected the request.
    GraphView(GraphViewAuthoringError),
    /// The active document is not a declarative UI document.
    NoUiDocument,
    /// Shared declarative UI authoring rejected the request.
    Ui(UiAuthoringError),
}

impl StructuredAuthoringError {
    /// Returns a stable diagnostic-style code for adapter responses.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedGraphKind(error) => error.code(),
            Self::NoGraphDocument => "editor.no_graph_document",
            Self::Graph(error) => error.code(),
            Self::NoGraphView => "editor.no_graph_view_document",
            Self::GraphView(error) => error.code(),
            Self::NoUiDocument => "editor.no_ui_document",
            Self::Ui(error) => error.code(),
        }
    }
}

impl fmt::Display for StructuredAuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedGraphKind(error) => error.fmt(formatter),
            Self::NoGraphDocument => formatter.write_str("the active Editor tab is not a Graph document"),
            Self::Graph(error) => error.fmt(formatter),
            Self::NoGraphView => formatter.write_str("the active Graph has no GraphView document"),
            Self::GraphView(error) => error.fmt(formatter),
            Self::NoUiDocument => formatter.write_str("the active Editor tab is not a UI document"),
            Self::Ui(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StructuredAuthoringError {}

impl From<UnsupportedGraphKind> for StructuredAuthoringError {
    fn from(error: UnsupportedGraphKind) -> Self {
        Self::UnsupportedGraphKind(error)
    }
}

impl From<GraphAuthoringError> for StructuredAuthoringError {
    fn from(error: GraphAuthoringError) -> Self {
        Self::Graph(error)
    }
}

impl From<GraphViewAuthoringError> for StructuredAuthoringError {
    fn from(error: GraphViewAuthoringError) -> Self {
        Self::GraphView(error)
    }
}

impl From<UiAuthoringError> for StructuredAuthoringError {
    fn from(error: UiAuthoringError) -> Self {
        Self::Ui(error)
    }
}

impl EditorSession {
    fn require_structured_graph_document(&self) -> Result<(), StructuredAuthoringError> {
        if matches!(self.current_document, CurrentDocument::Graph { .. } | CurrentDocument::None) {
            Ok(())
        } else {
            Err(StructuredAuthoringError::NoGraphDocument)
        }
    }

    /// Inspects the active semantic Graph through the shared authoring service.
    pub fn structured_graph_inspect(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphAuthoringSnapshot, StructuredAuthoringError> {
        self.require_structured_graph_document()?;
        Ok(GraphAuthoringService::new().inspect(&self.graph, permissions)?)
    }

    /// Validates the active semantic Graph through its built-in domain.
    pub fn structured_graph_validate(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphAuthoringValidation, StructuredAuthoringError> {
        self.require_structured_graph_document()?;
        let domain = AuthoringGraphDomain::for_graph(&self.graph)?;
        Ok(GraphAuthoringService::new().validate(&self.graph, &domain, permissions)?)
    }

    /// Previews one atomic semantic Graph command batch.
    pub fn structured_graph_preview(
        &self,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<GraphCommand>,
    ) -> Result<GraphAuthoringMutation, StructuredAuthoringError> {
        self.require_structured_graph_document()?;
        let domain = AuthoringGraphDomain::for_graph(&self.graph)?;
        Ok(GraphAuthoringService::new().preview(
            &self.graph,
            &domain,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?)
    }

    /// Applies one semantic Graph batch as one Editor undo operation.
    pub fn structured_graph_apply(
        &mut self,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<GraphCommand>,
    ) -> Result<GraphAuthoringMutation, StructuredAuthoringError> {
        self.require_structured_graph_document()?;
        let domain = AuthoringGraphDomain::for_graph(&self.graph)?;
        let checkpoint = self.snapshot();
        let mutation = GraphAuthoringService::new().apply(
            &mut self.graph,
            &domain,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?;
        self.diagnostics = mutation.diagnostics.clone();
        if mutation.success && !mutation.diff.is_empty() {
            if let Some(checkpoint) = checkpoint {
                self.undo_stack.push(checkpoint);
            }
            self.prune_graph_view();
            self.mark_dirty();
        }
        Ok(mutation)
    }

    /// Inspects the active GraphView presentation document.
    pub fn structured_graph_view_inspect(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphViewAuthoringSnapshot, StructuredAuthoringError> {
        self.require_structured_graph_document()?;
        let view = self
            .graph_view
            .as_ref()
            .ok_or(StructuredAuthoringError::NoGraphView)?;
        Ok(GraphViewAuthoringService::new().inspect(view, permissions)?)
    }

    /// Validates the active GraphView against its semantic Graph.
    pub fn structured_graph_view_validate(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphViewAuthoringValidation, StructuredAuthoringError> {
        self.require_structured_graph_document()?;
        let view = self
            .graph_view
            .as_ref()
            .ok_or(StructuredAuthoringError::NoGraphView)?;
        Ok(GraphViewAuthoringService::new().validate(&self.graph, view, permissions)?)
    }

    /// Previews one GraphView presentation command batch.
    pub fn structured_graph_view_preview(
        &self,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<GraphViewCommand>,
    ) -> Result<GraphViewAuthoringMutation, StructuredAuthoringError> {
        self.require_structured_graph_document()?;
        let view = self
            .graph_view
            .as_ref()
            .ok_or(StructuredAuthoringError::NoGraphView)?;
        Ok(GraphViewAuthoringService::new().preview(
            &self.graph,
            view,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?)
    }

    /// Applies one GraphView batch as one Editor undo operation.
    pub fn structured_graph_view_apply(
        &mut self,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<GraphViewCommand>,
    ) -> Result<GraphViewAuthoringMutation, StructuredAuthoringError> {
        self.require_structured_graph_document()?;
        let checkpoint = self.snapshot();
        let view = self
            .graph_view
            .as_mut()
            .ok_or(StructuredAuthoringError::NoGraphView)?;
        let mutation = GraphViewAuthoringService::new().apply(
            &self.graph,
            view,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?;
        self.diagnostics = mutation.diagnostics.clone();
        if mutation.success && !mutation.diff.is_empty() {
            if let Some(checkpoint) = checkpoint {
                self.undo_stack.push(checkpoint);
            }
            self.mark_dirty();
        }
        Ok(mutation)
    }

    /// Inspects the active declarative UI document through its live shared session.
    pub fn structured_ui_inspect(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<UiAuthoringSnapshot, StructuredAuthoringError> {
        let session = self
            .ui_authoring_session
            .as_ref()
            .ok_or(StructuredAuthoringError::NoUiDocument)?;
        Ok(UiAuthoringService::new().inspect(session, permissions)?)
    }

    /// Validates the active declarative UI document through its live shared session.
    pub fn structured_ui_validate(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<UiAuthoringValidation, StructuredAuthoringError> {
        let session = self
            .ui_authoring_session
            .as_ref()
            .ok_or(StructuredAuthoringError::NoUiDocument)?;
        Ok(UiAuthoringService::new().validate(session, permissions)?)
    }

    /// Previews one declarative UI command batch.
    pub fn structured_ui_preview(
        &self,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<UiDocumentCommand>,
    ) -> Result<UiAuthoringMutation, StructuredAuthoringError> {
        let session = self
            .ui_authoring_session
            .as_ref()
            .ok_or(StructuredAuthoringError::NoUiDocument)?;
        Ok(UiAuthoringService::new().preview(
            session,
            permissions,
            expected_revision,
            expected_generation,
            commands,
        )?)
    }

    /// Applies one declarative UI batch as one Editor undo operation.
    pub fn structured_ui_apply(
        &mut self,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<UiDocumentCommand>,
    ) -> Result<UiAuthoringMutation, StructuredAuthoringError> {
        let checkpoint = self.snapshot();
        let (mutation, updated_document) = {
            let session = self
                .ui_authoring_session
                .as_mut()
                .ok_or(StructuredAuthoringError::NoUiDocument)?;
            let mutation = UiAuthoringService::new().apply(
                session,
                permissions,
                expected_revision,
                expected_generation,
                commands,
            )?;
            let updated_document = (mutation.success && !mutation.diff.is_empty())
                .then(|| session.document().clone());
            (mutation, updated_document)
        };
        self.diagnostics = mutation.diagnostics.clone();
        if let Some(updated_document) = updated_document {
            if let Some(checkpoint) = checkpoint {
                self.undo_stack.push(checkpoint);
            }
            if let CurrentDocument::Ui { document, .. } = &mut self.current_document {
                *document = updated_document;
            }
            self.mark_dirty();
        }
        Ok(mutation)
    }
}
