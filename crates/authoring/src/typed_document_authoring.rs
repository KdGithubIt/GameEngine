//! Shared transactional authoring for persisted typed documents (ADR 0121).
//!
//! Material, Project Settings, and Animation Set editors use this boundary so
//! GUI, CLI, and MCP share permission checks, validation, stale-base handling,
//! deterministic preview diffs, and atomic replacement semantics. Persistence
//! remains an adapter concern and persisted document schemas are unchanged.

use crate::project_settings::SYSTEM_SETTINGS_SCHEMA_VERSION;
use crate::{
    AnimationSet, AuthoringPermission, AuthoringPermissionError, AuthoringPermissions, Diagnostic,
    MaterialAsset, ProjectSettings, ANIMATION_SET_SCHEMA_VERSION, PROJECT_SETTINGS_SCHEMA_VERSION,
};
use serde::Serialize;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Typed document contract consumed by the shared transactional service.
pub trait TypedAuthoringDocument: Clone + PartialEq + Serialize {
    /// Stable diagnostic code used when document-level validation fails.
    const INVALID_CODE: &'static str;

    /// Validates the persisted semantic contract without performing I/O.
    fn validate_authoring(&self) -> Result<(), String>;
}

impl TypedAuthoringDocument for MaterialAsset {
    const INVALID_CODE: &'static str = "material.invalid";

    fn validate_authoring(&self) -> Result<(), String> {
        self.validate().map_err(|error| error.to_string())
    }
}

impl TypedAuthoringDocument for ProjectSettings {
    const INVALID_CODE: &'static str = "project_settings.invalid";

    fn validate_authoring(&self) -> Result<(), String> {
        if self.schema_version != PROJECT_SETTINGS_SCHEMA_VERSION {
            return Err(format!(
                "project settings schema version {} is not supported (expected {})",
                self.schema_version, PROJECT_SETTINGS_SCHEMA_VERSION
            ));
        }
        if self.system_settings.schema_version != SYSTEM_SETTINGS_SCHEMA_VERSION {
            return Err(format!(
                "system settings schema version {} is not supported (expected {})",
                self.system_settings.schema_version, SYSTEM_SETTINGS_SCHEMA_VERSION
            ));
        }
        if let Some(layer) = self.layers.iter().find(|layer| layer.index > 31) {
            return Err(format!(
                "layer index {} exceeds the maximum of 31",
                layer.index
            ));
        }
        Ok(())
    }
}

impl TypedAuthoringDocument for AnimationSet {
    const INVALID_CODE: &'static str = "animation_set.invalid";

    fn validate_authoring(&self) -> Result<(), String> {
        if self.schema_version != ANIMATION_SET_SCHEMA_VERSION {
            return Err(format!(
                "animation set schema version {} is not supported (expected {})",
                self.schema_version, ANIMATION_SET_SCHEMA_VERSION
            ));
        }
        self.validate().map_err(|error| error.to_string())
    }
}

/// Live revision metadata owned by the adapter that owns a typed document.
///
/// Reopening a document constructs a fresh generation. Successful semantic
/// replacements advance only the revision, matching the Scene/UI stale-base
/// contract without persisting editor-local counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypedDocumentAuthoringState {
    generation: u64,
    revision: u64,
}

impl TypedDocumentAuthoringState {
    /// Starts authoring metadata for one newly opened document.
    pub fn new() -> Self {
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
        Self {
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            revision: 0,
        }
    }

    /// Current logical revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Current in-memory generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    fn advance(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}

impl Default for TypedDocumentAuthoringState {
    fn default() -> Self {
        Self::new()
    }
}

/// Immutable typed-document state returned by inspect.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypedDocumentAuthoringSnapshot<T> {
    /// Logical content revision.
    pub revision: u64,
    /// In-memory generation that changes when the document is reopened.
    pub generation: u64,
    /// Complete committed typed document.
    pub document: T,
}

/// Structured typed-document validation result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypedDocumentAuthoringValidation {
    /// Logical revision that was validated.
    pub revision: u64,
    /// In-memory generation that was validated.
    pub generation: u64,
    /// Whether validation produced no blocking diagnostic.
    pub success: bool,
    /// Shared semantic diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// Deterministic whole-document semantic replacement.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypedDocumentChange<T> {
    /// Committed value before the replacement.
    pub before: T,
    /// Proposed or committed value after the replacement.
    pub after: T,
}

