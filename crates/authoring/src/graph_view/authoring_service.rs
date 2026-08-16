//! Shared structured authoring service for GraphView presentation documents.
//!
//! GraphView remains a separate presentation document from the semantic Graph.
//! This service centralizes permission checks, stale-base rejection, preview
//! diffs, validation, and atomic presentation commits for structured adapters.

use super::{GraphView, GraphViewChange, GraphViewCommand, GraphViewTransaction, GraphViewTransactionError};
use crate::access::{AuthoringPermission, AuthoringPermissionError, AuthoringPermissions};
use crate::diagnostic::Diagnostic;
use crate::graph::Graph;
use serde::Serialize;
use std::fmt;

/// Immutable GraphView state returned by structured authoring inspection.
#[derive(Debug, Serialize)]
pub struct GraphViewAuthoringSnapshot {
    /// Logical content revision of the committed GraphView.
    pub revision: u64,
    /// In-memory generation used to reject stale edits across reopen or undo.
    pub generation: u64,
    /// Complete committed presentation document.
    pub view: GraphView,
}

/// Structured validation result for one committed GraphView.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphViewAuthoringValidation {
    /// Logical content revision validated by this result.
    pub revision: u64,
    /// In-memory generation validated by this result.
    pub generation: u64,
    /// Whether no blocking presentation diagnostic was produced.
    pub success: bool,
    /// Structured GraphView diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Result of previewing or applying one GraphView command batch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphViewAuthoringMutation {
    /// Whether the complete command batch passed validation.
    pub success: bool,
    /// Revision supplied by the caller as its base.
    pub base_revision: u64,
    /// Generation supplied by the caller as its base.
    pub base_generation: u64,
    /// Current committed revision after this operation.
    pub revision: u64,
    /// Current committed generation after this operation.
    pub generation: u64,
    /// Structured command or presentation diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Deterministic presentation diff proposed or committed by the batch.
    pub diff: Vec<GraphViewChange>,
}

/// Shared GraphView authoring service failure.
#[derive(Debug)]
pub enum GraphViewAuthoringError {
    /// The application did not grant the permission required by this operation.
    Permission(AuthoringPermissionError),
    /// The supplied mutation base no longer matches the live GraphView.
    Stale {
        /// Revision supplied by the caller.
        expected_revision: u64,
        /// Generation supplied by the caller.
        expected_generation: u64,
        /// Current committed GraphView revision.
        actual_revision: u64,
        /// Current committed GraphView generation.
        actual_generation: u64,
    },
}

impl GraphViewAuthoringError {
    /// Returns a stable diagnostic-style error code for adapter responses.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Permission(error) => error.code(),
            Self::Stale { .. } => "authoring.stale_revision",
        }
    }
}

impl fmt::Display for GraphViewAuthoringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permission(error) => error.fmt(formatter),
            Self::Stale {
                expected_revision,
                expected_generation,
                actual_revision,
                actual_generation,
            } => write!(
                formatter,
                "stale GraphView base: expected revision {expected_revision} generation {expected_generation}, current revision {actual_revision} generation {actual_generation}"
            ),
        }
    }
}

impl std::error::Error for GraphViewAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(error) => Some(error),
            Self::Stale { .. } => None,
        }
    }
}

impl From<AuthoringPermissionError> for GraphViewAuthoringError {
    fn from(value: AuthoringPermissionError) -> Self {
        Self::Permission(value)
    }
}

/// GUI-free GraphView authoring behavior shared by structured adapters.
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphViewAuthoringService;

impl GraphViewAuthoringService {
    /// Creates the stateless GraphView authoring service.
    pub fn new() -> Self {
        Self
    }

    /// Inspects the current committed GraphView presentation document.
    ///
    /// # Errors
    ///
    /// Returns [`GraphViewAuthoringError`] when read permission is absent.
    pub fn inspect(
        &self,
        view: &GraphView,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphViewAuthoringSnapshot, GraphViewAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        Ok(GraphViewAuthoringSnapshot {
            revision: view.revision(),
            generation: view.identity(),
            view: view.clone_for_transaction(),
        })
    }

    /// Validates the current GraphView against its semantic Graph.
    ///
    /// # Errors
    ///
    /// Returns [`GraphViewAuthoringError`] when read permission is absent.
    pub fn validate(
        &self,
        graph: &Graph,
        view: &GraphView,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphViewAuthoringValidation, GraphViewAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        let diagnostics = view.validate(graph);
        let success = !diagnostics.iter().any(Diagnostic::is_blocking);
        Ok(GraphViewAuthoringValidation {
            revision: view.revision(),
            generation: view.identity(),
            success,
            diagnostics,
        })
    }

