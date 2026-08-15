//! Structured diagnostic messages for the authoring pipeline.
//!
//! [`Diagnostic`] carries a stable `code` that tools and tests can match
//! against without depending on human-readable message text.
//!
//! See `AI_FRIENDLY_AUTHORING_SPEC.md` §14.

use crate::id::{AssetId, ComponentTypeId, EdgeId, EntityId, GraphId, GroupId, NodeId, PortId};
use serde::{Deserialize, Serialize};

/// The importance of a [`Diagnostic`] message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// An informational message that does not indicate a problem.
    Info,
    /// A potential problem that does not block the operation.
    Warning,
    /// A problem that blocks the operation or makes the data invalid.
    Error,
}

/// The authoring object associated with a [`Diagnostic`].
///
/// Targets allow tools to navigate directly to the affected object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticTarget {
    /// A project-relative source file and optional one-based line.
    SourceFile {
        /// Path relative to the owning source root, such as `game/src`.
        path: String,
        /// One-based source line when the producer supplied one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        line: Option<usize>,
    },
    /// An authoring entity.
    Entity {
        /// The stable identifier of the affected entity.
        id: EntityId,
    },
    /// An authoring asset.
    Asset {
        /// The stable identifier of the affected asset.
        id: AssetId,
    },
    /// A specific component on an entity.
    Component {
        /// The entity carrying the component.
        entity: EntityId,
        /// The type of the affected component.
        component_type: ComponentTypeId,
    },
    /// A graph.
    Graph {
        /// The stable identifier of the affected graph.
        id: GraphId,
    },
    /// A node within a graph.
    Node {
        /// The graph that contains the affected node.
        graph: GraphId,
        /// The stable identifier of the affected node.
        node: NodeId,
    },
    /// An edge within a graph.
    Edge {
        /// The graph that contains the affected edge.
        graph: GraphId,
        /// The stable identifier of the affected edge.
        edge: EdgeId,
    },
    /// A port within a graph node.
    Port {
        /// The graph that contains the affected node.
        graph: GraphId,
        /// The node that owns the affected port.
        node: NodeId,
        /// The stable identifier of the affected port.
        port: PortId,
    },
    /// A group within a graph.
    Group {
        /// The graph that contains the affected group.
        graph: GraphId,
        /// The stable identifier of the affected group.
        group: GroupId,
    },
}

/// A structured diagnostic message produced by validation or commands.
///
/// The `code` field is stable and suitable for use in tests and tool logic.
/// The `message` field is human-readable and may improve without breaking
/// compatibility.
///
/// # Examples
///
/// ```
/// use engine_authoring::diagnostic::{Diagnostic, Severity};
///
/// let d = Diagnostic::error("schema.unknown_component_type", "Unknown component type");
/// assert_eq!(d.severity, Severity::Error);
/// assert_eq!(d.code, "schema.unknown_component_type");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// The severity of this diagnostic.
    pub severity: Severity,

    /// A stable dot-separated code identifying the diagnostic kind.
    ///
    /// Codes MUST remain stable across releases so that tests and tools can
    /// depend on them. Message text may be improved without affecting the code.
    pub code: String,

    /// A human-readable description of the problem or condition.
    pub message: String,

    /// The authoring object that triggered this diagnostic, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<DiagnosticTarget>,

    /// Wall-clock time in milliseconds since the UNIX epoch when this diagnostic was created.
    ///
    /// `None` for diagnostics loaded from JSON that pre-dates this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_ms: Option<u64>,
}

impl Diagnostic {
    /// Creates an error-level diagnostic with the given code and message.
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: code.into(),
            message: message.into(),
            target: None,
            timestamp_ms: None,
        }
    }

    /// Creates a warning-level diagnostic with the given code and message.
    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            code: code.into(),
            message: message.into(),
            target: None,
            timestamp_ms: None,
        }
    }

    /// Creates an info-level diagnostic with the given code and message.
    pub fn info(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            code: code.into(),
            message: message.into(),
            target: None,
            timestamp_ms: None,
        }
    }

    /// Attaches a [`DiagnosticTarget`] to this diagnostic.
    pub fn with_target(mut self, target: DiagnosticTarget) -> Self {
        self.target = Some(target);
        self
    }

    /// Stamps this diagnostic with the current wall-clock time.
    pub fn with_timestamp_now(mut self) -> Self {
        self.timestamp_ms = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        );
        self
    }

    /// Returns `true` when this diagnostic blocks an operation.
    pub fn is_blocking(&self) -> bool {
        self.severity == Severity::Error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{ComponentTypeId, EntityId};

    #[test]
    fn error_diagnostic_is_blocking() {
        let d = Diagnostic::error("test.code", "test message");
        assert!(d.is_blocking());
    }

    #[test]
    fn warning_diagnostic_is_not_blocking() {
        let d = Diagnostic::warning("test.code", "test message");
        assert!(!d.is_blocking());
    }

    #[test]
    fn diagnostic_without_target_omits_target_in_json() {
        let d = Diagnostic::error("test.code", "message");
        let json = serde_json::to_string(&d).unwrap();
        assert!(
            !json.contains("\"target\""),
            "absent target should not appear in json: {json}"
        );
    }

    #[test]
    fn diagnostic_with_entity_target_survives_roundtrip() {
        let entity_id = EntityId::generate();
        let d = Diagnostic::error("entity.missing_component", "Missing required component")
            .with_target(DiagnosticTarget::Entity {
                id: entity_id.clone(),
            });

        let json = serde_json::to_string(&d).unwrap();
        let loaded: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(d, loaded);
    }

    #[test]
    fn diagnostic_with_component_target_survives_roundtrip() {
        let entity_id = EntityId::generate();
        let d = Diagnostic::error("schema.type_mismatch", "Field type mismatch").with_target(
            DiagnosticTarget::Component {
                entity: entity_id,
                component_type: ComponentTypeId::new("gameplay.health"),
            },
        );

        let json = serde_json::to_string(&d).unwrap();
        let loaded: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(d, loaded);
    }

    #[test]
    fn diagnostic_with_port_target_survives_roundtrip() {
        let d = Diagnostic::error("graph.port.invalid", "Invalid port").with_target(
            DiagnosticTarget::Port {
                graph: GraphId::generate(),
                node: NodeId::generate(),
                port: PortId::generate(),
            },
        );

        let json = serde_json::to_string(&d).unwrap();
        let loaded: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(d, loaded);
    }

    #[test]
    fn diagnostic_with_source_file_target_survives_roundtrip() {
        let diagnostic = Diagnostic::error("build.failed", "invalid import").with_target(
            DiagnosticTarget::SourceFile {
                path: "src/systems/mission.rs".to_owned(),
                line: Some(3),
            },
        );

        let json = serde_json::to_string(&diagnostic).unwrap();
        let loaded: Diagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(diagnostic, loaded);
    }

    #[test]
    fn severity_ordering_error_greater_than_warning() {
        assert!(Severity::Error > Severity::Warning);
        assert!(Severity::Warning > Severity::Info);
    }
}
