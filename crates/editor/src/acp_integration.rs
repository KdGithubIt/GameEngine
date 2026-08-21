//! Editor-side ACP composition seam.
//!
//! The registry owns provider runtime entries, the bridge owns the binding to
//! Agent Host authority, and AI Studio owns only user selection and presentation.

use crate::acp_agent_host_bridge::{
    AcpAgentHostBridge, AcpBridgeError, AcpBridgePoll, AcpEditorMcpCredentials,
    AcpProviderCompletionGate,
};
use crate::acp_agent_runtime::{
    AcpAgentRegistry, AcpAgentRuntime, AcpAgentSession, AcpRuntimeError, AcpRuntimeRegistry,
    AcpSessionOpenRequest,
};
use crate::agent_host::{AgentHost, ApprovalScope, CompletionStatus};
use std::path::PathBuf;

pub(crate) struct AcpIntegration {
    registry: AcpRuntimeRegistry,
    bridge: AcpAgentHostBridge,
}

impl AcpIntegration {
    pub(crate) fn new(
        working_directory: PathBuf,
        endpoint: impl Into<String>,
        run_bound_token: impl Into<String>,
        read_only_token: impl Into<String>,
    ) -> Result<Self, AcpBridgeError> {
        let credentials = AcpEditorMcpCredentials::new(endpoint, run_bound_token, read_only_token)?;
        Ok(Self {
            registry: AcpRuntimeRegistry::default(),
            bridge: AcpAgentHostBridge::new(credentials, working_directory)?,
        })
    }

    pub(crate) fn replace(
        &mut self,
        runtime: Box<dyn AcpAgentRuntime>,
    ) -> Result<(), AcpRuntimeError> {
        self.registry.replace(runtime)
    }

    pub(crate) fn registry(&self) -> &dyn AcpAgentRegistry {
        &self.registry
    }

    pub(crate) fn prepare_ask_session(&self, host: &AgentHost, gameengine_session_id: &str) -> Result<AcpSessionOpenRequest, AcpBridgeError> {
        self.bridge.prepare_ask_session(host, gameengine_session_id)
    }

    pub(crate) fn prepare_run_session(&self, host: &AgentHost, gameengine_session_id: &str, run_id: &str, working_directory: PathBuf) -> Result<AcpSessionOpenRequest, AcpBridgeError> {
        self.bridge.prepare_run_session(host, gameengine_session_id, run_id, working_directory)
    }

    pub(crate) fn attach_opened_ask_session(&mut self, host: &AgentHost, descriptor_id: String, session: Box<dyn AcpAgentSession>, expected_session_id: &str) -> Result<String, AcpBridgeError> {
        self.bridge.attach_opened_ask_session(host, descriptor_id, session, expected_session_id)
    }

    pub(crate) fn attach_opened_run_session(&mut self, host: &mut AgentHost, descriptor_id: String, session: Box<dyn AcpAgentSession>, expected_session_id: &str, expected_run_id: &str) -> Result<String, AcpBridgeError> {
        self.bridge.attach_opened_run_session(host, descriptor_id, session, expected_session_id, expected_run_id)
    }

    pub(crate) fn open_ask_session(
        &mut self,
        host: &AgentHost,
        agent_id: &str,
        gameengine_session_id: &str,
    ) -> Result<String, AcpBridgeError> {
        self.bridge
            .open_ask_session(host, &mut self.registry, agent_id, gameengine_session_id)
    }

    pub(crate) fn open_run_session(
        &mut self,
        host: &mut AgentHost,
        agent_id: &str,
        gameengine_session_id: &str,
        run_id: &str,
        working_directory: PathBuf,
    ) -> Result<String, AcpBridgeError> {
        self.bridge.open_run_session(
            host,
            &mut self.registry,
            agent_id,
            gameengine_session_id,
            run_id,
            working_directory,
        )
    }

    pub(crate) fn send_prompt(
        &mut self,
        host: &mut AgentHost,
        acp_session_id: &str,
        prompt: &str,
    ) -> Result<(), AcpBridgeError> {
        self.bridge.send_prompt(host, acp_session_id, prompt)
    }

    pub(crate) fn poll(
        &mut self,
        host: &mut AgentHost,
        acp_session_id: &str,
    ) -> Result<AcpBridgePoll, AcpBridgeError> {
        self.bridge.poll_session(host, acp_session_id)
    }

    pub(crate) fn resolve_permission(
        &mut self,
        host: &mut AgentHost,
        acp_session_id: &str,
        request_id: &str,
        scope: ApprovalScope,
    ) -> Result<(), AcpBridgeError> {
        self.bridge
            .resolve_permission(host, acp_session_id, request_id, scope)
    }

    pub(crate) fn record_provider_completion_gate(
        &mut self,
        host: &mut AgentHost,
        acp_session_id: &str,
        gate: AcpProviderCompletionGate,
        status: CompletionStatus,
        message: impl Into<String>,
    ) -> Result<bool, AcpBridgeError> {
        self.bridge
            .record_provider_completion_gate(host, acp_session_id, gate, status, message)
    }

    pub(crate) fn begin_managed_validation(
        &mut self,
        host: &mut AgentHost,
        acp_session_id: &str,
        code_changes_present: bool,
    ) -> Result<(), AcpBridgeError> {
        self.bridge
            .begin_managed_validation(host, acp_session_id, code_changes_present)
    }

    pub(crate) fn cancel_run(
        &mut self,
        host: &mut AgentHost,
        acp_session_id: &str,
    ) -> Result<(), AcpBridgeError> {
        self.bridge.cancel_run(host, acp_session_id)
    }

    pub(crate) fn runtime_identity(
        &self,
        acp_session_id: &str,
    ) -> Result<crate::acp_agent_runtime::AcpRuntimeIdentity, AcpBridgeError> {
        self.bridge.runtime_identity(acp_session_id).cloned()
    }

    pub(crate) fn cancel_session(&mut self, acp_session_id: &str) -> Result<(), AcpBridgeError> {
        self.bridge.cancel_session(acp_session_id)
    }

    pub(crate) fn close_session(&mut self, acp_session_id: &str) -> Result<(), AcpBridgeError> {
        self.bridge.close_session(acp_session_id)
    }

    pub(crate) fn reap_terminal_sessions(
        &mut self,
        host: &AgentHost,
    ) -> Result<(), AcpBridgeError> {
        self.bridge.reap_terminal_sessions(host)
    }
}
