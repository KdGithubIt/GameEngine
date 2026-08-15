//! Error types returned by editor session operations.

use engine_authoring::{
    Diagnostic, EntityId, GraphSaveError, GraphTransactionError, GraphTransactionValidationError,
    GraphViewSaveError, GraphViewTransactionError, PersistError, SceneSaveError, TransactionError,
    UiDocumentEditError,
};
use std::{fmt, io, path::PathBuf};

/// Editor session operation error.
#[derive(Debug)]
pub enum EditorSessionError {
    /// An operation required an open scene document.
    NoSceneDocument,
    /// An operation required an open declarative UI document.
    NoUiDocument,
    /// An operation was requested for a different concrete graph domain.
    WrongGraphDomain {
        /// Short description of the rejected operation.
        operation: &'static str,
    },
    /// A requested node-to-node connection is not valid for the active domain.
    InvalidGraphConnection(String),
    /// An Animation Graph transition edit contains invalid data or endpoints.
    InvalidAnimationTransition(String),
    /// A batch transform operation targeted an entity without a transform.
    MissingTransform(EntityId),
    /// An authored transform did not contain finite numeric position fields.
    InvalidTransform(EntityId),
    /// A UI authoring command was structurally invalid.
    UiEdit(UiDocumentEditError),
    /// Whole-document validation blocked a UI transaction commit.
    UiValidation {
        /// Stable user-facing summary retained after the transaction closes.
        message: String,
    },
    /// A scene transaction failed.
    SceneTransaction {
        /// Source transaction error.
        source: TransactionError,
    },
    /// A semantic graph transaction failed.
    GraphTransaction {
        /// Source transaction error.
        source: GraphTransactionError,
    },
    /// A semantic graph private transaction failed validation.
    GraphTransactionValidation {
        /// Source transaction validation error.
        source: GraphTransactionValidationError,
    },
    /// A graph view transaction failed.
    GraphViewTransaction {
        /// Source graph view transaction error.
        source: GraphViewTransactionError,
    },
    /// Domain diagnostics blocked an operation that requires a valid graph.
    Diagnostics,
}

impl fmt::Display for EditorSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSceneDocument => write!(formatter, "no scene document is open"),
            Self::NoUiDocument => write!(formatter, "no UI document is open"),
            Self::WrongGraphDomain { operation } => {
                write!(formatter, "cannot {operation} in the active graph domain")
            }
            Self::InvalidGraphConnection(message) => formatter.write_str(message),
            Self::InvalidAnimationTransition(message) => formatter.write_str(message),
            Self::MissingTransform(entity) => {
                write!(
                    formatter,
                    "entity `{entity}` has no engine.transform component"
                )
            }
            Self::InvalidTransform(entity) => {
                write!(
                    formatter,
                    "entity `{entity}` has an invalid transform position"
                )
            }
            Self::UiEdit(source) => write!(formatter, "{source}"),
            Self::UiValidation { message } => write!(formatter, "{message}"),
            Self::SceneTransaction { source } => write!(formatter, "{source}"),
            Self::GraphTransaction { source } => write!(formatter, "{source}"),
            Self::GraphTransactionValidation { source } => write!(formatter, "{source}"),
            Self::GraphViewTransaction { source } => write!(formatter, "{source}"),
            Self::Diagnostics => write!(formatter, "operation blocked by diagnostics"),
        }
    }
}

impl std::error::Error for EditorSessionError {}

/// Error returned by [`EditorSession::save`] and [`EditorSession::save_as`].
///
/// [`EditorSession::save`]: super::EditorSession::save
/// [`EditorSession::save_as`]: super::EditorSession::save_as

#[derive(Debug)]
pub enum EditorPersistError {
    /// No document path is currently associated with the session.
    NoDocument,
    /// Graph save requires a `.graph.json` path.
    InvalidGraphPath {
        /// Path rejected by the graph save pipeline.
        path: PathBuf,
    },
    /// A graph view exists but the document has no view path.
    MissingGraphViewPath,
    /// Scene serialization or validation failed.
    SceneSave(SceneSaveError),
    /// Semantic graph serialization or validation failed.
    GraphSave(GraphSaveError),
    /// Graph view serialization or validation failed.
    GraphViewSave(GraphViewSaveError),
    /// UI document serialization failed.
    UiSerialize(serde_json::Error),
    /// UI validation produced blocking diagnostics.
    InvalidUiDocument {
        /// Diagnostics that must be resolved before saving.
        diagnostics: Vec<Diagnostic>,
    },
    /// File write failed.
    Persist(PersistError),
}

impl fmt::Display for EditorPersistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDocument => write!(formatter, "no document path is available; use Save As"),
            Self::InvalidGraphPath { path } => write!(
                formatter,
                "graph documents must be saved as .graph.json files: {}",
                path.display()
            ),
            Self::MissingGraphViewPath => {
                write!(
                    formatter,
                    "graph view exists but no .graph.view.json path is set"
                )
            }
            Self::SceneSave(source) => write!(formatter, "{source}"),
            Self::GraphSave(source) => write!(formatter, "{source}"),
            Self::GraphViewSave(source) => write!(formatter, "{source}"),
            Self::UiSerialize(source) => write!(formatter, "UI serialization failed: {source}"),
            Self::InvalidUiDocument { diagnostics } => write!(
                formatter,
                "UI document has {} blocking diagnostic(s)",
                diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.severity == engine_authoring::Severity::Error)
                    .count()
            ),
            Self::Persist(source) => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for EditorPersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SceneSave(source) => Some(source),
            Self::GraphSave(source) => Some(source),
            Self::GraphViewSave(source) => Some(source),
            Self::UiSerialize(source) => Some(source),
            Self::Persist(source) => Some(source),
            Self::NoDocument
            | Self::InvalidGraphPath { .. }
            | Self::MissingGraphViewPath
            | Self::InvalidUiDocument { .. } => None,
        }
    }
}

/// Error returned by [`EditorSession::load_from_path`].
///
/// [`EditorSession::load_from_path`]: super::EditorSession::load_from_path

#[derive(Debug)]
pub enum EditorLoadError {
    /// File read failed.
    Io(io::Error),
    /// JSON parse or deserialization failed.
    Json(serde_json::Error),
    /// A required top-level field is missing or has the wrong type.
    MissingField(&'static str),
    /// The file was written by a newer or incompatible editor version.
    UnsupportedVersion(u64),
}

impl fmt::Display for EditorLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "file read failed: {source}"),
            Self::Json(source) => write!(formatter, "JSON error: {source}"),
            Self::MissingField(name) => write!(formatter, "missing field: {name}"),
            Self::UnsupportedVersion(v) => {
                write!(formatter, "unsupported format_version {v}; expected 1")
            }
        }
    }
}

impl std::error::Error for EditorLoadError {}
