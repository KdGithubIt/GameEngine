//! Provider-neutral ACP to Agent Host authority bridge.
//!
//! ACP sessions own transport mechanics only. Agent Host remains authoritative
//! for permissions, work claims, canonical authoring, validation and completion.

use crate::acp_agent_runtime::{
    AcpAgentRegistry, AcpAgentSession, AcpMcpAccessLevel, AcpNormalizedEvent,
    AcpPermissionOptionKind, AcpPermissionOutcome, AcpPermissionRequest, AcpPermissionResolution,
    AcpPermissionTarget, AcpRuntimeError, AcpRuntimeIdentity, AcpSessionBinding,
    AcpSessionOpenRequest, AcpToolCallStatus,
};
use crate::agent_host::{
    AgentCapability, AgentEventKind, AgentHost, AgentHostError, AgentRunState, AgentWorkClaim,
    ApprovalScope, CompletionStatus, PermissionCheck,
};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AcpEditorMcpCredentials {
    endpoint: String,
    run_bound_token: String,
    read_only_token: String,
}

impl AcpEditorMcpCredentials {
    pub(crate) fn new(
        endpoint: impl Into<String>,
        run_bound_token: impl Into<String>,
        read_only_token: impl Into<String>,
    ) -> Result<Self, AcpBridgeError> {
        let value = Self {
            endpoint: endpoint.into(),
            run_bound_token: run_bound_token.into(),
            read_only_token: read_only_token.into(),
        };
        if value.endpoint.trim().is_empty()
            || value.run_bound_token.is_empty()
            || value.read_only_token.is_empty()
        {
            return Err(AcpBridgeError::InvalidMcpCredentials);
        }
        Ok(value)
    }
}

impl fmt::Debug for AcpEditorMcpCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpEditorMcpCredentials")
            .field("endpoint", &self.endpoint)
            .field("run_bound_token", &"<redacted>")
            .field("read_only_token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcpProviderCompletionGate {
    AcceptanceCriteria,
    AuthoringValidation,
}

impl AcpProviderCompletionGate {
    fn name(self) -> &'static str {
        match self {
            Self::AcceptanceCriteria => "acceptance_criteria",
            Self::AuthoringValidation => "authoring_validation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpBridgePoll {
    Idle,
    AskEvent(AcpNormalizedEvent),
    Recorded {
        run_id: String,
        kind: AgentEventKind,
    },
    RecordedEvent {
        run_id: String,
        kind: AgentEventKind,
        event: AcpNormalizedEvent,
    },
    PermissionRequired {
        run_id: String,
        request_id: String,
        title: String,
        capability: AgentCapability,
    },
    ValidationReady {
        run_id: String,
    },
    TurnFailed {
        run_id: String,
        reason: String,
    },
}

#[derive(Debug)]
pub(crate) enum AcpBridgeError {
    Host(AgentHostError),
    Runtime(AcpRuntimeError),
    InvalidMcpCredentials,
    InvalidWorkingDirectory,
    AgentNotRegistered(String),
    SessionNotFound(String),
    DuplicateSession(String),
    InvalidSessionId,
    BindingMismatch(String),
    RunSessionMismatch(String),
    TerminalRun(String),
    PermissionSessionMismatch(String),
    DuplicatePermission(String),
    PendingPermissionNotFound(String),
    UndeclaredCapability(AgentCapability),
    UnsafePermissionOptions(String),
    NotRunBound(String),
    ValidationNotReady(String),
}

impl fmt::Display for AcpBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host(error) => write!(formatter, "Agent Host error: {error}"),
            Self::Runtime(error) => write!(formatter, "ACP runtime error: {error}"),
            Self::InvalidMcpCredentials => {
                write!(formatter, "ACP MCP credentials must not be empty")
            }
            Self::InvalidWorkingDirectory => {
                write!(formatter, "ACP working directory must be absolute")
            }
            Self::AgentNotRegistered(id) => write!(formatter, "ACP agent `{id}` is not registered"),
            Self::SessionNotFound(id) => write!(formatter, "ACP session `{id}` was not found"),
            Self::DuplicateSession(id) => {
                write!(formatter, "ACP session `{id}` is already attached")
            }
            Self::InvalidSessionId => write!(formatter, "ACP session ID must not be empty"),
            Self::BindingMismatch(message) => write!(formatter, "ACP binding mismatch: {message}"),
            Self::RunSessionMismatch(run_id) => write!(
                formatter,
                "AgentRun `{run_id}` does not belong to the requested session"
            ),
            Self::TerminalRun(run_id) => write!(formatter, "AgentRun `{run_id}` is terminal"),
            Self::PermissionSessionMismatch(id) => write!(
                formatter,
                "ACP permission request belongs to session `{id}`"
            ),
            Self::DuplicatePermission(id) => write!(
                formatter,
                "ACP permission request `{id}` is already pending"
            ),
            Self::PendingPermissionNotFound(id) => {
                write!(formatter, "ACP permission request `{id}` is not pending")
            }
            Self::UndeclaredCapability(capability) => write!(
                formatter,
                "ACP requested undeclared capability `{}`",
                capability.label()
            ),
            Self::UnsafePermissionOptions(id) => write!(
                formatter,
                "ACP permission `{id}` has no safe allow-once option"
            ),
            Self::NotRunBound(id) => write!(formatter, "ACP session `{id}` is not run-bound"),
            Self::ValidationNotReady(run_id) => {
                write!(formatter, "AgentRun `{run_id}` is not validation-ready")
            }
        }
    }
}

