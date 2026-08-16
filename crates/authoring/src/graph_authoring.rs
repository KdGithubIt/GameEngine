//! Shared domain-neutral Graph query and mutation service.
//!
//! The service is intentionally transport- and GUI-free. It centralizes
//! permissions, stale-base checks, domain-aware validation, semantic preview
//! diffs, and atomic Graph command application while leaving GraphView
//! presentation transactions separate as required by ADR 0016.

use super::{Graph, GraphChange, GraphCommand};
use crate::access::{
    AuthoringPermission, AuthoringPermissionError, AuthoringPermissions,
};
use crate::diagnostic::Diagnostic;
use crate::graph_domain::{
    apply_graph_commands_with_domain, validate_graph_with_domain, GraphCommandApplication,
    GraphDomain,
};
use serde::Serialize;
use std::fmt;

/// Immutable semantic Graph state returned by structured authoring inspection.
#[derive(Debug, Serialize)]
pub struct GraphAuthoringSnapshot {
    /// Logical content revision of the committed Graph.
    pub revision: u64,
    /// In-memory generation used to reject stale edits across reload or undo.
    pub generation: u64,
    /// Complete semantic Graph document at this revision.
    pub graph: Graph,
}

/// Structured validation result for one committed semantic Graph.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphAuthoringValidation {
    /// Logical content revision validated by this result.
    pub revision: u64,
    /// In-memory generation validated by this result.
    pub generation: u64,
    /// Whether structural and selected-domain validation produced no blocking diagnostic.
    pub success: bool,
    /// Structured structural and domain diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Result of previewing or applying one semantic Graph command batch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphAuthoringMutation {
    /// Whether the complete command batch passed structural and domain validation.
    pub success: bool,
    /// Revision supplied by the caller as its base.
    pub base_revision: u64,
    /// Generation supplied by the caller as its base.
    pub base_generation: u64,
    /// Current committed revision after this operation.
    pub revision: u64,
    /// Current committed generation after this operation.
    pub generation: u64,
    /// Structured command, structural, and domain diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Deterministic semantic diff proposed or committed by the command batch.
    pub diff: Vec<GraphChange>,
}

/// Shared semantic Graph authoring service failure.
#[derive(Debug)]
pub enum GraphAuthoringError {
    /// The application did not grant the permission required by this operation.
    Permission(AuthoringPermissionError),
    /// The supplied mutation base no longer matches the live Graph.
    Stale {
        /// Revision supplied by the caller.
        expected_revision: u64,
        /// Generation supplied by the caller.
        expected_generation: u64,
        /// Current committed Graph revision.
        actual_revision: u64,
        /// Current committed Graph generation.
        actual_generation: u64,
    },
}

impl GraphAuthoringError {
    /// Returns a stable diagnostic-style error code for adapter responses.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Permission(error) => error.code(),
            Self::Stale { .. } => "authoring.stale_revision",
        }
    }
}

impl fmt::Display for GraphAuthoringError {
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
                "stale Graph base: expected revision {expected_revision} generation {expected_generation}, current revision {actual_revision} generation {actual_generation}"
            ),
        }
    }
}

impl std::error::Error for GraphAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(error) => Some(error),
            Self::Stale { .. } => None,
        }
    }
}

impl From<AuthoringPermissionError> for GraphAuthoringError {
    fn from(value: AuthoringPermissionError) -> Self {
        Self::Permission(value)
    }
}

/// GUI-free semantic Graph authoring behavior shared by structured adapters.
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphAuthoringService;

impl GraphAuthoringService {
    /// Creates the stateless Graph authoring service.
    pub fn new() -> Self {
        Self
    }

    /// Inspects the current committed semantic Graph.
    ///
    /// # Errors
    ///
    /// Returns [`GraphAuthoringError`] when read permission is absent.
    pub fn inspect(
        &self,
        graph: &Graph,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphAuthoringSnapshot, GraphAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        Ok(GraphAuthoringSnapshot {
            revision: graph.revision(),
            generation: graph.identity(),
            graph: graph.clone_for_transaction(),
        })
    }