/// Result of previewing or applying one typed-document replacement.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypedDocumentAuthoringMutation<T> {
    /// Whether the replacement passed shared validation.
    pub success: bool,
    /// Caller-observed base revision.
    pub base_revision: u64,
    /// Caller-observed base generation.
    pub base_generation: u64,
    /// Current revision after the operation.
    pub revision: u64,
    /// Current generation after the operation.
    pub generation: u64,
    /// Shared semantic diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Empty for a no-op or rejected replacement, otherwise one replacement.
    pub diff: Vec<TypedDocumentChange<T>>,
}

/// Shared typed-document authoring failure.
#[derive(Debug)]
pub enum TypedDocumentAuthoringError {
    /// The host did not grant the required authoring permission.
    Permission(AuthoringPermissionError),
    /// The caller's revision/generation pair no longer names live state.
    Stale {
        /// Caller-observed revision.
        expected_revision: u64,
        /// Caller-observed generation.
        expected_generation: u64,
        /// Current revision.
        actual_revision: u64,
        /// Current generation.
        actual_generation: u64,
    },
}

impl TypedDocumentAuthoringError {
    /// Stable diagnostic-style code exposed by structured adapters.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Permission(error) => error.code(),
            Self::Stale { .. } => "authoring.stale_revision",
        }
    }
}

impl fmt::Display for TypedDocumentAuthoringError {
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
                "stale typed-document base: expected revision {expected_revision} generation {expected_generation}, current revision {actual_revision} generation {actual_generation}"
            ),
        }
    }
}

impl std::error::Error for TypedDocumentAuthoringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Permission(error) => Some(error),
            Self::Stale { .. } => None,
        }
    }
}

impl From<AuthoringPermissionError> for TypedDocumentAuthoringError {
    fn from(value: AuthoringPermissionError) -> Self {
        Self::Permission(value)
    }
}

/// GUI-free transactional service shared by typed-document adapters.
#[derive(Debug, Default, Clone, Copy)]
pub struct TypedDocumentAuthoringService;

impl TypedDocumentAuthoringService {
    /// Creates the stateless service.
    pub fn new() -> Self {
        Self
    }

    /// Inspects one live typed document.
    pub fn inspect<T: TypedAuthoringDocument>(
        &self,
        document: &T,
        state: &TypedDocumentAuthoringState,
        permissions: &AuthoringPermissions,
    ) -> Result<TypedDocumentAuthoringSnapshot<T>, TypedDocumentAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        Ok(TypedDocumentAuthoringSnapshot {
            revision: state.revision,
            generation: state.generation,
            document: document.clone(),
        })
    }

    /// Validates one live typed document without mutation.
    pub fn validate<T: TypedAuthoringDocument>(
        &self,
        document: &T,
        state: &TypedDocumentAuthoringState,
        permissions: &AuthoringPermissions,
    ) -> Result<TypedDocumentAuthoringValidation, TypedDocumentAuthoringError> {
        permissions.require(AuthoringPermission::Read)?;
        let diagnostics = validation_diagnostics(document);
        Ok(TypedDocumentAuthoringValidation {
            revision: state.revision,
            generation: state.generation,
            success: diagnostics.is_empty(),
            diagnostics,
        })
    }

    /// Previews an atomic whole-document replacement without changing live state.
    pub fn preview<T: TypedAuthoringDocument>(
        &self,
        document: &T,
        state: &TypedDocumentAuthoringState,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        replacement: T,
    ) -> Result<TypedDocumentAuthoringMutation<T>, TypedDocumentAuthoringError> {
        permissions.require(AuthoringPermission::Preview)?;
        ensure_current(state, expected_revision, expected_generation)?;
        Ok(evaluate(document, state, expected_revision, expected_generation, replacement, false))
    }

    /// Applies one validated whole-document replacement atomically.
    pub fn apply<T: TypedAuthoringDocument>(
        &self,
        document: &mut T,
        state: &mut TypedDocumentAuthoringState,
        permissions: &AuthoringPermissions,
        expected_revision: u64,
        expected_generation: u64,
        replacement: T,
    ) -> Result<TypedDocumentAuthoringMutation<T>, TypedDocumentAuthoringError> {
        permissions.require(AuthoringPermission::ProjectDataWrite)?;
        ensure_current(state, expected_revision, expected_generation)?;
        let diagnostics = validation_diagnostics(&replacement);
        if !diagnostics.is_empty() {
            return Ok(TypedDocumentAuthoringMutation {
                success: false,
                base_revision: expected_revision,
                base_generation: expected_generation,
                revision: state.revision,
                generation: state.generation,
                diagnostics,
                diff: Vec::new(),
            });
        }
        let diff = replacement_diff(document, &replacement);
        if !diff.is_empty() {
            *document = replacement;
            state.advance();
        }
        Ok(TypedDocumentAuthoringMutation {
            success: true,
            base_revision: expected_revision,
            base_generation: expected_generation,
            revision: state.revision,
            generation: state.generation,
            diagnostics,
            diff,
        })
    }
}