impl std::error::Error for AcpBridgeError {}
impl From<AgentHostError> for AcpBridgeError {
    fn from(value: AgentHostError) -> Self {
        Self::Host(value)
    }
}
impl From<AcpRuntimeError> for AcpBridgeError {
    fn from(value: AcpRuntimeError) -> Self {
        Self::Runtime(value)
    }
}

struct AttachedSession {
    descriptor_id: String,
    identity: AcpRuntimeIdentity,
    run_id: Option<String>,
    session: Box<dyn AcpAgentSession>,
    pending_permissions: BTreeMap<String, AcpPermissionRequest>,
    turn_finished: bool,
}

pub(crate) struct AcpAgentHostBridge {
    mcp: AcpEditorMcpCredentials,
    working_directory: PathBuf,
    sessions: BTreeMap<String, AttachedSession>,
}

impl AcpAgentHostBridge {
    pub(crate) fn new(
        mcp: AcpEditorMcpCredentials,
        working_directory: PathBuf,
    ) -> Result<Self, AcpBridgeError> {
        if !working_directory.is_absolute() {
            return Err(AcpBridgeError::InvalidWorkingDirectory);
        }
        Ok(Self {
            mcp,
            working_directory,
            sessions: BTreeMap::new(),
        })
    }

    pub(crate) fn open_ask_session(
        &mut self,
        host: &AgentHost,
        registry: &mut dyn AcpAgentRegistry,
        agent_id: &str,
        gameengine_session_id: &str,
    ) -> Result<String, AcpBridgeError> {
        host.session(gameengine_session_id)?;
        let binding = AcpSessionBinding::read_only(
            gameengine_session_id,
            self.mcp.endpoint.clone(),
            self.mcp.read_only_token.clone(),
        )?;
        self.open_registered(
            registry,
            agent_id,
            binding,
            self.working_directory.clone(),
            None,
        )
    }

    pub(crate) fn open_run_session(
        &mut self,
        host: &mut AgentHost,
        registry: &mut dyn AcpAgentRegistry,
        agent_id: &str,
        gameengine_session_id: &str,
        run_id: &str,
        working_directory: PathBuf,
    ) -> Result<String, AcpBridgeError> {
        if !host
            .session(gameengine_session_id)?
            .runs
            .iter()
            .any(|run| run.id == run_id)
        {
            return Err(AcpBridgeError::RunSessionMismatch(run_id.to_owned()));
        }
        if host.run(run_id)?.state.is_terminal() {
            return Err(AcpBridgeError::TerminalRun(run_id.to_owned()));
        }
        let binding = AcpSessionBinding::run_bound(
            gameengine_session_id,
            run_id,
            self.mcp.endpoint.clone(),
            self.mcp.run_bound_token.clone(),
        )?;
        let acp_id = self.open_registered(
            registry,
            agent_id,
            binding,
            working_directory,
            Some(run_id.to_owned()),
        )?;
        if let Err(error) = self.record_identity(host, &acp_id) {
            let _ = self.close_session(&acp_id);
            return Err(error);
        }
        Ok(acp_id)
    }

    fn open_registered(
        &mut self,
        registry: &mut dyn AcpAgentRegistry,
        agent_id: &str,
        binding: AcpSessionBinding,
        working_directory: PathBuf,
        expected_run_id: Option<String>,
    ) -> Result<String, AcpBridgeError> {
        let expected_session_id = binding.gameengine_session_id.clone();
        let request = AcpSessionOpenRequest::new(binding, working_directory)?;
        let (descriptor_id, mut session) = {
            let runtime = registry
                .runtime_mut(agent_id)
                .ok_or_else(|| AcpBridgeError::AgentNotRegistered(agent_id.to_owned()))?;
            let descriptor_id = runtime.descriptor().id.clone();
            let session = runtime.open_session(request)?;
            (descriptor_id, session)
        };
        let identity = session.runtime_identity().clone();
        let acp_id = session.acp_session_id().to_owned();
        if acp_id.trim().is_empty() {
            let _ = session.close();
            return Err(AcpBridgeError::InvalidSessionId);
        }
        if self.sessions.contains_key(&acp_id) {
            let _ = session.close();
            return Err(AcpBridgeError::DuplicateSession(acp_id));
        }
        if session.binding().gameengine_session_id != expected_session_id {
            let _ = session.close();
            return Err(AcpBridgeError::BindingMismatch(
                "GameEngine session identity changed while opening ACP".to_owned(),
            ));
        }
        if session.binding().gameengine_run_id != expected_run_id {
            let _ = session.close();
            return Err(AcpBridgeError::BindingMismatch(
                "AgentRun identity changed while opening ACP".to_owned(),
            ));
        }
        self.sessions.insert(
            acp_id.clone(),
            AttachedSession {
                descriptor_id,
                identity,
                run_id: expected_run_id,
                session,
                pending_permissions: BTreeMap::new(),
                turn_finished: false,
            },
        );
        Ok(acp_id)
    }

