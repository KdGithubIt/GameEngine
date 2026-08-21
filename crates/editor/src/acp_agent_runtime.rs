//! Provider-neutral Agent Client Protocol (ACP) runtime contracts beneath Agent Host.
//!
//! Provider adapters translate ACP wire behavior into these types. They do not
//! own GameEngine permissions, work claims, project mutation, validation,
//! persistence, or completion.

mod transport;

pub(crate) use transport::AcpProcessRuntime;

use crate::agent_host::{AgentCapability, AgentEventKind};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

/// Stable ACP wire version used by the first GameEngine adapters.
pub(crate) const ACP_STABLE_PROTOCOL_VERSION: u16 = 1;
/// Stable benchmark identity for the Editor MCP contract handed to ACP agents.
pub(crate) const ACP_GAMEENGINE_MCP_TOOL_CONTRACT: &str = "gameengine_editor-mcp-http-v1";
/// Stable benchmark identity for run-bound Agent Host permission authority.
pub(crate) const ACP_RUN_BOUND_PERMISSION_PROFILE: &str = "agent_host-run_bound_read_write-v1";

/// Data-driven description of one ACP-capable agent.
///
/// Provider adapters supply launch data and minimum expected ACP behavior; the
/// common transport performs live protocol and capability negotiation.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AcpAgentDescriptor {
    pub(crate) id: String,
    pub(crate) executable: OsString,
    pub(crate) arguments: Vec<OsString>,
    pub(crate) environment: BTreeMap<OsString, OsString>,
    pub(crate) capabilities: AcpCapabilities,
    pub(crate) runtime_identity: AcpRuntimeIdentity,
}

impl fmt::Debug for AcpAgentDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpAgentDescriptor")
            .field("id", &self.id)
            .field("executable", &self.executable)
            .field("argument_count", &self.arguments.len())
            .field("environment_variable_count", &self.environment.len())
            .field("capabilities", &self.capabilities)
            .field("runtime_identity", &self.runtime_identity)
            .finish()
    }
}

/// Negotiated ACP capabilities that affect GameEngine orchestration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct AcpCapabilities {
    pub(crate) session_load: bool,
    pub(crate) session_resume: bool,
    pub(crate) session_list: bool,
    pub(crate) session_close: bool,
    pub(crate) session_config_options: bool,
    pub(crate) mcp_http: bool,
    pub(crate) mcp_sse: bool,
    pub(crate) mcp_over_acp: bool,
    pub(crate) extensions: BTreeSet<String>,
}

/// Agent implementation identity returned by ACP initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpRuntimeIdentity {
    pub(crate) protocol_version: u16,
    pub(crate) agent_name: String,
    pub(crate) agent_version: Option<String>,
}

impl AcpRuntimeIdentity {
    pub(crate) fn stable(agent_name: impl Into<String>, agent_version: Option<String>) -> Self {
        Self {
            protocol_version: ACP_STABLE_PROTOCOL_VERSION,
            agent_name: agent_name.into(),
            agent_version,
        }
    }
}

/// Editor MCP authority that may be handed to an ACP session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcpMcpAccessLevel {
    ReadOnly,
    AgentRunBoundReadWrite,
}

/// Ephemeral Editor MCP connection for one ACP session.
///
/// This type is intentionally not serializable and always redacts its bearer
/// credential from `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AcpMcpConnection {
    endpoint: String,
    authorization_token: String,
    pub(crate) access: AcpMcpAccessLevel,
}

impl AcpMcpConnection {
    pub(crate) fn new(
        endpoint: impl Into<String>,
        authorization_token: impl Into<String>,
        access: AcpMcpAccessLevel,
    ) -> Result<Self, AcpRuntimeError> {
        let endpoint = endpoint.into();
        let authorization_token = authorization_token.into();
        if endpoint.trim().is_empty() || authorization_token.is_empty() {
            return Err(AcpRuntimeError::InvalidSessionBinding(
                "ACP MCP endpoint and credential must not be empty".to_owned(),
            ));
        }
        Ok(Self {
            endpoint,
            authorization_token,
            access,
        })
    }

    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) fn authorization_token(&self) -> &str {
        &self.authorization_token
    }
}