fn evaluate<T: TypedAuthoringDocument>(
    document: &T,
    state: &TypedDocumentAuthoringState,
    expected_revision: u64,
    expected_generation: u64,
    replacement: T,
    committed: bool,
) -> TypedDocumentAuthoringMutation<T> {
    let diagnostics = validation_diagnostics(&replacement);
    if !diagnostics.is_empty() {
        return TypedDocumentAuthoringMutation {
            success: false,
            base_revision: expected_revision,
            base_generation: expected_generation,
            revision: state.revision,
            generation: state.generation,
            diagnostics,
            diff: Vec::new(),
        };
    }
    let diff = replacement_diff(document, &replacement);
    TypedDocumentAuthoringMutation {
        success: true,
        base_revision: expected_revision,
        base_generation: expected_generation,
        revision: if committed && !diff.is_empty() {
            state.revision.saturating_add(1)
        } else {
            state.revision
        },
        generation: state.generation,
        diagnostics,
        diff,
    }
}

fn replacement_diff<T: TypedAuthoringDocument>(before: &T, after: &T) -> Vec<TypedDocumentChange<T>> {
    if before == after {
        Vec::new()
    } else {
        vec![TypedDocumentChange {
            before: before.clone(),
            after: after.clone(),
        }]
    }
}

fn validation_diagnostics<T: TypedAuthoringDocument>(document: &T) -> Vec<Diagnostic> {
    document
        .validate_authoring()
        .err()
        .map(|message| Diagnostic::error(T::INVALID_CODE, message))
        .into_iter()
        .collect()
}

fn ensure_current(
    state: &TypedDocumentAuthoringState,
    expected_revision: u64,
    expected_generation: u64,
) -> Result<(), TypedDocumentAuthoringError> {
    if state.revision == expected_revision && state.generation == expected_generation {
        return Ok(());
    }
    Err(TypedDocumentAuthoringError::Stale {
        expected_revision,
        expected_generation,
        actual_revision: state.revision,
        actual_generation: state.generation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writable() -> AuthoringPermissions {
        AuthoringPermissions::read_only()
            .with(AuthoringPermission::Preview)
            .with(AuthoringPermission::ProjectDataWrite)
    }

    #[test]
    fn material_preview_is_non_destructive_and_apply_advances_revision() {
        let service = TypedDocumentAuthoringService::new();
        let mut document = MaterialAsset::default();
        let mut state = TypedDocumentAuthoringState::new();
        let base = service.inspect(&document, &state, &writable()).expect("inspect");
        let mut replacement = document.clone();
        replacement.roughness = 0.25;
        let preview = service
            .preview(
                &document,
                &state,
                &writable(),
                base.revision,
                base.generation,
                replacement.clone(),
            )
            .expect("preview");
        assert!(preview.success);
        assert_eq!(preview.diff.len(), 1);
        assert_ne!(document, replacement);
        let applied = service
            .apply(
                &mut document,
                &mut state,
                &writable(),
                base.revision,
                base.generation,
                replacement.clone(),
            )
            .expect("apply");
        assert!(applied.success);
        assert_eq!(document, replacement);
        assert_eq!(applied.revision, base.revision + 1);
    }

    #[test]
    fn stale_and_invalid_replacements_never_mutate() {
        let service = TypedDocumentAuthoringService::new();
        let mut document = MaterialAsset::default();
        let original = document.clone();
        let mut state = TypedDocumentAuthoringState::new();
        let stale_generation = state.generation();
        let stale = service.apply(
            &mut document,
            &mut state,
            &writable(),
            99,
            stale_generation,
            original.clone(),
        );
        assert!(matches!(stale, Err(TypedDocumentAuthoringError::Stale { .. })));
        let mut invalid = original.clone();
        invalid.roughness = f32::NAN;
        let revision = state.revision();
        let generation = state.generation();
        let result = service
            .apply(
                &mut document,
                &mut state,
                &writable(),
                revision,
                generation,
                invalid,
            )
            .expect("validation result");
        assert!(!result.success);
        assert_eq!(document, original);
    }

    #[test]
    fn all_first_release_typed_documents_share_validation_boundary() {
        let service = TypedDocumentAuthoringService::new();
        let permissions = AuthoringPermissions::read_only();
        let state = TypedDocumentAuthoringState::new();
        assert!(service.validate(&MaterialAsset::default(), &state, &permissions).unwrap().success);
        assert!(service.validate(&ProjectSettings::default(), &state, &permissions).unwrap().success);
        assert!(service.validate(&AnimationSet::empty(), &state, &permissions).unwrap().success);
    }
}