    fn record_identity(&self, host: &mut AgentHost, acp_id: &str) -> Result<(), AcpBridgeError> {
        let attached = self.attached(acp_id)?;
        let Some(run_id) = attached.run_id.as_deref() else {
            return Ok(());
        };
        host.record_event(
            run_id,
            AgentEventKind::ProviderOutput,
            format!(
                "ACP runtime attached: descriptor={}, protocol=v{}, agent={}, version={}, acp_session={}.",
                attached.descriptor_id,
                attached.identity.protocol_version,
                attached.identity.agent_name,
                attached.identity.agent_version.as_deref().unwrap_or("unknown"),
                acp_id,
            ),
        )?;
        Ok(())
    }

    pub(crate) fn send_prompt(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        prompt: &str,
    ) -> Result<(), AcpBridgeError> {
        if let Err(error) = self.attached_mut(acp_id)?.session.send_prompt(prompt) {
            return self.fail_runtime(host, acp_id, error);
        }
        self.attached_mut(acp_id)?.turn_finished = false;
        Ok(())
    }

    pub(crate) fn poll_session(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
    ) -> Result<AcpBridgePoll, AcpBridgeError> {
        let event = match self.attached_mut(acp_id)?.session.try_next_event() {
            Ok(event) => event,
            Err(error) => return self.fail_runtime(host, acp_id, error),
        };
        let Some(event) = event else {
            return Ok(AcpBridgePoll::Idle);
        };
        if let Some(error) = terminal_prompt_error(&event) {
            return self.fail_runtime(host, acp_id, error);
        }
        match self.attached(acp_id)?.run_id.clone() {
            Some(run_id) => self.record_run_event(host, acp_id, &run_id, event),
            None => self.record_ask_event(host, acp_id, event),
        }
    }

    fn record_ask_event(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        event: AcpNormalizedEvent,
    ) -> Result<AcpBridgePoll, AcpBridgeError> {
        if let AcpNormalizedEvent::PermissionRequest(request) = &event {
            self.check_permission_session(acp_id, request)?;
            let read_only_binding =
                self.attached(acp_id)?.session.binding().mcp.access == AcpMcpAccessLevel::ReadOnly;
            let (outcome, message) = match &request.target {
                AcpPermissionTarget::GameEngineMcpTool {
                    stable_name,
                    mutating: false,
                } if read_only_binding => {
                    let Some(option_id) = allow_once_option(request) else {
                        self.resolve_acp(
                            host,
                            acp_id,
                            AcpPermissionResolution {
                                request_id: request.request_id.clone(),
                                outcome: host_rejection_outcome(request),
                            },
                        )?;
                        return Ok(AcpBridgePoll::AskEvent(
                            AcpNormalizedEvent::ProtocolDiagnostic {
                                message: format!(
                                    "ACP host rejected registered GameEngine MCP tool `{stable_name}` because the agent offered no safe allow-once permission option."
                                ),
                            },
                        ));
                    };
                    (
                        AcpPermissionOutcome::SelectedOption(option_id),
                        format!(
                            "ACP host allowed registered read-only GameEngine MCP tool `{stable_name}` under the Ask MCP contract."
                        ),
                    )
                }
                AcpPermissionTarget::GameEngineMcpTool {
                    stable_name,
                    mutating: true,
                } => (
                    host_rejection_outcome(request),
                    format!(
                        "ACP host denied GameEngine MCP tool `{stable_name}` because Ask is read-only; no user decline was recorded."
                    ),
                ),
                AcpPermissionTarget::AgentCapability(capability) => (
                    host_rejection_outcome(request),
                    format!(
                        "ACP host denied `{}` because Ask cannot grant elevated Agent Host capabilities; no user decline was recorded.",
                        capability.label()
                    ),
                ),
                AcpPermissionTarget::Unclassified
                | AcpPermissionTarget::GameEngineMcpTool { .. } => (
                    host_rejection_outcome(request),
                    "ACP host denied an unclassified permission request; no user decline was recorded."
                        .to_owned(),
                ),
            };
            self.resolve_acp(
                host,
                acp_id,
                AcpPermissionResolution {
                    request_id: request.request_id.clone(),
                    outcome,
                },
            )?;
            return Ok(AcpBridgePoll::AskEvent(
                AcpNormalizedEvent::ProtocolDiagnostic { message },
            ));
        }
        Ok(AcpBridgePoll::AskEvent(event))
    }

