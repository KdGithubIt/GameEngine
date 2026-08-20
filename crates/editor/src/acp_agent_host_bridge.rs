//! Provider-neutral ACP to Agent Host authority bridge.
//!
//! ACP sessions remain transport clients. Agent Host keeps final authority over
//! permissions, claims, canonical authoring, validation, audit, and completion.

use crate::acp_agent_runtime::{
    AcpAgentRegistry, AcpAgentSession, AcpNormalizedEvent, AcpPermissionOptionKind,
    AcpPermissionOutcome, AcpPermissionRequest, AcpPermissionResolution, AcpRuntimeError,
    AcpRuntimeIdentity, AcpSessionBinding, AcpToolCallStatus,
};
use crate::agent_host::{
    AgentCapability, AgentEventKind, AgentHost, AgentHostError, AgentRunState, AgentWorkClaim,
    ApprovalScope, CompletionStatus, PermissionCheck,
};
use std::collections::BTreeMap;
use std::fmt;

/// Ephemeral Editor MCP credentials exposed to ACP. Unrestricted read-write is absent.
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

/// Provider-owned gates ACP may report. Host-owned validation gates are not representable.
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

/// Result of one non-blocking ACP pump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpBridgePoll {
    Idle,
    AskEvent(AcpNormalizedEvent),
    Recorded {
        run_id: String,
        kind: AgentEventKind,
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
}

#[derive(Debug)]
pub(crate) enum AcpBridgeError {
    Host(AgentHostError),
    Runtime(AcpRuntimeError),
    InvalidMcpCredentials,
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
            Self::InvalidMcpCredentials => write!(formatter, "ACP MCP credentials must not be empty"),
            Self::AgentNotRegistered(id) => write!(formatter, "ACP agent `{id}` is not registered"),
            Self::SessionNotFound(id) => write!(formatter, "ACP session `{id}` was not found"),
            Self::DuplicateSession(id) => write!(formatter, "ACP session `{id}` is already attached"),
            Self::InvalidSessionId => write!(formatter, "ACP session ID must not be empty"),
            Self::BindingMismatch(message) => write!(formatter, "ACP binding mismatch: {message}"),
            Self::RunSessionMismatch(run_id) => {
                write!(formatter, "AgentRun `{run_id}` does not belong to the requested session")
            }
            Self::TerminalRun(run_id) => write!(formatter, "AgentRun `{run_id}` is terminal"),
            Self::PermissionSessionMismatch(id) => {
                write!(formatter, "ACP permission request belongs to session `{id}`")
            }
            Self::DuplicatePermission(id) => {
                write!(formatter, "ACP permission request `{id}` is already pending")
            }
            Self::PendingPermissionNotFound(id) => {
                write!(formatter, "ACP permission request `{id}` is not pending")
            }
            Self::UndeclaredCapability(capability) => write!(
                formatter,
                "ACP requested undeclared capability `{}`",
                capability.label()
            ),
            Self::UnsafePermissionOptions(id) => {
                write!(formatter, "ACP permission `{id}` has no safe allow-once option")
            }
            Self::NotRunBound(id) => write!(formatter, "ACP session `{id}` is not run-bound"),
            Self::ValidationNotReady(run_id) => {
                write!(formatter, "AgentRun `{run_id}` is not validation-ready")
            }
        }
    }
}

impl std::error::Error for AcpBridgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Host(error) => Some(error),
            Self::Runtime(error) => Some(error),
            _ => None,
        }
    }
}

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

/// Binds ACP session mechanics to existing Agent Host authority.
pub(crate) struct AcpAgentHostBridge {
    mcp: AcpEditorMcpCredentials,
    sessions: BTreeMap<String, AttachedSession>,
}

impl AcpAgentHostBridge {
    pub(crate) fn new(mcp: AcpEditorMcpCredentials) -> Self {
        Self {
            mcp,
            sessions: BTreeMap::new(),
        }
    }

