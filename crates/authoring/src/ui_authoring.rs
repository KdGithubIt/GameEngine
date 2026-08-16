//! Shared declarative UI query and mutation service.
//!
//! The service owns adapter-neutral permissions, stale-base protection,
//! non-destructive preview, validation, and one-batch commit semantics over
//! [`UiDocumentCommand`]. Applications remain responsible for selecting and
//! persisting the live UI document and for their own undo presentation.

use super::UiDocument;
use crate::access::{
    AuthoringPermission, AuthoringPermissionError, AuthoringPermissions,
};
use crate::diagnostic::Diagnostic;
use crate::ui_edit::{
    UiDocumentChange, UiDocumentCommand, UiDocumentCommitError, UiDocumentEditError,
    UiDocumentTransaction,
};
use serde::Serialize;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Live UI authoring state used by structured adapters.
///
/// The in-memory generation is intentionally not serialized. Reopening or
/// restoring a UI document creates a new generation even when its content
/// revision returns to the same value, preventing ABA-style stale edits.
pub struct UiAuthoringSession {
    document: UiDocument,
    identity: u64,
    revision: u64,
}

impl UiAuthoringSession {
    /// Creates a live UI authoring session around one committed document.
    pub fn new(document: UiDocument) -> Self {
        Self {
            document,
            identity: next_ui_identity(),
            revision: 0,
        }
    }

    /// Returns the current committed UI document.
    pub fn document(&self) -> &UiDocument {
        &self.document
    }

    fn commit(&mut self, document: UiDocument) {
        self.document = document;
        self.revision = self.revision.saturating_add(1);
    }
}

/// Immutable UI state returned by structured authoring inspection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiAuthoringSnapshot {
    /// Logical content revision of the committed UI document.
    pub revision: u64,
    /// In-memory generation used to reject stale edits across reload or undo.
    pub generation: u64,
    /// Complete committed declarative UI document.
    pub document: UiDocument,
}

/// Structured validation result for one committed UI document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiAuthoringValidation {
    /// Logical content revision validated by this result.
    pub revision: u64,
    /// In-memory generation validated by this result.
    pub generation: u64,
    /// Whether no blocking UI diagnostic was produced.
    pub success: bool,
    /// Structured whole-document diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Result of previewing or applying one UI command batch.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiAuthoringMutation {
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
    /// Structured command or whole-document diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Deterministic structural UI changes proposed or committed by the batch.
    pub diff: Vec<UiDocumentChange>,
}

/// Shared UI authoring service failure.
#[derive(Debug)]
pub enum UiAuthoringError {
    /// The application did not grant the permission required by this operation.
    Permission(AuthoringPermissionError),
    /// The supplied mutation base no longer matches the live UI document.
    Stale {
        /// Revision supplied by the caller.
        expected_revision: u64,
        /// Generation supplied by the caller.
        expected_generation: u64,
        /// Current committed UI revision.
        actual_revision: u64,
        /// Current committed UI generation.
        actual_generation: u64,
    },
    /// Final transaction commit failed after the same document had validated.
    Commit(UiDocumentCommitError),
}

impl UiAuthoringError {
    /// Returns a stable diagnostic-style error code for adapter responses.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Permission(error) => error.code(),
            Self::Stale { .. } => "authoring.stale_revision",
            Self::Commit(_) => "authoring.validation_failed",
        }
    }
}

impl fmt::Display for UiAuthoringError {
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
                "stale UI base: expected revision {expected_revision} generation {expected_generation}, current revision {actual_revision} generation {actual_generation}"
            ),
            Self::Commit(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for UiAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(error) => Some(error),
            Self::Commit(error) => Some(error),
            Self::Stale { .. } => None,
        }
    }
}

impl From<AuthoringPermissionError> for UiAuthoringError {
    fn from(value: AuthoringPermissionError) -> Self {
        Self::Permission(value)
    }
}

/// GUI-free declarative UI authoring behavior shared by structured adapters.
#[derive(Debug, Default, Clone, Copy)]
pub struct UiAuthoringService;

impl UiAuthoringService {
    /// Creates the stateless UI authoring service.
    pub fn new() -> Self {
        Self
    }