    fn record_run_event(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        run_id: &str,
        event: AcpNormalizedEvent,
    ) -> Result<AcpBridgePoll, AcpBridgeError> {
        if host.run(run_id)?.state.is_terminal() {
            self.close_session(acp_id)?;
            return Err(AcpBridgeError::TerminalRun(run_id.to_owned()));
        }
        let kind = event.host_event_kind();
        let recorded_event = event.clone();
        match event {
            AcpNormalizedEvent::AgentMessage { text } => {
                host.record_event(run_id, AgentEventKind::AssistantMessage, text)?
            }
            AcpNormalizedEvent::Progress { step, detail } => {
                host.record_semantic_progress(run_id, step, detail)?
            }
            AcpNormalizedEvent::Plan { entries } => {
                host.record_semantic_progress(run_id, "ACP plan", entries.join("\n"))?
            }
            AcpNormalizedEvent::ToolCall {
                tool_call_id,
                title,
                status,
            } => {
                let success = match status {
                    AcpToolCallStatus::Pending | AcpToolCallStatus::InProgress => None,
                    AcpToolCallStatus::Completed => Some(true),
                    AcpToolCallStatus::Failed => Some(false),
                };
                host.record_tool_action(
                    run_id,
                    format!("acp:{tool_call_id}"),
                    format!("{title} ({status:?})"),
                    success,
                )?;
            }
            AcpNormalizedEvent::PermissionRequest(request) => {
                return self.record_permission(host, acp_id, run_id, request);
            }
            AcpNormalizedEvent::SessionInfo { title } => host.record_semantic_progress(
                run_id,
                "ACP session",
                title.unwrap_or_else(|| "session metadata updated".to_owned()),
            )?,
            AcpNormalizedEvent::TurnFinished { stop_reason } => {
                host.record_semantic_progress(run_id, "ACP turn finished", format!("Agent returned control with `{stop_reason}`; Agent Host completion remains authoritative."))?;
                self.attached_mut(acp_id)?.turn_finished = true;
                if self.validation_ready(host, acp_id)? {
                    return Ok(AcpBridgePoll::ValidationReady {
                        run_id: run_id.to_owned(),
                    });
                }
                if let Some(reason) =
                    self.turn_failure_reason(host, acp_id, stop_reason.as_str())?
                {
                    host.record_event(run_id, AgentEventKind::Failure, reason.clone())?;
                    host.transition_run(
                        run_id,
                        AgentRunState::Failed,
                        "ACP provider turn ended without satisfying Agent Host completion gates.",
                    )?;
                    return Ok(AcpBridgePoll::TurnFailed {
                        run_id: run_id.to_owned(),
                        reason,
                    });
                }
            }
            AcpNormalizedEvent::PromptFailed { message } => {
                return self.fail_runtime(host, acp_id, AcpRuntimeError::Transport(message));
            }
            AcpNormalizedEvent::ProtocolDiagnostic { message } => host.record_event(
                run_id,
                AgentEventKind::ProviderOutput,
                format!("ACP protocol diagnostic: {message}"),
            )?,
        }
        Ok(AcpBridgePoll::RecordedEvent {
            run_id: run_id.to_owned(),
            kind,
            event: recorded_event,
        })
    }

    fn record_permission(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        run_id: &str,
        request: AcpPermissionRequest,
    ) -> Result<AcpBridgePoll, AcpBridgeError> {
        self.check_permission_session(acp_id, &request)?;
        match request.target.clone() {
            AcpPermissionTarget::GameEngineMcpTool {
                stable_name,
                mutating,
            } => {
                let run_bound_binding = self.attached(acp_id)?.session.binding().mcp.access
                    == AcpMcpAccessLevel::AgentRunBoundReadWrite;
                if !run_bound_binding {
                    self.resolve_acp(
                        host,
                        acp_id,
                        AcpPermissionResolution {
                            request_id: request.request_id.clone(),
                            outcome: host_rejection_outcome(&request),
                        },
                    )?;
                    host.record_event(
                        run_id,
                        AgentEventKind::PermissionResolved,
                        format!(
                            "ACP host denied GameEngine MCP tool `{stable_name}` because the session is not run-bound; no user decline was recorded."
                        ),
                    )?;
                    return Ok(AcpBridgePoll::Recorded {
                        run_id: run_id.to_owned(),
                        kind: AgentEventKind::PermissionResolved,
                    });
                }
                let Some(option_id) = allow_once_option(&request) else {
                    self.resolve_acp(
                        host,
                        acp_id,
                        AcpPermissionResolution {
                            request_id: request.request_id.clone(),
                            outcome: host_rejection_outcome(&request),
                        },
                    )?;
                    return Err(AcpBridgeError::UnsafePermissionOptions(request.request_id));
                };
                self.resolve_acp(
                    host,
                    acp_id,
                    AcpPermissionResolution {
                        request_id: request.request_id.clone(),
                        outcome: AcpPermissionOutcome::SelectedOption(option_id),
                    },
                )?;
                host.record_event(
                    run_id,
                    AgentEventKind::PermissionResolved,
                    format!(
                        "ACP host allowed registered GameEngine MCP tool `{stable_name}` under the run-bound {} authoring contract.",
                        if mutating { "write" } else { "read" }
                    ),
                )?;
                Ok(AcpBridgePoll::Recorded {
                    run_id: run_id.to_owned(),
                    kind: AgentEventKind::PermissionResolved,
                })
            }
            AcpPermissionTarget::Unclassified => {
                self.resolve_acp(
                    host,
                    acp_id,
                    AcpPermissionResolution {
                        request_id: request.request_id.clone(),
                        outcome: host_rejection_outcome(&request),
                    },
                )?;
                host.record_event(
                    run_id,
                    AgentEventKind::PermissionResolved,
                    "ACP host denied a permission request whose tool identity/capability could not be safely classified; no user decline was recorded.",
                )?;
                Ok(AcpBridgePoll::Recorded {
                    run_id: run_id.to_owned(),
                    kind: AgentEventKind::PermissionResolved,
                })
            }
            AcpPermissionTarget::AgentCapability(required_capability) => {
                if !host
                    .run(run_id)?
                    .proposal_snapshot
                    .requested_capabilities
                    .contains(&required_capability)
                {
                    self.resolve_acp(
                        host,
                        acp_id,
                        AcpPermissionResolution {
                            request_id: request.request_id.clone(),
                            outcome: host_rejection_outcome(&request),
                        },
                    )?;
                    return Err(AcpBridgeError::UndeclaredCapability(required_capability));
                }
                match host.check_permission(run_id, required_capability)? {
                    PermissionCheck::Granted => {
                        let Some(option_id) = allow_once_option(&request) else {
                            self.resolve_acp(
                                host,
                                acp_id,
                                AcpPermissionResolution {
                                    request_id: request.request_id.clone(),
                                    outcome: host_rejection_outcome(&request),
                                },
                            )?;
                            return Err(AcpBridgeError::UnsafePermissionOptions(
                                request.request_id,
                            ));
                        };
                        self.resolve_acp(
                            host,
                            acp_id,
                            AcpPermissionResolution {
                                request_id: request.request_id.clone(),
                                outcome: AcpPermissionOutcome::SelectedOption(option_id),
                            },
                        )?;
                        Ok(AcpBridgePoll::Recorded {
                            run_id: run_id.to_owned(),
                            kind: AgentEventKind::PermissionResolved,
                        })
                    }
                    PermissionCheck::Denied => {
                        self.resolve_acp(
                            host,
                            acp_id,
                            AcpPermissionResolution {
                                request_id: request.request_id.clone(),
                                outcome: host_rejection_outcome(&request),
                            },
                        )?;
                        host.record_event(
                            run_id,
                            AgentEventKind::PermissionResolved,
                            format!(
                                "ACP host denied `{}` by existing Agent Host policy; no user decline was recorded.",
                                required_capability.label()
                            ),
                        )?;
                        Ok(AcpBridgePoll::Recorded {
                            run_id: run_id.to_owned(),
                            kind: AgentEventKind::PermissionResolved,
                        })
                    }
                    PermissionCheck::RequiresApproval => {
                        let request_id = request.request_id.clone();
                        let title = request.title.clone();
                        let pending = &mut self.attached_mut(acp_id)?.pending_permissions;
                        if pending.contains_key(&request_id) {
                            return Err(AcpBridgeError::DuplicatePermission(request_id));
                        }
                        pending.insert(request_id.clone(), request);
                        Ok(AcpBridgePoll::PermissionRequired {
                            run_id: run_id.to_owned(),
                            request_id,
                            title,
                            capability: required_capability,
                        })
                    }
                }
            }
        }
    }