impl fmt::Debug for AcpMcpConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpMcpConnection")
            .field("endpoint", &self.endpoint)
            .field("authorization_token", &"<redacted>")
            .field("access", &self.access)
            .finish()
    }
}

/// Immutable GameEngine authority attached to one ACP session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpSessionBinding {
    pub(crate) gameengine_session_id: String,
    pub(crate) gameengine_run_id: Option<String>,
    pub(crate) mcp: AcpMcpConnection,
}

impl AcpSessionBinding {
    pub(crate) fn read_only(
        gameengine_session_id: impl Into<String>,
        endpoint: impl Into<String>,
        authorization_token: impl Into<String>,
    ) -> Result<Self, AcpRuntimeError> {
        Self::new(
            gameengine_session_id,
            None,
            AcpMcpConnection::new(endpoint, authorization_token, AcpMcpAccessLevel::ReadOnly)?,
        )
    }

    pub(crate) fn run_bound(
        gameengine_session_id: impl Into<String>,
        gameengine_run_id: impl Into<String>,
        endpoint: impl Into<String>,
        authorization_token: impl Into<String>,
    ) -> Result<Self, AcpRuntimeError> {
        Self::new(
            gameengine_session_id,
            Some(gameengine_run_id.into()),
            AcpMcpConnection::new(
                endpoint,
                authorization_token,
                AcpMcpAccessLevel::AgentRunBoundReadWrite,
            )?,
        )
    }

    fn new(
        gameengine_session_id: impl Into<String>,
        gameengine_run_id: Option<String>,
        mcp: AcpMcpConnection,
    ) -> Result<Self, AcpRuntimeError> {
        let gameengine_session_id = gameengine_session_id.into();
        if gameengine_session_id.trim().is_empty() {
            return Err(AcpRuntimeError::InvalidSessionBinding(
                "GameEngine session ID must not be empty".to_owned(),
            ));
        }
        match mcp.access {
            AcpMcpAccessLevel::ReadOnly if gameengine_run_id.is_some() => {
                return Err(AcpRuntimeError::InvalidSessionBinding(
                    "read-only ACP sessions must not carry AgentRun identity".to_owned(),
                ));
            }
            AcpMcpAccessLevel::AgentRunBoundReadWrite
                if gameengine_run_id
                    .as_deref()
                    .is_none_or(|run_id| run_id.trim().is_empty()) =>
            {
                return Err(AcpRuntimeError::InvalidSessionBinding(
                    "run-bound ACP MCP access requires an AgentRun ID".to_owned(),
                ));
            }
            _ => {}
        }
        Ok(Self {
            gameengine_session_id,
            gameengine_run_id,
            mcp,
        })
    }
}

/// Explicit ACP session lifecycle operation selected by Agent Host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpSessionOpenMode {
    New,
    Load { acp_session_id: String },
    Resume { acp_session_id: String },
}

/// Provider-neutral input for opening one ACP session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpSessionOpenRequest {
    pub(crate) binding: AcpSessionBinding,
    pub(crate) working_directory: PathBuf,
    pub(crate) mode: AcpSessionOpenMode,
}

impl AcpSessionOpenRequest {
    pub(crate) fn new(
        binding: AcpSessionBinding,
        working_directory: impl Into<PathBuf>,
    ) -> Result<Self, AcpRuntimeError> {
        Self::build(binding, working_directory.into(), AcpSessionOpenMode::New)
    }

    pub(crate) fn load(
        binding: AcpSessionBinding,
        working_directory: impl Into<PathBuf>,
        acp_session_id: impl Into<String>,
    ) -> Result<Self, AcpRuntimeError> {
        Self::build(
            binding,
            working_directory.into(),
            AcpSessionOpenMode::Load {
                acp_session_id: acp_session_id.into(),
            },
        )
    }

    pub(crate) fn resume(
        binding: AcpSessionBinding,
        working_directory: impl Into<PathBuf>,
        acp_session_id: impl Into<String>,
    ) -> Result<Self, AcpRuntimeError> {
        Self::build(
            binding,
            working_directory.into(),
            AcpSessionOpenMode::Resume {
                acp_session_id: acp_session_id.into(),
            },
        )
    }