    /// Inspects the current committed UI document.
    ///
    /// # Errors
    ///
    /// Returns [`UiAuthoringError`] when read permission is absent.
    pub fn inspect(
        &self,
        session: &UiAuthoringSession,
        permissions: &AuthoringPermissions,
    ) -> Result<UiAuthoringSnapshot, UiAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        Ok(UiAuthoringSnapshot {
            revision: session.revision,
            generation: session.identity,
            document: session.document.clone(),
        })
    }

    /// Validates the current committed UI document without mutation.
    ///
    /// # Errors
    ///
    /// Returns [`UiAuthoringError`] when read permission is absent.
    pub fn validate(
        &self,
        session: &UiAuthoringSession,
        permissions: &AuthoringPermissions,
    ) -> Result<UiAuthoringValidation, UiAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        let diagnostics = session.document.validate();
        let success = !diagnostics.iter().any(Diagnostic::is_blocking);
        Ok(UiAuthoringValidation {
            revision: session.revision,
            generation: session.identity,
            success,
            diagnostics,
        })
    }

    /// Previews one atomic UI command batch without changing live state.
    ///
    /// # Errors
    ///
    /// Returns [`UiAuthoringError`] when preview permission is absent or the
    /// supplied revision/generation pair is stale.
    pub fn preview(
        &self,
        session: &UiAuthoringSession,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<UiDocumentCommand>,
    ) -> Result<UiAuthoringMutation, UiAuthoringError> {
        permissions.require(AuthoringPermission::Preview)?;
        ensure_current(session, expected_revision, expected_generation)?;
        let evaluated = evaluate(&session.document, commands)?;
        Ok(evaluated.mutation(
            expected_revision,
            expected_generation,
            false,
        ))
    }

    /// Applies one atomic UI command batch to the live session.
    ///
    /// Invalid commands or final document validation failure leave the session
    /// unchanged. An empty successful batch is a no-op and does not advance the
    /// revision.
    ///
    /// # Errors
    ///
    /// Returns [`UiAuthoringError`] when project-data-write permission is
    /// absent, the supplied revision/generation pair is stale, or final commit
    /// unexpectedly fails after validation.
    pub fn apply(
        &self,
        session: &mut UiAuthoringSession,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        commands: Vec<UiDocumentCommand>,
    ) -> Result<UiAuthoringMutation, UiAuthoringError> {
        permissions.require(AuthoringPermission::ProjectDataWrite)?;
        ensure_current(session, expected_revision, expected_generation)?;
        let evaluated = evaluate(&session.document, commands)?;

        match evaluated {
            EvaluatedUiMutation::Accepted {
                diagnostics,
                diff,
                document,
            } => {
                let committed = !diff.is_empty();
                if committed {
                    session.commit(*document);
                }
                Ok(UiAuthoringMutation {
                    success: true,
                    base_revision: expected_revision,
                    base_generation: expected_generation,
                    revision: if committed {
                        session.revision
                    } else {
                        expected_revision
                    },
                    generation: session.identity,
                    diagnostics,
                    diff,
                })
            }
            EvaluatedUiMutation::Rejected { diagnostics, diff } => Ok(UiAuthoringMutation {
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

enum EvaluatedUiMutation {
    Accepted {
        diagnostics: Vec<Diagnostic>,
        diff: Vec<UiDocumentChange>,
        document: Box<UiDocument>,
    },
    Rejected {
        diagnostics: Vec<Diagnostic>,
        diff: Vec<UiDocumentChange>,
    },
}

impl EvaluatedUiMutation {
    fn mutation(
        &self,
        base_revision: u64,
        base_generation: u64,
        committed: bool,
    ) -> UiAuthoringMutation {
        match self {
            Self::Accepted {
                diagnostics, diff, ..
            } => UiAuthoringMutation {
                success: true,
                base_revision,
                base_generation,
                revision: if committed && !diff.is_empty() {
                    base_revision.saturating_add(1)
                } else {
                    base_revision
                },
                generation: base_generation,
                diagnostics: diagnostics.clone(),
                diff: diff.clone(),
            },
            Self::Rejected { diagnostics, diff } => UiAuthoringMutation {
                success: false,
                base_revision,
                base_generation,
                revision: base_revision,
                generation: base_generation,
                diagnostics: diagnostics.clone(),
                diff: diff.clone(),
            },
        }
    }
}

fn evaluate(
    document: &UiDocument,
    commands: Vec<UiDocumentCommand>,
) -> Result<EvaluatedUiMutation, UiAuthoringError> {
    let mut transaction = UiDocumentTransaction::begin(document);
    let mut diff = Vec::new();
    for command in commands {
        match transaction.apply(command) {
            Ok(result) => diff.extend(result.changes),
            Err(error) => {
                return Ok(EvaluatedUiMutation::Rejected {
                    diagnostics: vec![diagnostic_for_edit_error(&error)],
                    diff,
                });
            }
        }
    }

    let diagnostics = transaction.document().validate();
    if diagnostics.iter().any(Diagnostic::is_blocking) {
        return Ok(EvaluatedUiMutation::Rejected { diagnostics, diff });
    }

    let document = transaction.commit().map_err(UiAuthoringError::Commit)?;
    Ok(EvaluatedUiMutation::Accepted {
        diagnostics,
        diff,
        document: Box::new(document),
    })
}

fn diagnostic_for_edit_error(error: &UiDocumentEditError) -> Diagnostic {
    let code = match error {
        UiDocumentEditError::NodeNotFound(_) => "ui.node_not_found",
        UiDocumentEditError::RootMutation => "ui.root_mutation",
        UiDocumentEditError::ParentIsNotContainer(_) => "ui.parent_not_container",
        UiDocumentEditError::DuplicateNodeId(_) => "ui.duplicate_node_id",
        UiDocumentEditError::DuplicateSubtreeNodeId(_) => "ui.duplicate_subtree_node_id",
        UiDocumentEditError::EmptyNodeId => "ui.empty_node_id",
        UiDocumentEditError::TreeCycle { .. } => "ui.tree_cycle",
        UiDocumentEditError::ReplacementIdMismatch { .. } => "ui.replacement_id_mismatch",
    };
    Diagnostic::error(code, error.to_string())
}

fn ensure_current(
    session: &UiAuthoringSession,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<(), UiAuthoringError> {
    if session.revision == expected_revision && session.identity == expected_generation {
        return Ok(());
    }
    Err(UiAuthoringError::Stale {
        expected_revision,
        expected_generation,
        actual_revision: session.revision,
        actual_generation: session.identity,
    })
}

fn next_ui_identity() -> u64 {
    static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);
    NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::UiScalePolicy;

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    #[test]
    fn preview_is_non_destructive_and_apply_advances_one_revision() {
        let service = UiAuthoringService::new();
        let mut session = UiAuthoringSession::new(UiDocument::default());
        let base = service.inspect(&session, &writable()).expect("inspect");
        let command = UiDocumentCommand::RenameNode {
            node: "root".into(),
            new_id: "screen_root".into(),
        };

        let preview = service
            .preview(
                &session,
                &writable(),
                base.revision,
                base.generation,
                vec![command.clone()],
            )
            .expect("preview");
        assert!(preview.success);
        assert_eq!(preview.diff.len(), 1);
        assert_eq!(session.document().root.id, "root");

        let applied = service
            .apply(
                &mut session,
                &writable(),
                base.revision,
                base.generation,
                vec![command],
            )
            .expect("apply");
        assert!(applied.success);
        assert_eq!(session.document().root.id, "screen_root");
        assert_eq!(applied.revision, base.revision + 1);
        assert_eq!(applied.generation, base.generation);
    }

    #[test]
    fn invalid_final_document_is_rejected_atomically() {
        let service = UiAuthoringService::new();
        let mut session = UiAuthoringSession::new(UiDocument::default());
        let base = service.inspect(&session, &writable()).expect("inspect");

        let result = service
            .apply(
                &mut session,
                &writable(),
                base.revision,
                base.generation,
                vec![UiDocumentCommand::SetResponsiveSettings {
                    reference_resolution: [0.0, 1080.0],
                    scale_policy: UiScalePolicy::ConstantPixels,
                    safe_area_padding: [0.0; 4],
                }],
            )
            .expect("validation rejection is a structured result");

        assert!(!result.success);
        assert_eq!(session.document().reference_resolution, [1920.0, 1080.0]);
        assert_eq!(result.revision, base.revision);
        assert!(result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ui.invalid_reference_resolution"));
    }

    #[test]
    fn stale_generation_rejects_a_reopened_document() {
        let service = UiAuthoringService::new();
        let session = UiAuthoringSession::new(UiDocument::default());
        let base = service.inspect(&session, &writable()).expect("inspect");
        let mut reopened = UiAuthoringSession::new(session.document().clone());
        assert_eq!(base.revision, 0);

        let error = service
            .apply(
                &mut reopened,
                &writable(),
                base.revision,
                base.generation,
                Vec::new(),
            )
            .expect_err("new generation must reject stale base");
        assert_eq!(error.code(), "authoring.stale_revision");
    }

    #[test]
    fn command_error_is_structured_and_does_not_mutate() {
        let service = UiAuthoringService::new();
        let mut session = UiAuthoringSession::new(UiDocument::default());
        let base = service.inspect(&session, &writable()).expect("inspect");

        let result = service
            .apply(
                &mut session,
                &writable(),
                base.revision,
                base.generation,
                vec![UiDocumentCommand::RemoveNode {
                    node: "missing".into(),
                }],
            )
            .expect("edit rejection is a structured result");
        assert!(!result.success);
        assert_eq!(session.document().root.id, "root");
        assert_eq!(result.diagnostics[0].code, "ui.node_not_found");
    }

    #[test]
    fn write_requires_shared_project_data_permission() {
        let service = UiAuthoringService::new();
        let permissions = AuthoringPermissions::read_only();
        let mut session = UiAuthoringSession::new(UiDocument::default());
        let base = service.inspect(&session, &permissions).expect("read is allowed");

        let error = service
            .apply(
                &mut session,
                &permissions,
                base.revision,
                base.generation,
                Vec::new(),
            )
            .expect_err("read-only access must reject apply");
        assert_eq!(error.code(), "authoring.permission_denied");
    }
}