    pub(crate) fn resolve_permission(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        request_id: &str,
        scope: ApprovalScope,
    ) -> Result<(), AcpBridgeError> {
        let run_id = self.run_id(acp_id)?.to_owned();
        let request = self
            .attached(acp_id)?
            .pending_permissions
            .get(request_id)
            .cloned()
            .ok_or_else(|| AcpBridgeError::PendingPermissionNotFound(request_id.to_owned()))?;
        let AcpPermissionTarget::AgentCapability(required_capability) = &request.target else {
            return Err(AcpBridgeError::UnsafePermissionOptions(
                request_id.to_owned(),
            ));
        };
        let required_capability = *required_capability;
        if scope == ApprovalScope::Deny {
            host.resolve_permission(&run_id, required_capability, scope)?;
            self.resolve_cancel(host, acp_id, request_id)?;
        } else {
            let Some(option_id) = allow_once_option(&request) else {
                self.resolve_cancel(host, acp_id, request_id)?;
                self.attached_mut(acp_id)?
                    .pending_permissions
                    .remove(request_id);
                return Err(AcpBridgeError::UnsafePermissionOptions(
                    request_id.to_owned(),
                ));
            };
            host.resolve_permission(&run_id, required_capability, scope)?;
            if host.check_permission(&run_id, required_capability)? != PermissionCheck::Granted {
                return Err(AcpBridgeError::UnsafePermissionOptions(
                    request_id.to_owned(),
                ));
            }
            self.resolve_acp(
                host,
                acp_id,
                AcpPermissionResolution {
                    request_id: request_id.to_owned(),
                    outcome: AcpPermissionOutcome::SelectedOption(option_id),
                },
            )?;
        }
        self.attached_mut(acp_id)?
            .pending_permissions
            .remove(request_id);
        Ok(())
    }

    fn resolve_cancel(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        request_id: &str,
    ) -> Result<(), AcpBridgeError> {
        self.resolve_acp(
            host,
            acp_id,
            AcpPermissionResolution {
                request_id: request_id.to_owned(),
                outcome: AcpPermissionOutcome::Cancelled,
            },
        )
    }

    fn resolve_acp(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        resolution: AcpPermissionResolution,
    ) -> Result<(), AcpBridgeError> {
        match self
            .attached_mut(acp_id)?
            .session
            .resolve_permission(resolution)
        {
            Ok(()) => Ok(()),
            Err(error) => self.fail_runtime(host, acp_id, error),
        }
    }