    /// Validates the current Graph through foundation and selected-domain rules.
    ///
    /// GraphView presentation validity is deliberately not part of this result.
    ///
    /// # Errors
    ///
    /// Returns [`GraphAuthoringError`] when read permission is absent.
    pub fn validate(
        &self,
        graph: &Graph,
        domain: &dyn GraphDomain,
        permissions: &AuthoringPermissions,
    ) -> Result<GraphAuthoringValidation, GraphAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        let diagnostics = validate_graph_with_domain(graph, domain);
        let success = !diagnostics.iter().any(Diagnostic::is_blocking);
        Ok(GraphAuthoringValidation {
            revision: graph.revision(),
            generation: graph.identity(),
            success,
            diagnostics,
        })
    }

    /// Previews one atomic semantic Graph command batch without mutation.
    ///
    /// The selected domain participates in validation before the batch is
    /// reported successful. GraphView presentation state is never modified.
    ///
    /// # Errors
    ///
    /// Returns [`GraphAuthoringError`] when preview permission is absent or the
    /// supplied revision/generation pair is stale.
    pub fn preview(
        &self,
        graph: &Graph,
        domain: &dyn GraphDomain,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<GraphCommand>,
    ) -> Result<GraphAuthoringMutation, GraphAuthoringError> {
        permissions.require(AuthoringPermission::Preview)?;
        ensure_current(graph, expected_revision, expected_generation)?;
        Ok(mutation_from_application(
            apply_graph_commands_with_domain(graph, domain, commands),
            expected_revision,
            expected_generation,
        ))
    }

    /// Applies one atomic semantic Graph command batch.
    ///
    /// Domain or structural validation failure leaves the live Graph unchanged.
    /// An empty successful batch is a no-op and does not advance the revision.
    /// GraphView presentation state remains a separate transaction boundary.
    ///
    /// # Errors
    ///
    /// Returns [`GraphAuthoringError`] when project-data-write permission is
    /// absent or the supplied revision/generation pair is stale.
    pub fn apply(
        &self,
        graph: &mut Graph,
        domain: &dyn GraphDomain,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<GraphCommand>,
    ) -> Result<GraphAuthoringMutation, GraphAuthoringError> {
        permissions.require(AuthoringPermission::ProjectDataWrite)?;
        ensure_current(graph, expected_revision, expected_generation)?;

        let application = apply_graph_commands_with_domain(graph, domain, commands);
        match application {
            GraphCommandApplication::Applied {
                diagnostics,
                diff,
                graph: candidate,
            } => {
                if diff.is_empty() {
                    return Ok(GraphAuthoringMutation {
                        success: true,
                        base_revision: expected_revision,
                        base_generation: expected_generation,
                        revision: expected_revision,
                        generation: expected_generation,
                        diagnostics,
                        diff,
                    });
                }
                *graph = *candidate;
                Ok(GraphAuthoringMutation {
                    success: true,
                    base_revision: expected_revision,
                    base_generation: expected_generation,
                    revision: graph.revision(),
                    generation: graph.identity(),
                    diagnostics,
                    diff,
                })
            }
            GraphCommandApplication::Rejected { diagnostics, diff } => Ok(GraphAuthoringMutation {
                success: false,
                base_revision: expected_revision,
                base_generation: expected_generation,
                revision: expected_revision,
                generation: expected_generation,
                diagnostics,
                diff,
            }),
        }
    }
}

fn mutation_from_application(
    application: GraphCommandApplication,
    base_revision: u64,
    base_generation: u64,
) -> GraphAuthoringMutation {
    match application {
        GraphCommandApplication::Applied {
            diagnostics, diff, ..
        } => GraphAuthoringMutation {
            success: true,
            base_revision,
            base_generation,
            revision: base_revision,
            generation: base_generation,
            diagnostics,
            diff,
        },
        GraphCommandApplication::Rejected { diagnostics, diff } => GraphAuthoringMutation {
            success: false,
            base_revision,
            base_generation,
            revision: base_revision,
            generation: base_generation,
            diagnostics,
            diff,
        },
    }
}