    fn build(
        binding: AcpSessionBinding,
        working_directory: PathBuf,
        mode: AcpSessionOpenMode,
    ) -> Result<Self, AcpRuntimeError> {
        let request = Self {
            binding,
            working_directory,
            mode,
        };
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<(), AcpRuntimeError> {
        if !self.working_directory.is_absolute() {
            return Err(AcpRuntimeError::InvalidSessionBinding(
                "ACP session working directory must be absolute".to_owned(),
            ));
        }
        let existing_session_id = match &self.mode {
            AcpSessionOpenMode::New => None,
            AcpSessionOpenMode::Load { acp_session_id }
            | AcpSessionOpenMode::Resume { acp_session_id } => Some(acp_session_id),
        };
        if existing_session_id.is_some_and(|session_id| session_id.trim().is_empty()) {
            return Err(AcpRuntimeError::InvalidSessionBinding(
                "ACP load/resume session ID must not be empty".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Agent-provided ACP permission option kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpPermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Other(String),
}

/// One ACP permission option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpPermissionOption {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: AcpPermissionOptionKind,
}

/// ACP permission request after classification into GameEngine authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpPermissionTarget {
    /// Exact registered GameEngine MCP tool identity recovered from provider
    /// metadata and normalized against the MCP inventory.
    GameEngineMcpTool { stable_name: String, mutating: bool },
    /// Non-MCP operation classified through ACP ToolKind into Agent Host policy.
    AgentCapability(AgentCapability),
    /// No trustworthy stable tool identity/capability could be established.
    Unclassified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpPermissionRequest {
    pub(crate) request_id: String,
    pub(crate) acp_session_id: String,
    pub(crate) tool_call_id: String,
    pub(crate) title: String,
    pub(crate) target: AcpPermissionTarget,
    pub(crate) options: Vec<AcpPermissionOption>,
}

/// Result returned to ACP after Agent Host resolves its own permission policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcpPermissionResolution {
    pub(crate) request_id: String,
    pub(crate) outcome: AcpPermissionOutcome,
}

/// ACP-side permission response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpPermissionOutcome {
    SelectedOption(String),
    Cancelled,
}

/// Normalized ACP tool-call state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcpToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// ACP updates normalized before entering the Agent Host timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpNormalizedEvent {
    AgentMessage {
        text: String,
    },
    Progress {
        step: String,
        detail: String,
    },
    Plan {
        entries: Vec<String>,
    },
    ToolCall {
        tool_call_id: String,
        title: String,
        status: AcpToolCallStatus,
    },
    PermissionRequest(AcpPermissionRequest),
    SessionInfo {
        title: Option<String>,
    },
    TurnFinished {
        stop_reason: String,
    },
    /// The ACP prompt RPC itself failed after the request was accepted.
    ///
    /// This is terminal for the current GameEngine Ask/AgentRun. It is distinct
    /// from a protocol diagnostic, which can describe malformed or unknown
    /// updates without proving that the prompt ended.
    PromptFailed {
        message: String,
    },
    ProtocolDiagnostic {
        message: String,
    },
}

impl AcpNormalizedEvent {
    /// Projects ACP semantics into the existing host event vocabulary.
    ///
    /// A finished ACP turn is progress, never GameEngine completion.
    pub(crate) fn host_event_kind(&self) -> AgentEventKind {
        match self {
            Self::AgentMessage { .. } => AgentEventKind::AssistantMessage,
            Self::Progress { .. }
            | Self::Plan { .. }
            | Self::SessionInfo { .. }
            | Self::TurnFinished { .. } => AgentEventKind::SemanticProgress,
            Self::ToolCall { .. } => AgentEventKind::ToolAction,
            Self::PermissionRequest(_) => AgentEventKind::PermissionRequested,
            Self::PromptFailed { .. } => AgentEventKind::Failure,
            Self::ProtocolDiagnostic { .. } => AgentEventKind::ProviderOutput,
        }
    }
}

/// One live ACP session owned by a registered runtime adapter.
pub(crate) trait AcpAgentSession: Send {
    fn acp_session_id(&self) -> &str;
    fn binding(&self) -> &AcpSessionBinding;
    fn capabilities(&self) -> &AcpCapabilities;
    fn runtime_identity(&self) -> &AcpRuntimeIdentity;
    /// Writes one session configuration value through the formal ACP method.
    ///
    /// Runtimes without session configuration support fail closed instead of
    /// silently translating the value into provider-specific CLI flags.
    fn set_session_config_option(
        &mut self,
        _option_id: &str,
        _value: &str,
    ) -> Result<(), AcpRuntimeError> {
        Err(AcpRuntimeError::Unsupported(format!(
            "ACP session `{}` does not support session configuration writes",
            self.acp_session_id()
        )))
    }
    fn send_prompt(&mut self, prompt: &str) -> Result<(), AcpRuntimeError>;
    /// Polls one event without blocking the Editor thread.
    fn try_next_event(&mut self) -> Result<Option<AcpNormalizedEvent>, AcpRuntimeError>;
    fn resolve_permission(
        &mut self,
        resolution: AcpPermissionResolution,
    ) -> Result<(), AcpRuntimeError>;
    fn cancel(&mut self) -> Result<(), AcpRuntimeError>;
    fn close(&mut self) -> Result<(), AcpRuntimeError>;
}

/// Provider-adapter registration point for the common ACP runtime.
pub(crate) trait AcpAgentRuntime: Send {
    fn descriptor(&self) -> &AcpAgentDescriptor;
    fn open_session(
        &mut self,
        request: AcpSessionOpenRequest,
    ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError>;
}

/// Registry interface consumed by Agent Host.
pub(crate) trait AcpAgentRegistry {
    fn descriptors(&self) -> Vec<&AcpAgentDescriptor>;
    fn runtime_mut(&mut self, id: &str) -> Option<&mut (dyn AcpAgentRuntime + '_)>;
}

/// Data-driven in-process registry for ACP adapters.
#[derive(Default)]
pub(crate) struct AcpRuntimeRegistry {
    runtimes: BTreeMap<String, Box<dyn AcpAgentRuntime>>,
}

impl AcpRuntimeRegistry {
    pub(crate) fn register(
        &mut self,
        runtime: Box<dyn AcpAgentRuntime>,
    ) -> Result<(), AcpRuntimeError> {
        let descriptor = runtime.descriptor();
        validate_descriptor(descriptor)?;
        if self.runtimes.contains_key(&descriptor.id) {
            return Err(AcpRuntimeError::DuplicateAgentId(descriptor.id.clone()));
        }
        self.runtimes.insert(descriptor.id.clone(), runtime);
        Ok(())
    }