    fn check_permission_session(
        &self,
        acp_id: &str,
        request: &AcpPermissionRequest,
    ) -> Result<(), AcpBridgeError> {
        if request.acp_session_id == acp_id {
            Ok(())
        } else {
            Err(AcpBridgeError::PermissionSessionMismatch(
                request.acp_session_id.clone(),
            ))
        }
    }

    pub(crate) fn record_provider_completion_gate(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        gate: AcpProviderCompletionGate,
        status: CompletionStatus,
        message: impl Into<String>,
    ) -> Result<bool, AcpBridgeError> {
        let run_id = self.run_id(acp_id)?.to_owned();
        host.record_completion_gate(&run_id, gate.name(), status, message)?;
        self.validation_ready(host, acp_id)
    }

    pub(crate) fn validation_ready(
        &self,
        host: &AgentHost,
        acp_id: &str,
    ) -> Result<bool, AcpBridgeError> {
        let attached = self.attached(acp_id)?;
        let Some(run_id) = attached.run_id.as_deref() else {
            return Ok(false);
        };
        if !attached.turn_finished || !attached.pending_permissions.is_empty() {
            return Ok(false);
        }
        let run = host.run(run_id)?;
        if run
            .work_claims
            .contains(&AgentWorkClaim::shared_resource("canonical_authoring"))
        {
            return Ok(false);
        }
        Ok(matches!(
            run.state,
            AgentRunState::Executing | AgentRunState::AwaitingUser | AgentRunState::Repairing
        ) && gate_satisfied(run.completion.acceptance_criteria)
            && gate_satisfied(run.completion.authoring_validation))
    }

    fn turn_failure_reason(
        &self,
        host: &AgentHost,
        acp_id: &str,
        stop_reason: &str,
    ) -> Result<Option<String>, AcpBridgeError> {
        let attached = self.attached(acp_id)?;
        let Some(run_id) = attached.run_id.as_deref() else {
            return Ok(None);
        };
        let run = host.run(run_id)?;
        Ok(provider_turn_failure_reason(
            run.state,
            stop_reason,
            !attached.pending_permissions.is_empty(),
            run.work_claims
                .contains(&AgentWorkClaim::shared_resource("canonical_authoring")),
            run.completion.acceptance_criteria,
            run.completion.authoring_validation,
        ))
    }

    pub(crate) fn begin_managed_validation(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        code_changes_present: bool,
    ) -> Result<(), AcpBridgeError> {
        let run_id = self.run_id(acp_id)?.to_owned();
        if !self.validation_ready(host, acp_id)? {
            return Err(AcpBridgeError::ValidationNotReady(run_id));
        }
        host.begin_managed_validation(&run_id, code_changes_present)?;
        Ok(())
    }

    pub(crate) fn cancel_run(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
    ) -> Result<(), AcpBridgeError> {
        let run_id = self.run_id(acp_id)?.to_owned();
        if let Err(error) = self.attached_mut(acp_id)?.session.cancel() {
            host.record_event(
                &run_id,
                AgentEventKind::ProviderOutput,
                format!("ACP cancellation transport failed: {error}"),
            )?;
        }
        host.cancel_run(&run_id)?;
        self.close_session(acp_id)?;
        Ok(())
    }

    pub(crate) fn reap_terminal_sessions(
        &mut self,
        host: &AgentHost,
    ) -> Result<(), AcpBridgeError> {
        let mut terminal = Vec::new();
        for (acp_id, attached) in &self.sessions {
            if let Some(run_id) = attached.run_id.as_deref()
                && host.run(run_id)?.state.is_terminal()
            {
                terminal.push(acp_id.clone());
            }
        }
        for acp_id in terminal {
            self.close_session(&acp_id)?;
        }
        Ok(())
    }

    pub(crate) fn close_session(&mut self, acp_id: &str) -> Result<(), AcpBridgeError> {
        let mut attached = self
            .sessions
            .remove(acp_id)
            .ok_or_else(|| AcpBridgeError::SessionNotFound(acp_id.to_owned()))?;
        attached.session.close()?;
        Ok(())
    }

    pub(crate) fn run_id(&self, acp_id: &str) -> Result<&str, AcpBridgeError> {
        self.attached(acp_id)?
            .run_id
            .as_deref()
            .ok_or_else(|| AcpBridgeError::NotRunBound(acp_id.to_owned()))
    }

    pub(crate) fn runtime_identity(
        &self,
        acp_id: &str,
    ) -> Result<&AcpRuntimeIdentity, AcpBridgeError> {
        Ok(&self.attached(acp_id)?.identity)
    }

    fn attached(&self, acp_id: &str) -> Result<&AttachedSession, AcpBridgeError> {
        self.sessions
            .get(acp_id)
            .ok_or_else(|| AcpBridgeError::SessionNotFound(acp_id.to_owned()))
    }
    fn attached_mut(&mut self, acp_id: &str) -> Result<&mut AttachedSession, AcpBridgeError> {
        self.sessions
            .get_mut(acp_id)
            .ok_or_else(|| AcpBridgeError::SessionNotFound(acp_id.to_owned()))
    }