fn ensure_current(
    graph: &Graph,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<(), GraphAuthoringError> {
    let actual_revision = graph.revision();
    let actual_generation = graph.identity();
    if actual_revision == expected_revision && actual_generation == expected_generation {
        return Ok(());
    }
    Err(GraphAuthoringError::Stale {
        expected_revision,
        expected_generation,
        actual_revision,
        actual_generation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_domain::TestGraphDomain;
    use crate::{EdgeId, GraphId, NodeId};

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    fn valid_graph(domain: &TestGraphDomain) -> Graph {
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "shared_graph",
        );
        let root = NodeId::generate();
        let action = NodeId::generate();
        graph
            .nodes
            .insert(root.clone(), domain.root_node(root.clone()));
        graph
            .nodes
            .insert(action.clone(), domain.action_node(action.clone(), "act"));
        let edge = EdgeId::generate();
        graph.edges.insert(
            edge.clone(),
            domain.root_to_action_edge(edge, root, action),
        );
        graph
    }

    #[test]
    fn preview_is_non_destructive_and_apply_advances_one_revision() {
        let domain = TestGraphDomain::new();
        let service = GraphAuthoringService::new();
        let mut graph = valid_graph(&domain);
        let base = service.inspect(&graph, &writable()).expect("inspect");
        let node = NodeId::generate();
        let command = GraphCommand::AddNode {
            node: domain.number_source_node(node.clone()),
        };

        let preview = service
            .preview(
                &graph,
                &domain,
                &writable(),
                base.revision,
                base.generation,
                vec![command.clone()],
            )
            .expect("preview");
        assert!(preview.success);
        assert_eq!(preview.diff.len(), 1);
        assert!(!graph.nodes.contains_key(&node));
        assert_eq!(preview.revision, base.revision);

        let applied = service
            .apply(
                &mut graph,
                &domain,
                &writable(),
                base.revision,
                base.generation,
                vec![command],
            )
            .expect("apply");
        assert!(applied.success);
        assert!(graph.nodes.contains_key(&node));
        assert_eq!(applied.revision, base.revision + 1);
        assert_eq!(applied.generation, base.generation);
    }

    #[test]
    fn domain_rejection_leaves_live_graph_unchanged() {
        let domain = TestGraphDomain::new();
        let service = GraphAuthoringService::new();
        let mut graph = valid_graph(&domain);
        let base = service.inspect(&graph, &writable()).expect("inspect");
        let extra_root = NodeId::generate();

        let result = service
            .apply(
                &mut graph,
                &domain,
                &writable(),
                base.revision,
                base.generation,
                vec![GraphCommand::AddNode {
                    node: domain.root_node(extra_root.clone()),
                }],
            )
            .expect("domain rejection is a structured result");

        assert!(!result.success);
        assert!(!graph.nodes.contains_key(&extra_root));
        assert_eq!(graph.revision(), base.revision);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "test_domain.multiple_roots"));
    }

    #[test]
    fn stale_generation_rejects_an_aba_round_trip() {
        let domain = TestGraphDomain::new();
        let service = GraphAuthoringService::new();
        let graph = valid_graph(&domain);
        let base = service.inspect(&graph, &writable()).expect("inspect");
        let json = serde_json::to_string(&graph).expect("serialize graph");
        let mut restored: Graph = serde_json::from_str(&json).expect("restore graph");
        assert_eq!(restored.revision(), base.revision);
        assert_ne!(restored.identity(), base.generation);

        let error = service
            .apply(
                &mut restored,
                &domain,
                &writable(),
                base.revision,
                base.generation,
                Vec::new(),
            )
            .expect_err("restored generation must reject stale base");
        assert_eq!(error.code(), "authoring.stale_revision");
    }

    #[test]
    fn write_requires_shared_project_data_permission() {
        let domain = TestGraphDomain::new();
        let service = GraphAuthoringService::new();
        let mut graph = valid_graph(&domain);
        let permissions = AuthoringPermissions::read_only();
        let base = service.inspect(&graph, &permissions).expect("read is allowed");

        let error = service
            .apply(
                &mut graph,
                &domain,
                &permissions,
                base.revision,
                base.generation,
                Vec::new(),
            )
            .expect_err("read-only access must reject apply");
        assert_eq!(error.code(), "authoring.permission_denied");
    }
}