    /// Replaces one machine-local adapter registration after validating the new
    /// descriptor. Existing open sessions own their session object and are not
    /// mutated; subsequent sessions use the refreshed runtime configuration.
    pub(crate) fn replace(
        &mut self,
        runtime: Box<dyn AcpAgentRuntime>,
    ) -> Result<(), AcpRuntimeError> {
        validate_descriptor(runtime.descriptor())?;
        let descriptor_id = runtime.descriptor().id.clone();
        if !self.runtimes.contains_key(&descriptor_id) {
            return self.register(runtime);
        }
        self.runtimes.insert(descriptor_id, runtime);
        Ok(())
    }
}

impl AcpAgentRegistry for AcpRuntimeRegistry {
    fn descriptors(&self) -> Vec<&AcpAgentDescriptor> {
        self.runtimes
            .values()
            .map(|runtime| runtime.descriptor())
            .collect()
    }

    fn runtime_mut(&mut self, id: &str) -> Option<&mut (dyn AcpAgentRuntime + '_)> {
        match self.runtimes.get_mut(id) {
            Some(runtime) => Some(runtime.as_mut()),
            None => None,
        }
    }
}

fn validate_descriptor(descriptor: &AcpAgentDescriptor) -> Result<(), AcpRuntimeError> {
    if descriptor.id.trim().is_empty()
        || descriptor.id.trim() != descriptor.id
        || descriptor.id.chars().any(char::is_whitespace)
    {
        return Err(AcpRuntimeError::InvalidDescriptor(
            "descriptor ID must be non-empty and contain no whitespace".to_owned(),
        ));
    }
    if descriptor.executable.is_empty()
        || descriptor.runtime_identity.protocol_version == 0
        || descriptor.runtime_identity.agent_name.trim().is_empty()
    {
        return Err(AcpRuntimeError::InvalidDescriptor(
            "executable and runtime identity must be present".to_owned(),
        ));
    }
    Ok(())
}

/// Failure at the GameEngine ACP runtime boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpRuntimeError {
    InvalidDescriptor(String),
    InvalidSessionBinding(String),
    DuplicateAgentId(String),
    Unsupported(String),
    Transport(String),
    Protocol(String),
}

impl fmt::Display for AcpRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDescriptor(message) => {
                write!(formatter, "invalid ACP descriptor: {message}")
            }
            Self::InvalidSessionBinding(message) => {
                write!(formatter, "invalid ACP session binding: {message}")
            }
            Self::DuplicateAgentId(id) => {
                write!(formatter, "ACP agent `{id}` is already registered")
            }
            Self::Unsupported(message) => write!(formatter, "unsupported ACP operation: {message}"),
            Self::Transport(message) => write!(formatter, "ACP transport error: {message}"),
            Self::Protocol(message) => write!(formatter, "ACP protocol error: {message}"),
        }
    }
}