    fn fail_runtime<T>(
        &mut self,
        host: &mut AgentHost,
        acp_id: &str,
        error: AcpRuntimeError,
    ) -> Result<T, AcpBridgeError> {
        let run_id = self
            .sessions
            .get(acp_id)
            .and_then(|entry| entry.run_id.clone());
        if let Some(run_id) = run_id
            && !host.run(&run_id)?.state.is_terminal()
        {
            host.record_event(
                &run_id,
                AgentEventKind::Failure,
                format!("ACP runtime/session failed: {error}"),
            )?;
            host.transition_run(
                &run_id,
                AgentRunState::Failed,
                "ACP runtime/session failure ended the run.",
            )?;
        }
        if let Some(mut attached) = self.sessions.remove(acp_id) {
            let _ = attached.session.close();
        }
        Err(AcpBridgeError::Runtime(error))
    }
}

fn allow_once_option(request: &AcpPermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| option.kind == AcpPermissionOptionKind::AllowOnce)
        .map(|option| option.id.clone())
}

fn reject_once_option(request: &AcpPermissionRequest) -> Option<String> {
    request
        .options
        .iter()
        .find(|option| option.kind == AcpPermissionOptionKind::RejectOnce)
        .map(|option| option.id.clone())
}

fn host_rejection_outcome(request: &AcpPermissionRequest) -> AcpPermissionOutcome {
    reject_once_option(request)
        .map(AcpPermissionOutcome::SelectedOption)
        .unwrap_or(AcpPermissionOutcome::Cancelled)
}

fn terminal_prompt_error(event: &AcpNormalizedEvent) -> Option<AcpRuntimeError> {
    let AcpNormalizedEvent::PromptFailed { message } = event else {
        return None;
    };
    Some(AcpRuntimeError::Transport(message.clone()))
}

fn provider_turn_failure_reason(
    state: AgentRunState,
    stop_reason: &str,
    has_pending_permissions: bool,
    has_canonical_authoring: bool,
    acceptance_criteria: CompletionStatus,
    authoring_validation: CompletionStatus,
) -> Option<String> {
    if state == AgentRunState::AwaitingUser
        || !matches!(state, AgentRunState::Executing | AgentRunState::Repairing)
    {
        return None;
    }
    if stop_reason != "end_turn" {
        return Some(format!(
            "ACP agent returned control with `{stop_reason}` before successful host completion."
        ));
    }
    if has_pending_permissions {
        return Some(
            "ACP agent returned control while a permission request was still unresolved."
                .to_owned(),
        );
    }
    if has_canonical_authoring {
        return Some(
            "ACP agent returned control while canonical authoring work was still active."
                .to_owned(),
        );
    }

    let mut unsatisfied = Vec::new();
    if !gate_satisfied(acceptance_criteria) {
        unsatisfied.push("acceptance_criteria");
    }
    if !gate_satisfied(authoring_validation) {
        unsatisfied.push("authoring_validation");
    }
    if unsatisfied.is_empty() {
        None
    } else {
        Some(format!(
            "ACP agent returned control before required provider completion gates were satisfied: {}.",
            unsatisfied.join(", ")
        ))
    }
}