    /// Opens Ask with only the Editor read-only credential and no AgentRun identity.
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
        self.open_registered(registry, agent_id, binding, None)
    }

    /// Opens Build/Repair with the existing AgentRun-bound Editor MCP credential.
    ///
    /// Chat 1 transports must preserve `binding.gameengine_run_id` on MCP calls.
    /// The existing Editor MCP host then performs the same active-run check and
    /// short-lived `canonical_authoring` claim used by current external agents.
    pub(crate) fn open_run_session(
        &mut self,
        host: &mut AgentHost,
        registry: &mut dyn AcpAgentRegistry,
        agent_id: &str,
        gameengine_session_id: &str,
        run_id: &str,
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
        let acp_id = self.open_registered(registry, agent_id, binding, Some(run_id.to_owned()))?;
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
        expected_run_id: Option<String>,
    ) -> Result<String, AcpBridgeError> {
        let expected_session_id = binding.gameengine_session_id.clone();
        let (descriptor_id, identity, mut session) = {
            let runtime = registry
                .runtime_mut(agent_id)
                .ok_or_else(|| AcpBridgeError::AgentNotRegistered(agent_id.to_owned()))?;
            let descriptor = runtime.descriptor().clone();
            let session = runtime.open_session(binding)?;
            (descriptor.id, descriptor.runtime_identity, session)
        };
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
                acp_id
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
        let result = self.attached_mut(acp_id)?.session.send_prompt(prompt);
        if let Err(error) = result {
            return self.fail_runtime(host, acp_id, error);
        }
        self.attached_mut(acp_id)?.turn_finished = false;
        Ok(())
    }

    /// Polls at most one normalized ACP event without blocking the Editor thread.
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
        let run_id = self.attached(acp_id)?.run_id.clone();
        match run_id {
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
            self.resolve_acp(
                host,
                acp_id,
                AcpPermissionResolution {
                    request_id: request.request_id.clone(),
                    outcome: AcpPermissionOutcome::Cancelled,
                },
            )?;
            return Ok(AcpBridgePoll::AskEvent(AcpNormalizedEvent::ProtocolDiagnostic {
                message: format!(
                    "ACP permission `{}` denied: Ask is read-only.",
                    request.request_id
                ),
            }));
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
        match event {
            AcpNormalizedEvent::AgentMessage { text } => {
                host.record_event(run_id, AgentEventKind::AssistantMessage, text)?;
            }
            AcpNormalizedEvent::Progress { step, detail } => {
                host.record_semantic_progress(run_id, step, detail)?;
            }
            AcpNormalizedEvent::Plan { entries } => {
                host.record_semantic_progress(run_id, "ACP plan", entries.join("\n"))?;
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
            AcpNormalizedEvent::SessionInfo { title } => {
                host.record_semantic_progress(
                    run_id,
                    "ACP session",
                    title.unwrap_or_else(|| "session metadata updated".to_owned()),
                )?;
            }
            AcpNormalizedEvent::TurnFinished { stop_reason } => {
                host.record_semantic_progress(
                    run_id,
                    "ACP turn finished",
                    format!(
                        "Agent returned control with `{stop_reason}`; Agent Host completion remains authoritative."
                    ),
                )?;
                self.attached_mut(acp_id)?.turn_finished = true;
                if self.validation_ready(host, acp_id)? {
                    return Ok(AcpBridgePoll::ValidationReady {
                        run_id: run_id.to_owned(),
                    });
                }
            }
            AcpNormalizedEvent::ProtocolDiagnostic { message } => {
                host.record_event(
                    run_id,
                    AgentEventKind::ProviderOutput,
                    format!("ACP protocol diagnostic: {message}"),
                )?;
            }
        }
        Ok(AcpBridgePoll::Recorded {
            run_id: run_id.to_owned(),
            kind,
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
        if !host
            .run(run_id)?
            .proposal_snapshot
            .requested_capabilities
            .contains(&request.required_capability)
        {
            self.resolve_cancel(host, acp_id, &request.request_id)?;
            host.record_event(
                run_id,
                AgentEventKind::PermissionResolved,
                format!(
                    "ACP permission `{}` denied: `{}` was not declared by the authorized proposal.",
                    request.request_id,
                    request.required_capability.label()
                ),
            )?;
            return Err(AcpBridgeError::UndeclaredCapability(
                request.required_capability,
            ));
        }

        match host.check_permission(run_id, request.required_capability)? {
            PermissionCheck::Granted => {
                let Some(option_id) = allow_once_option(&request) else {
                    self.resolve_cancel(host, acp_id, &request.request_id)?;
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
                        "ACP permission `{}` granted for this request by GameEngine policy.",
                        request.request_id
                    ),
                )?;
                Ok(AcpBridgePoll::Recorded {
                    run_id: run_id.to_owned(),
                    kind: AgentEventKind::PermissionResolved,
                })
            }
            PermissionCheck::Denied => {
                self.resolve_cancel(host, acp_id, &request.request_id)?;
                host.record_event(
                    run_id,
                    AgentEventKind::PermissionResolved,
                    format!("ACP permission `{}` denied by GameEngine policy.", request.request_id),
                )?;
                Ok(AcpBridgePoll::Recorded {
                    run_id: run_id.to_owned(),
                    kind: AgentEventKind::PermissionResolved,
                })
            }
            PermissionCheck::RequiresApproval => {
                let request_id = request.request_id.clone();
                let title = request.title.clone();
                let capability = request.required_capability;
                let pending = &mut self.attached_mut(acp_id)?.pending_permissions;
                if pending.contains_key(&request_id) {
                    return Err(AcpBridgeError::DuplicatePermission(request_id));
                }
                pending.insert(request_id.clone(), request);
                Ok(AcpBridgePoll::PermissionRequired {
                    run_id: run_id.to_owned(),
                    request_id,
                    title,
                    capability,
                })
            }
        }
    }

    /// Resolves GameEngine policy first, then grants only the current ACP request.
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

        if scope == ApprovalScope::Deny {
            host.resolve_permission(&run_id, request.required_capability, scope)?;
            self.resolve_cancel(host, acp_id, request_id)?;
        } else {
            let Some(option_id) = allow_once_option(&request) else {
                self.resolve_cancel(host, acp_id, request_id)?;
                self.attached_mut(acp_id)?.pending_permissions.remove(request_id);
                return Err(AcpBridgeError::UnsafePermissionOptions(request_id.to_owned()));
            };
            host.resolve_permission(&run_id, request.required_capability, scope)?;
            if host.check_permission(&run_id, request.required_capability)? != PermissionCheck::Granted
            {
                return Err(AcpBridgeError::UnsafePermissionOptions(request_id.to_owned()));
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
        self.attached_mut(acp_id)?.pending_permissions.remove(request_id);
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
        match self.attached_mut(acp_id)?.session.resolve_permission(resolution) {
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

    /// Records only the two provider-owned completion gates.
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

    /// ACP turn completion is only validation-ready after provider-owned gates settle.
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

    /// Hands a ready run to Agent Host's existing validation path; ACP cannot complete it.
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

    /// Propagates GameEngine cancellation into ACP, then ends Agent Host authority.
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

    /// Drops ACP access after an authoritative AgentRun becomes terminal.
    pub(crate) fn reap_terminal_sessions(&mut self, host: &AgentHost) -> Result<(), AcpBridgeError> {
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
        let run_id = self.sessions.get(acp_id).and_then(|entry| entry.run_id.clone());
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

fn gate_satisfied(status: CompletionStatus) -> bool {
    matches!(status, CompletionStatus::Passed | CompletionStatus::NotApplicable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_agent_runtime::{
        AcpAgentDescriptor, AcpAgentRuntime, AcpCapabilities, AcpPermissionOption,
        AcpRuntimeRegistry,
    };
    use crate::agent_host::AgentProposal;
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Default)]
    struct StubState {
        events: VecDeque<AcpNormalizedEvent>,
        resolutions: Vec<AcpPermissionResolution>,
        next_error: Option<AcpRuntimeError>,
        cancelled: bool,
        closed: bool,
    }

    struct StubSession {
        id: String,
        binding: AcpSessionBinding,
        state: Arc<Mutex<StubState>>,
    }

    impl AcpAgentSession for StubSession {
        fn acp_session_id(&self) -> &str {
            &self.id
        }

        fn binding(&self) -> &AcpSessionBinding {
            &self.binding
        }

        fn send_prompt(&mut self, _prompt: &str) -> Result<(), AcpRuntimeError> {
            Ok(())
        }

        fn try_next_event(&mut self) -> Result<Option<AcpNormalizedEvent>, AcpRuntimeError> {
            let mut state = self.state.lock().expect("stub state");
            if let Some(error) = state.next_error.take() {
                return Err(error);
            }
            Ok(state.events.pop_front())
        }

        fn resolve_permission(
            &mut self,
            resolution: AcpPermissionResolution,
        ) -> Result<(), AcpRuntimeError> {
            self.state
                .lock()
                .expect("stub state")
                .resolutions
                .push(resolution);
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), AcpRuntimeError> {
            self.state.lock().expect("stub state").cancelled = true;
            Ok(())
        }

        fn close(&mut self) -> Result<(), AcpRuntimeError> {
            self.state.lock().expect("stub state").closed = true;
            Ok(())
        }
    }

    struct StubRuntime {
        descriptor: AcpAgentDescriptor,
        session_id: String,
        state: Arc<Mutex<StubState>>,
    }

    impl AcpAgentRuntime for StubRuntime {
        fn descriptor(&self) -> &AcpAgentDescriptor {
            &self.descriptor
        }

        fn open_session(
            &mut self,
            binding: AcpSessionBinding,
        ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError> {
            Ok(Box::new(StubSession {
                id: self.session_id.clone(),
                binding,
                state: Arc::clone(&self.state),
            }))
        }
    }

    fn registry(state: Arc<Mutex<StubState>>, session_id: &str) -> AcpRuntimeRegistry {
        let mut registry = AcpRuntimeRegistry::default();
        registry
            .register(Box::new(StubRuntime {
                descriptor: AcpAgentDescriptor {
                    id: "test.acp".to_owned(),
                    executable: OsString::from("test-acp"),
                    arguments: Vec::new(),
                    capabilities: AcpCapabilities::default(),
                    runtime_identity: AcpRuntimeIdentity::stable(
                        "test-acp",
                        Some("1.0".to_owned()),
                    ),
                },
                session_id: session_id.to_owned(),
                state,
            }))
            .expect("register stub");
        registry
    }

    fn temp_paths(label: &str) -> (PathBuf, PathBuf, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "gameengine-acp-bridge-{label}-{}-{unique}",
            std::process::id()
        ));
        let project = root.join("project");
        let storage = root.join("storage");
        fs::create_dir_all(&project).expect("project dir");
        (project, storage, root)
    }

    fn host_run(
        label: &str,
        capabilities: &[AgentCapability],
    ) -> (AgentHost, String, String, PathBuf) {
        let (project, storage, cleanup) = temp_paths(label);
        let mut host = AgentHost::open(project, storage).expect("host");
        let session = host.create_session("ACP test").expect("session");
        let mut proposal: AgentProposal = host.session(&session).expect("session").proposal.clone();
        proposal
            .requested_capabilities
            .extend(capabilities.iter().copied());
        let version = host.update_proposal(&session, proposal).expect("proposal");
        let run = host
            .start_run_authorized(&session, version, "ACP test")
            .expect("run");
        host.transition_run(&run, AgentRunState::Executing, "execute")
            .expect("executing");
        (host, session, run, cleanup)
    }

    fn bridge() -> AcpAgentHostBridge {
        AcpAgentHostBridge::new(
            AcpEditorMcpCredentials::new(
                "http://127.0.0.1:1/mcp",
                "run-secret",
                "read-secret",
            )
            .expect("credentials"),
        )
    }

    #[test]
    fn ask_permission_is_cancelled() {
        let (project, storage, cleanup) = temp_paths("ask");
        let mut host = AgentHost::open(project, storage).expect("host");
        let game_session = host.create_session("Ask").expect("session");
        let state = Arc::new(Mutex::new(StubState::default()));
        let mut registry = registry(Arc::clone(&state), "acp-ask");
        let mut bridge = bridge();
        let acp_id = bridge
            .open_ask_session(&host, &mut registry, "test.acp", &game_session)
            .expect("open Ask");
        state
            .lock()
            .expect("state")
            .events
            .push_back(permission_event(&acp_id));
        assert!(matches!(
            bridge.poll_session(&mut host, &acp_id).expect("poll"),
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::ProtocolDiagnostic { .. })
        ));
        assert!(matches!(
            state.lock().expect("state").resolutions.as_slice(),
            [AcpPermissionResolution {
                outcome: AcpPermissionOutcome::Cancelled,
                ..
            }]
        ));
        let _ = fs::remove_dir_all(cleanup);
    }

    #[test]
    fn turn_finished_is_not_completion_and_identity_is_recorded() {
        let (mut host, game_session, run, cleanup) = host_run("turn", &[]);
        let state = Arc::new(Mutex::new(StubState::default()));
        let mut registry = registry(Arc::clone(&state), "acp-run");
        let mut bridge = bridge();
        let acp_id = bridge
            .open_run_session(&mut host, &mut registry, "test.acp", &game_session, &run)
            .expect("open run");
        state
            .lock()
            .expect("state")
            .events
            .push_back(AcpNormalizedEvent::TurnFinished {
                stop_reason: "end_turn".to_owned(),
            });
        assert!(matches!(
            bridge.poll_session(&mut host, &acp_id).expect("poll"),
            AcpBridgePoll::Recorded {
                kind: AgentEventKind::SemanticProgress,
                ..
            }
        ));
        let run_state = host.run(&run).expect("run");
        assert_eq!(run_state.state, AgentRunState::Executing);
        assert!(run_state.events.iter().any(|event| {
            event.kind == AgentEventKind::ProviderOutput
                && event.message.contains("descriptor=test.acp")
                && event.message.contains("acp_session=acp-run")
        }));
        assert!(matches!(
            host.complete_run(&run),
            Err(AgentHostError::CompletionPending)
        ));
        let _ = fs::remove_dir_all(cleanup);
    }

    #[test]
    fn validation_ready_waits_for_canonical_authoring_claim() {
        let (mut host, game_session, run, cleanup) = host_run("validation-ready", &[]);
        let state = Arc::new(Mutex::new(StubState::default()));
        let mut registry = registry(Arc::clone(&state), "acp-validation");
        let mut bridge = bridge();
        let acp_id = bridge
            .open_run_session(&mut host, &mut registry, "test.acp", &game_session, &run)
            .expect("open run");
        assert!(!bridge
            .record_provider_completion_gate(
                &mut host,
                &acp_id,
                AcpProviderCompletionGate::AcceptanceCriteria,
                CompletionStatus::Passed,
                "accepted",
            )
            .expect("acceptance gate"));
        assert!(!bridge
            .record_provider_completion_gate(
                &mut host,
                &acp_id,
                AcpProviderCompletionGate::AuthoringValidation,
                CompletionStatus::Passed,
                "authoring valid",
            )
            .expect("authoring gate"));
        state
            .lock()
            .expect("state")
            .events
            .push_back(AcpNormalizedEvent::TurnFinished {
                stop_reason: "end_turn".to_owned(),
            });
        assert!(matches!(
            bridge.poll_session(&mut host, &acp_id).expect("poll"),
            AcpBridgePoll::ValidationReady { .. }
        ));
        host.acquire_work_claims(
            &run,
            [AgentWorkClaim::shared_resource("canonical_authoring")],
        )
        .expect("canonical authoring claim");
        assert!(!bridge
            .validation_ready(&host, &acp_id)
            .expect("blocked readiness"));
        host.release_work_claims(
            &run,
            [AgentWorkClaim::shared_resource("canonical_authoring")],
        )
        .expect("release canonical authoring");
        assert!(bridge
            .validation_ready(&host, &acp_id)
            .expect("ready after release"));
        let _ = fs::remove_dir_all(cleanup);
    }

    #[test]
    fn run_approval_is_projected_as_allow_once() {
        let (mut host, game_session, run, cleanup) =
            host_run("permission", &[AgentCapability::NetworkAccess]);
        let state = Arc::new(Mutex::new(StubState::default()));
        let mut registry = registry(Arc::clone(&state), "acp-permission");
        let mut bridge = bridge();
        let acp_id = bridge
            .open_run_session(&mut host, &mut registry, "test.acp", &game_session, &run)
            .expect("open run");
        state
            .lock()
            .expect("state")
            .events
            .push_back(permission_event(&acp_id));
        assert!(matches!(
            bridge.poll_session(&mut host, &acp_id).expect("poll"),
            AcpBridgePoll::PermissionRequired { .. }
        ));
        bridge
            .resolve_permission(&mut host, &acp_id, "permission-1", ApprovalScope::Run)
            .expect("resolve");
        assert!(matches!(
            state.lock().expect("state").resolutions.last(),
            Some(AcpPermissionResolution {
                outcome: AcpPermissionOutcome::SelectedOption(option),
                ..
            }) if option == "once"
        ));
        let _ = fs::remove_dir_all(cleanup);
    }

    #[test]
    fn cancellation_and_runtime_failure_end_authority() {
        let (mut host, game_session, run, cleanup) = host_run("cancel", &[]);
        let state = Arc::new(Mutex::new(StubState::default()));
        let mut registry = registry(Arc::clone(&state), "acp-cancel");
        let mut bridge = bridge();
        let acp_id = bridge
            .open_run_session(&mut host, &mut registry, "test.acp", &game_session, &run)
            .expect("open run");
        bridge.cancel_run(&mut host, &acp_id).expect("cancel");
        assert_eq!(host.run(&run).expect("run").state, AgentRunState::Cancelled);
        assert!(state.lock().expect("state").cancelled);
        assert!(state.lock().expect("state").closed);

        let (mut host, game_session, run, cleanup_failure) = host_run("failure", &[]);
        let state = Arc::new(Mutex::new(StubState::default()));
        let mut registry = registry(Arc::clone(&state), "acp-failure");
        let mut bridge = bridge();
        let acp_id = bridge
            .open_run_session(&mut host, &mut registry, "test.acp", &game_session, &run)
            .expect("open run");
        state.lock().expect("state").next_error =
            Some(AcpRuntimeError::Transport("child exited".to_owned()));
        assert!(matches!(
            bridge.poll_session(&mut host, &acp_id),
            Err(AcpBridgeError::Runtime(AcpRuntimeError::Transport(_)))
        ));
        assert_eq!(host.run(&run).expect("run").state, AgentRunState::Failed);
        assert!(state.lock().expect("state").closed);
        let _ = fs::remove_dir_all(cleanup);
        let _ = fs::remove_dir_all(cleanup_failure);
    }

    fn permission_event(acp_id: &str) -> AcpNormalizedEvent {
        AcpNormalizedEvent::PermissionRequest(AcpPermissionRequest {
            request_id: "permission-1".to_owned(),
            acp_session_id: acp_id.to_owned(),
            tool_call_id: "tool-1".to_owned(),
            title: "network".to_owned(),
            required_capability: AgentCapability::NetworkAccess,
            options: vec![
                AcpPermissionOption {
                    id: "always".to_owned(),
                    name: "Always".to_owned(),
                    kind: AcpPermissionOptionKind::AllowAlways,
                },
                AcpPermissionOption {
                    id: "once".to_owned(),
                    name: "Once".to_owned(),
                    kind: AcpPermissionOptionKind::AllowOnce,
                },
            ],
        })
    }
}