    /// Previews one atomic GraphView command batch without changing live state.
    ///
    /// # Errors
    ///
    /// Returns [`GraphViewAuthoringError`] when preview permission is absent or
    /// the supplied revision/generation pair is stale.
    pub fn preview(
        &self,
        graph: &Graph,
        view: &GraphView,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<GraphViewCommand>,
    ) -> Result<GraphViewAuthoringMutation, GraphViewAuthoringError> {
        permissions.require(AuthoringPermission::Preview)?;
        ensure_current(view, expected_revision, expected_generation)?;
        evaluate(graph, view, expected_revision, expected_generation, commands)
    }

    /// Applies one atomic GraphView command batch to the live presentation document.
    ///
    /// # Errors
    ///
    /// Returns [`GraphViewAuthoringError`] when project-data-write permission is
    /// absent or the supplied revision/generation pair is stale.
    pub fn apply(
        &self,
        graph: &Graph,
        view: &mut GraphView,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<GraphViewCommand>,
    ) -> Result<GraphViewAuthoringMutation, GraphViewAuthoringError> {
        permissions.require(AuthoringPermission::ProjectDataWrite)?;
        ensure_current(view, expected_revision, expected_generation)?;

        let mut transaction = GraphViewTransaction::begin(view);
        for command in commands {
            transaction.apply(command);
        }
        let diff = transaction.preview_diff().to_vec();
        if diff.is_empty() {
            let diagnostics = view.validate(graph);
            let success = !diagnostics.iter().any(Diagnostic::is_blocking);
            return Ok(GraphViewAuthoringMutation {
                success,
                base_revision: expected_revision,
                base_generation: expected_generation,
                revision: expected_revision,
                generation: expected_generation,
                diagnostics,
                diff,
            });
        }

        match transaction.commit(view, graph) {
            Ok(committed_diff) => {
                let diagnostics = view.validate(graph);
                Ok(GraphViewAuthoringMutation {
                    success: !diagnostics.iter().any(Diagnostic::is_blocking),
                    base_revision: expected_revision,
                    base_generation: expected_generation,
                    revision: view.revision(),
                    generation: view.identity(),
                    diagnostics,
                    diff: committed_diff,
                })
            },
            Err(GraphViewTransactionError::ValidationFailed { diagnostics }) => {
                Ok(GraphViewAuthoringMutation {
                    success: false,
                    base_revision: expected_revision,
                    base_generation: expected_generation,
                    revision: expected_revision,
                    generation: expected_generation,
                    diagnostics,
                    diff,
                })
            }
            Err(GraphViewTransactionError::Conflict {
                expected_identity,
                expected_revision: conflict_revision,
                actual_identity,
                actual_revision,
            }) => Err(GraphViewAuthoringError::Stale {
                expected_revision: conflict_revision,
                expected_generation: expected_identity,
                actual_revision,
                actual_generation: actual_identity,
            })
        }
    }
}

fn evaluate(
    graph: &Graph,
    view: &GraphView,
    base_revision: u64,
    base_generation: u64,
    commands: Vec<GraphViewCommand>,
) -> Result<GraphViewAuthoringMutation, GraphViewAuthoringError> {
    let mut candidate = view.clone_for_transaction();
    let mut transaction = GraphViewTransaction::begin(&candidate);
    for command in commands {
        transaction.apply(command);
    }
    let diff = transaction.preview_diff().to_vec();
    if diff.is_empty() {
        let diagnostics = candidate.validate(graph);
        let success = !diagnostics.iter().any(Diagnostic::is_blocking);
        return Ok(GraphViewAuthoringMutation {
            success,
            base_revision,
            base_generation,
            revision: base_revision,
            generation: base_generation,
            diagnostics,
            diff,
        });
    }
    match transaction.commit(&mut candidate, graph) {
        Ok(_) => {
            let diagnostics = candidate.validate(graph);
            Ok(GraphViewAuthoringMutation {
                success: !diagnostics.iter().any(Diagnostic::is_blocking),
                base_revision,
                base_generation,
                revision: base_revision,
                generation: base_generation,
                diagnostics,
                diff,
            })
        },
        Err(GraphViewTransactionError::ValidationFailed { diagnostics }) => {
            Ok(GraphViewAuthoringMutation {
                success: false,
                base_revision,
                base_generation,
                revision: base_revision,
                generation: base_generation,
                diagnostics,
                diff,
            })
        }
        Err(GraphViewTransactionError::Conflict {
            expected_identity,
            expected_revision,
            actual_identity,
            actual_revision,
        }) => Err(GraphViewAuthoringError::Stale {
            expected_revision,
            expected_generation: expected_identity,
            actual_revision,
            actual_generation: actual_identity,
        })
    }
}

fn ensure_current(
    view: &GraphView,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<(), GraphViewAuthoringError> {
    let actual_revision = view.revision();
    let actual_generation = view.identity();
    if actual_revision == expected_revision && actual_generation == expected_generation {
        return Ok(());
    }
    Err(GraphViewAuthoringError::Stale {
        expected_revision,
        expected_generation,
        actual_revision,
        actual_generation,
    })
}