fn gate_satisfied(status: CompletionStatus) -> bool {
    matches!(
        status,
        CompletionStatus::Passed | CompletionStatus::NotApplicable
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_agent_runtime::{
        AcpAgentDescriptor, AcpAgentRuntime, AcpCapabilities, AcpPermissionOption,
        AcpRuntimeRegistry,
    };
    use std::ffi::OsString;

    fn permission_request(options: Vec<AcpPermissionOption>) -> AcpPermissionRequest {
        AcpPermissionRequest {
            request_id: "permission-1".to_owned(),
            acp_session_id: "acp-1".to_owned(),
            tool_call_id: "opaque-call-1".to_owned(),
            title: "asset search".to_owned(),
            target: AcpPermissionTarget::Unclassified,
            options,
        }
    }

    #[test]
    fn host_policy_rejection_is_not_explicit_user_cancellation() {
        let request = permission_request(vec![AcpPermissionOption {
            id: "reject_once".to_owned(),
            name: "Reject once".to_owned(),
            kind: AcpPermissionOptionKind::RejectOnce,
        }]);
        assert_eq!(
            host_rejection_outcome(&request),
            AcpPermissionOutcome::SelectedOption("reject_once".to_owned())
        );
        assert_ne!(
            host_rejection_outcome(&request),
            AcpPermissionOutcome::Cancelled
        );
    }

    #[test]
    fn provider_end_turn_with_satisfied_gates_can_validate() {
        assert_eq!(
            provider_turn_failure_reason(
                AgentRunState::Executing,
                "end_turn",
                false,
                false,
                CompletionStatus::Passed,
                CompletionStatus::NotApplicable,
            ),
            None
        );
    }

    #[test]
    fn provider_end_turn_with_failed_gate_becomes_terminal_failure() {
        let reason = provider_turn_failure_reason(
            AgentRunState::Executing,
            "end_turn",
            false,
            false,
            CompletionStatus::Failed,
            CompletionStatus::NotApplicable,
        )
        .expect("failed provider gate must terminate the returned turn");
        assert!(reason.contains("acceptance_criteria"));
    }

    #[test]
    fn awaiting_user_is_not_failed_when_provider_returns_control() {
        assert_eq!(
            provider_turn_failure_reason(
                AgentRunState::AwaitingUser,
                "end_turn",
                false,
                false,
                CompletionStatus::Pending,
                CompletionStatus::Pending,
            ),
            None
        );
    }

    #[test]
    fn prompt_rpc_failure_is_terminal_but_diagnostic_is_not() {
        let failure = AcpNormalizedEvent::PromptFailed {
            message: "context length exceeded after truncated tool arguments".to_owned(),
        };
        assert_eq!(failure.host_event_kind(), AgentEventKind::Failure);
        assert!(matches!(
            terminal_prompt_error(&failure),
            Some(AcpRuntimeError::Transport(message))
                if message.contains("context length exceeded")
        ));

        let diagnostic = AcpNormalizedEvent::ProtocolDiagnostic {
            message: "unknown optional ACP notification".to_owned(),
        };
        assert!(terminal_prompt_error(&diagnostic).is_none());
    }

    struct PromptFailingRuntime {
        descriptor: AcpAgentDescriptor,
    }

    impl PromptFailingRuntime {
        fn new() -> Self {
            Self {
                descriptor: AcpAgentDescriptor {
                    id: "prompt-failing-test".to_owned(),
                    executable: OsString::from("prompt-failing-test"),
                    arguments: Vec::new(),
                    environment: BTreeMap::new(),
                    capabilities: AcpCapabilities::default(),
                    runtime_identity: AcpRuntimeIdentity::stable(
                        "prompt-failing-test",
                        Some("1.0.0".to_owned()),
                    ),
                },
            }
        }
    }

    impl AcpAgentRuntime for PromptFailingRuntime {
        fn descriptor(&self) -> &AcpAgentDescriptor {
            &self.descriptor
        }

        fn open_session(
            &mut self,
            request: AcpSessionOpenRequest,
        ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError> {
            Ok(Box::new(PromptFailingSession {
                binding: request.binding,
                capabilities: self.descriptor.capabilities.clone(),
                runtime_identity: self.descriptor.runtime_identity.clone(),
                event: Some(AcpNormalizedEvent::PromptFailed {
                    message: "context length exceeded after finish_reason=length truncated tool arguments"
                        .to_owned(),
                }),
            }))
        }
    }

    struct PromptFailingSession {
        binding: AcpSessionBinding,
        capabilities: AcpCapabilities,
        runtime_identity: AcpRuntimeIdentity,
        event: Option<AcpNormalizedEvent>,
    }

    impl AcpAgentSession for PromptFailingSession {
        fn acp_session_id(&self) -> &str {
            "acp-prompt-failure-test"
        }

        fn binding(&self) -> &AcpSessionBinding {
            &self.binding
        }

        fn capabilities(&self) -> &AcpCapabilities {
            &self.capabilities
        }

        fn runtime_identity(&self) -> &AcpRuntimeIdentity {
            &self.runtime_identity
        }

        fn send_prompt(&mut self, _prompt: &str) -> Result<(), AcpRuntimeError> {
            Ok(())
        }

        fn try_next_event(&mut self) -> Result<Option<AcpNormalizedEvent>, AcpRuntimeError> {
            Ok(self.event.take())
        }

        fn resolve_permission(
            &mut self,
            _resolution: AcpPermissionResolution,
        ) -> Result<(), AcpRuntimeError> {
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), AcpRuntimeError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), AcpRuntimeError> {
            Ok(())
        }
    }

    #[test]
    fn truncated_prompt_failure_fails_run_and_closes_session() {
        let root = std::env::temp_dir().join(format!(
            "gameengine-acp-prompt-failure-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("project");
        let storage = root.join("state");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&project).expect("project fixture");
        std::fs::create_dir_all(&workspace).expect("workspace fixture");

        let mut host = AgentHost::open(project.clone(), storage).expect("Agent Host");
        let gameengine_session_id = host.create_session("Prompt failure").expect("session");
        let proposal_version = host
            .session(&gameengine_session_id)
            .expect("session")
            .proposal
            .version;
        let run_id = host
            .start_run_authorized(&gameengine_session_id, proposal_version, "test")
            .expect("run");

        let mut registry = AcpRuntimeRegistry::default();
        registry
            .register(Box::new(PromptFailingRuntime::new()))
            .expect("test runtime");
        let credentials =
            AcpEditorMcpCredentials::new("http://127.0.0.1:1/mcp", "run-token", "read-token")
                .expect("credentials");
        let mut bridge = AcpAgentHostBridge::new(credentials, project).expect("bridge");
        let acp_session_id = bridge
            .open_run_session(
                &mut host,
                &mut registry,
                "prompt-failing-test",
                &gameengine_session_id,
                &run_id,
                workspace,
            )
            .expect("ACP session");
        bridge
            .send_prompt(&mut host, &acp_session_id, "use a tool")
            .expect("prompt accepted");

        let error = bridge
            .poll_session(&mut host, &acp_session_id)
            .expect_err("truncated prompt failure must terminate the run");
        assert!(error.to_string().contains("context length exceeded"));
        assert_eq!(host.run(&run_id).expect("run").state, AgentRunState::Failed);
        assert!(matches!(
            bridge.poll_session(&mut host, &acp_session_id),
            Err(AcpBridgeError::SessionNotFound(id)) if id == acp_session_id
        ));

        let _ = std::fs::remove_dir_all(root);
    }
}