impl std::error::Error for AcpRuntimeError {}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubRuntime(AcpAgentDescriptor);

    impl AcpAgentRuntime for StubRuntime {
        fn descriptor(&self) -> &AcpAgentDescriptor {
            &self.0
        }

        fn open_session(
            &mut self,
            _request: AcpSessionOpenRequest,
        ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError> {
            Err(AcpRuntimeError::Unsupported("stub".to_owned()))
        }
    }

    fn descriptor(id: &str) -> AcpAgentDescriptor {
        AcpAgentDescriptor {
            id: id.to_owned(),
            executable: OsString::from("future-acp-agent"),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            capabilities: AcpCapabilities::default(),
            runtime_identity: AcpRuntimeIdentity::stable("future-acp-agent", None),
        }
    }

    #[test]
    fn registry_is_not_provider_enum_driven() {
        let mut registry = AcpRuntimeRegistry::default();
        registry
            .register(Box::new(StubRuntime(descriptor("future.vendor.agent"))))
            .expect("arbitrary ACP descriptor ID should register");
        assert!(registry.runtime_mut("future.vendor.agent").is_some());
    }

    #[test]
    fn duplicate_agent_id_is_rejected() {
        let mut registry = AcpRuntimeRegistry::default();
        registry
            .register(Box::new(StubRuntime(descriptor("same.agent"))))
            .expect("first registration should succeed");
        assert_eq!(
            registry
                .register(Box::new(StubRuntime(descriptor("same.agent"))))
                .expect_err("duplicate registration must fail"),
            AcpRuntimeError::DuplicateAgentId("same.agent".to_owned())
        );
    }

    #[test]
    fn bindings_separate_read_only_and_run_bound_mcp() {
        let read_only =
            AcpSessionBinding::read_only("session-1", "http://127.0.0.1:1/mcp", "secret")
                .expect("read-only binding should be valid");
        assert!(read_only.gameengine_run_id.is_none());
        assert_eq!(read_only.mcp.access, AcpMcpAccessLevel::ReadOnly);

        let run_bound =
            AcpSessionBinding::run_bound("session-1", "run-1", "http://127.0.0.1:1/mcp", "secret")
                .expect("run-bound binding should be valid");
        assert_eq!(run_bound.gameengine_run_id.as_deref(), Some("run-1"));
        assert_eq!(
            run_bound.mcp.access,
            AcpMcpAccessLevel::AgentRunBoundReadWrite
        );
    }

    #[test]
    fn mcp_debug_redacts_credentials() {
        let connection = AcpMcpConnection::new(
            "http://127.0.0.1:1/mcp",
            "secret",
            AcpMcpAccessLevel::ReadOnly,
        )
        .expect("MCP connection should be valid");
        let debug = format!("{connection:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn finished_acp_turn_is_not_host_completion() {
        let event = AcpNormalizedEvent::TurnFinished {
            stop_reason: "end_turn".to_owned(),
        };
        assert_eq!(event.host_event_kind(), AgentEventKind::SemanticProgress);
        assert_ne!(event.host_event_kind(), AgentEventKind::Completion);
    }
}
