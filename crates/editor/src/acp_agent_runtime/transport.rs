use super::{
    validate_descriptor, AcpAgentDescriptor, AcpAgentRuntime, AcpAgentSession, AcpCapabilities,
    AcpNormalizedEvent, AcpPermissionOption, AcpPermissionOptionKind, AcpPermissionOutcome,
    AcpPermissionRequest, AcpPermissionResolution, AcpRuntimeError, AcpRuntimeIdentity,
    AcpSessionBinding, AcpSessionOpenMode, AcpSessionOpenRequest, AcpToolCallStatus,
    ACP_STABLE_PROTOCOL_VERSION,
};
use crate::agent_host::AgentCapability;
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        AgentCapabilities, CancelNotification, CloseSessionRequest, ContentBlock, ContentChunk,
        HttpHeader, Implementation, InitializeRequest, LoadSessionRequest, McpServer, McpServerHttp,
        NewSessionRequest, PermissionOptionKind, PromptRequest, RequestPermissionOutcome,
        RequestPermissionRequest, RequestPermissionResponse, ResumeSessionRequest,
        SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate,
        SetSessionConfigOptionRequest, StopReason, ToolCallStatus, ToolKind,
    },
};
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client};
use futures::{
    StreamExt,
    channel::{mpsc as async_mpsc, oneshot},
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};

const GAMEENGINE_ACP_CLIENT_NAME: &str = "gameengine-ai-studio";
const GAMEENGINE_MCP_SERVER_NAME: &str = "gameengine_editor";
const GAMEENGINE_AGENT_RUN_ID_HEADER: &str = "X-GameEngine-Agent-Run-Id";

#[derive(Debug)]
enum RuntimeCommand {
    Prompt(String),
    SetConfigOption {
        option_id: String,
        value: String,
        response: mpsc::SyncSender<Result<(), AcpRuntimeError>>,
    },
    Cancel,
    Close,
}

#[derive(Debug)]
struct OpenedSession {
    acp_session_id: String,
    capabilities: AcpCapabilities,
    runtime_identity: AcpRuntimeIdentity,
    config_option_ids: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct TrackedToolCall {
    title: String,
    kind: ToolKind,
    status: AcpToolCallStatus,
}

type PendingPermissions = Arc<Mutex<HashMap<String, oneshot::Sender<AcpPermissionOutcome>>>>;
type ToolCalls = Arc<Mutex<HashMap<String, TrackedToolCall>>>;

/// Provider-neutral ACP subprocess runtime using the official Rust SDK.
pub(crate) struct AcpProcessRuntime {
    descriptor: AcpAgentDescriptor,
}

impl AcpProcessRuntime {
    pub(crate) fn new(descriptor: AcpAgentDescriptor) -> Result<Self, AcpRuntimeError> {
        validate_descriptor(&descriptor)?;
        if descriptor.runtime_identity.protocol_version != ACP_STABLE_PROTOCOL_VERSION {
            return Err(AcpRuntimeError::Unsupported(format!(
                "stable ACP core transport supports protocol v{ACP_STABLE_PROTOCOL_VERSION} only"
            )));
        }
        if descriptor.capabilities.mcp_over_acp {
            return Err(AcpRuntimeError::Unsupported(
                "stable ACP transport does not enable unstable MCP-over-ACP".to_owned(),
            ));
        }
        if !descriptor.capabilities.extensions.is_empty() {
            return Err(AcpRuntimeError::Unsupported(
                "ACP extension requirements are not enabled by the stable core transport".to_owned(),
            ));
        }
        Ok(Self { descriptor })
    }
}

impl AcpAgentRuntime for AcpProcessRuntime {
    fn descriptor(&self) -> &AcpAgentDescriptor {
        &self.descriptor
    }

    fn open_session(
        &mut self,
        request: AcpSessionOpenRequest,
    ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError> {
        request.validate()?;

        let config = build_agent_config(&self.descriptor)?;
        let descriptor = self.descriptor.clone();
        let binding = request.binding.clone();
        let (command_tx, command_rx) = async_mpsc::unbounded();
        let (event_tx, event_rx) = mpsc::channel();
        let (open_tx, open_rx) = mpsc::sync_channel(1);
        let pending_permissions: PendingPermissions = Arc::new(Mutex::new(HashMap::new()));
        let terminal_error = Arc::new(Mutex::new(None));
        let closing = Arc::new(AtomicBool::new(false));
        let prompt_in_flight = Arc::new(AtomicBool::new(false));

        let thread_pending_permissions = Arc::clone(&pending_permissions);
        let thread_terminal_error = Arc::clone(&terminal_error);
        let thread_closing = Arc::clone(&closing);
        let thread_prompt_in_flight = Arc::clone(&prompt_in_flight);
        let thread_event_tx = event_tx.clone();
        let thread_name = format!("acp-{}", descriptor.id);

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let result = pollster::block_on(run_connection(
                    descriptor,
                    request,
                    config,
                    command_rx,
                    thread_event_tx.clone(),
                    open_tx,
                    thread_pending_permissions,
                    thread_closing.clone(),
                    thread_prompt_in_flight,
                ));

                if let Err(error) = result {
                    if let Ok(mut terminal) = thread_terminal_error.lock() {
                        *terminal = Some(error.clone());
                    }
                    if !thread_closing.load(Ordering::SeqCst) {
                        let _ = thread_event_tx.send(AcpNormalizedEvent::ProtocolDiagnostic {
                            message: error.to_string(),
                        });
                    }
                }
            })
            .map_err(|error| {
                AcpRuntimeError::Transport(format!("failed to spawn ACP runtime thread: {error}"))
            })?;

        let opened = open_rx.recv().map_err(|_| {
            terminal_error
                .lock()
                .ok()
                .and_then(|error| error.clone())
                .unwrap_or_else(|| {
                    AcpRuntimeError::Transport(
                        "ACP connection ended before session negotiation completed".to_owned(),
                    )
                })
        })??;

        Ok(Box::new(AcpProcessSession {
            acp_session_id: opened.acp_session_id,
            binding,
            capabilities: opened.capabilities,
            runtime_identity: opened.runtime_identity,
            config_option_ids: opened.config_option_ids,
            command_tx,
            event_rx,
            pending_permissions,
            terminal_error,
            closing,
            prompt_in_flight,
        }))
    }
}

struct AcpProcessSession {
    acp_session_id: String,
    binding: AcpSessionBinding,
    capabilities: AcpCapabilities,
    runtime_identity: AcpRuntimeIdentity,
    config_option_ids: BTreeSet<String>,
    command_tx: async_mpsc::UnboundedSender<RuntimeCommand>,
    event_rx: mpsc::Receiver<AcpNormalizedEvent>,
    pending_permissions: PendingPermissions,
    terminal_error: Arc<Mutex<Option<AcpRuntimeError>>>,
    closing: Arc<AtomicBool>,
    prompt_in_flight: Arc<AtomicBool>,
}

impl AcpProcessSession {
    fn send_command(&mut self, command: RuntimeCommand) -> Result<(), AcpRuntimeError> {
        self.ensure_connected()?;
        self.command_tx
            .unbounded_send(command)
            .map_err(|_| self.connection_error("ACP command channel is closed"))
    }

    fn ensure_connected(&self) -> Result<(), AcpRuntimeError> {
        let terminal = self.terminal_error.lock().map_err(|_| {
            AcpRuntimeError::Transport("ACP terminal state lock is poisoned".to_owned())
        })?;
        match terminal.as_ref() {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn connection_error(&self, fallback: &str) -> AcpRuntimeError {
        self.terminal_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
            .unwrap_or_else(|| AcpRuntimeError::Transport(fallback.to_owned()))
    }

    fn cancel_pending_permissions(&self) -> Result<(), AcpRuntimeError> {
        let mut pending = self.pending_permissions.lock().map_err(|_| {
            AcpRuntimeError::Transport("ACP permission state lock is poisoned".to_owned())
        })?;
        for (_, sender) in pending.drain() {
            let _ = sender.send(AcpPermissionOutcome::Cancelled);
        }
        Ok(())
    }
}

impl Drop for AcpProcessSession {
    fn drop(&mut self) {
        if !self.closing.swap(true, Ordering::SeqCst) {
            let _ = self.cancel_pending_permissions();
            let _ = self.command_tx.unbounded_send(RuntimeCommand::Close);
        }
    }
}

impl AcpAgentSession for AcpProcessSession {
    fn acp_session_id(&self) -> &str {
        &self.acp_session_id
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

    fn set_session_config_option(
        &mut self,
        option_id: &str,
        value: &str,
    ) -> Result<(), AcpRuntimeError> {
        self.ensure_connected()?;
        if option_id.trim().is_empty() || option_id.trim() != option_id {
            return Err(AcpRuntimeError::Protocol(
                "ACP session config option ID must be non-empty and unpadded".to_owned(),
            ));
        }
        if value.trim().is_empty() || value.trim() != value {
            return Err(AcpRuntimeError::Protocol(
                "ACP session config value must be non-empty and unpadded".to_owned(),
            ));
        }
        if !self.capabilities.session_config_options
            || !self.config_option_ids.contains(option_id)
        {
            return Err(AcpRuntimeError::Unsupported(format!(
                "agent did not advertise ACP session config option `{option_id}`"
            )));
        }
        let (response_tx, response_rx) = mpsc::sync_channel(1);
        self.send_command(RuntimeCommand::SetConfigOption {
            option_id: option_id.to_owned(),
            value: value.to_owned(),
            response: response_tx,
        })?;
        response_rx.recv().map_err(|_| {
            self.connection_error("ACP session config response channel is closed")
        })?
    }

    fn send_prompt(&mut self, prompt: &str) -> Result<(), AcpRuntimeError> {
        if prompt.trim().is_empty() {
            return Err(AcpRuntimeError::Protocol(
                "ACP prompt must not be empty".to_owned(),
            ));
        }
        if self
            .prompt_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(AcpRuntimeError::Protocol(
                "ACP session already has a prompt in flight".to_owned(),
            ));
        }
        let result = self.send_command(RuntimeCommand::Prompt(prompt.to_owned()));
        if result.is_err() {
            self.prompt_in_flight.store(false, Ordering::SeqCst);
        }
        result
    }

    fn try_next_event(&mut self) -> Result<Option<AcpNormalizedEvent>, AcpRuntimeError> {
        match self.event_rx.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::TryRecvError::Empty) => {
                self.ensure_connected()?;
                Ok(None)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(self.connection_error("ACP event channel is closed"))
            }
        }
    }

    fn resolve_permission(
        &mut self,
        resolution: AcpPermissionResolution,
    ) -> Result<(), AcpRuntimeError> {
        self.ensure_connected()?;
        let sender = self
            .pending_permissions
            .lock()
            .map_err(|_| {
                AcpRuntimeError::Transport("ACP permission state lock is poisoned".to_owned())
            })?
            .remove(&resolution.request_id)
            .ok_or_else(|| {
                AcpRuntimeError::Protocol(format!(
                    "ACP permission request `{}` is not pending",
                    resolution.request_id
                ))
            })?;
        sender.send(resolution.outcome).map_err(|_| {
            AcpRuntimeError::Transport(format!(
                "ACP permission request `{}` is no longer awaiting a response",
                resolution.request_id
            ))
        })
    }

    fn cancel(&mut self) -> Result<(), AcpRuntimeError> {
        self.cancel_pending_permissions()?;
        self.send_command(RuntimeCommand::Cancel)
    }

    fn close(&mut self) -> Result<(), AcpRuntimeError> {
        if self.closing.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.cancel_pending_permissions()?;
        self.command_tx
            .unbounded_send(RuntimeCommand::Close)
            .map_err(|_| self.connection_error("ACP command channel is closed"))
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_connection(
    descriptor: AcpAgentDescriptor,
    request: AcpSessionOpenRequest,
    config: AcpAgentConfig,
    command_rx: async_mpsc::UnboundedReceiver<RuntimeCommand>,
    event_tx: mpsc::Sender<AcpNormalizedEvent>,
    open_tx: mpsc::SyncSender<Result<OpenedSession, AcpRuntimeError>>,
    pending_permissions: PendingPermissions,
    closing: Arc<AtomicBool>,
    prompt_in_flight: Arc<AtomicBool>,
) -> Result<(), AcpRuntimeError> {
    let active_session_id = Arc::new(Mutex::new(None::<String>));
    let tool_calls: ToolCalls = Arc::new(Mutex::new(HashMap::new()));
    let permission_sequence = Arc::new(AtomicU64::new(1));

    let notification_session_id = Arc::clone(&active_session_id);
    let notification_tools = Arc::clone(&tool_calls);
    let notification_events = event_tx.clone();

    let permission_session_id = Arc::clone(&active_session_id);
    let permission_tools = Arc::clone(&tool_calls);
    let permission_events = event_tx.clone();
    let permission_pending = Arc::clone(&pending_permissions);
    let permission_sequence_clone = Arc::clone(&permission_sequence);

    let close_events = event_tx.clone();
    let close_flag = Arc::clone(&closing);
    let session_open_tx = open_tx.clone();

    let transport = AcpAgent::new(config);
    let result = Client
        .builder()
        .name(GAMEENGINE_ACP_CLIENT_NAME)
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                handle_session_notification(
                    notification,
                    &notification_session_id,
                    &notification_tools,
                    &notification_events,
                )
                .map_err(sdk_internal_error)
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let response = handle_permission_request(
                    request,
                    &permission_session_id,
                    &permission_tools,
                    &permission_events,
                    &permission_pending,
                    &permission_sequence_clone,
                )
                .await
                .map_err(sdk_internal_error)?;
                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_close(async move |_connection| {
            if !close_flag.load(Ordering::SeqCst) {
                let _ = close_events.send(AcpNormalizedEvent::ProtocolDiagnostic {
                    message: "ACP agent stdout reached EOF".to_owned(),
                });
            }
            Ok(())
        })
        .connect_with(transport, async move |connection| {
            let initialize = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1).client_info(Implementation::new(
                        GAMEENGINE_ACP_CLIENT_NAME,
                        env!("CARGO_PKG_VERSION"),
                    )),
                )
                .block_task()
                .await?;

            if initialize.protocol_version != ProtocolVersion::V1 {
                return Err(agent_client_protocol::Error::invalid_request().data(format!(
                    "GameEngine supports ACP protocol v{ACP_STABLE_PROTOCOL_VERSION}, but agent negotiated {:?}",
                    initialize.protocol_version
                )));
            }

            let mut negotiated = normalize_capabilities(&initialize.agent_capabilities);
            let mut initialize_requirements = descriptor.capabilities.clone();
            initialize_requirements.session_config_options = false;
            validate_required_capabilities(&initialize_requirements, &negotiated)
                .map_err(sdk_internal_error)?;
            let runtime_identity = normalize_runtime_identity(&initialize)
                .and_then(|identity| validate_runtime_identity(&descriptor.runtime_identity, identity))
                .map_err(sdk_internal_error)?;

            if !negotiated.mcp_http {
                return Err(agent_client_protocol::Error::invalid_request().data(
                    "ACP agent did not negotiate HTTP MCP support required by the GameEngine MCP binding",
                ));
            }
            let mcp_servers = vec![mcp_server(&request.binding)];
            if let AcpSessionOpenMode::Load { acp_session_id }
            | AcpSessionOpenMode::Resume { acp_session_id } = &request.mode
            {
                *active_session_id
                    .lock()
                    .map_err(|_| agent_client_protocol::Error::internal_error().data(
                        "ACP session state lock is poisoned",
                    ))? = Some(acp_session_id.clone());
            }
            let (session_id, config_option_ids) = open_acp_session(
                &connection,
                &request,
                mcp_servers,
                &negotiated,
            )
            .await
            .map_err(sdk_internal_error)?;
            negotiated.session_config_options |= !config_option_ids.is_empty();
            validate_required_capabilities(&descriptor.capabilities, &negotiated)
                .map_err(sdk_internal_error)?;

            *active_session_id
                .lock()
                .map_err(|_| agent_client_protocol::Error::internal_error().data("ACP session state lock is poisoned"))? =
                Some(session_id.clone());

            session_open_tx
                .send(Ok(OpenedSession {
                    acp_session_id: session_id.clone(),
                    capabilities: negotiated.clone(),
                    runtime_identity,
                    config_option_ids,
                }))
                .map_err(|_| agent_client_protocol::Error::internal_error().data("ACP session opener was dropped"))?;

            run_command_loop(
                connection,
                session_id,
                negotiated,
                command_rx,
                event_tx,
                prompt_in_flight,
            )
            .await
        })
        .await;

    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            let runtime_error = classify_sdk_connection_error(error);
            let _ = open_tx.send(Err(runtime_error.clone()));
            Err(runtime_error)
        }
    }
}

async fn run_command_loop(
    connection: agent_client_protocol::ConnectionTo<agent_client_protocol::Agent>,
    session_id: String,
    capabilities: AcpCapabilities,
    mut command_rx: async_mpsc::UnboundedReceiver<RuntimeCommand>,
    event_tx: mpsc::Sender<AcpNormalizedEvent>,
    prompt_in_flight: Arc<AtomicBool>,
) -> agent_client_protocol::Result<()> {
    while let Some(command) = command_rx.next().await {
        match command {
            RuntimeCommand::Prompt(prompt) => {
                let prompt_events = event_tx.clone();
                let prompt_state = Arc::clone(&prompt_in_flight);
                connection
                    .send_request(PromptRequest::new(
                        SessionId::new(session_id.clone()),
                        vec![prompt.into()],
                    ))
                    .on_receiving_result(async move |result| {
                        prompt_state.store(false, Ordering::SeqCst);
                        match result {
                            Ok(response) => {
                                send_event(
                                    &prompt_events,
                                    AcpNormalizedEvent::TurnFinished {
                                        stop_reason: stop_reason_label(response.stop_reason).to_owned(),
                                    },
                                )?;
                            }
                            Err(error) => {
                                send_event(
                                    &prompt_events,
                                    AcpNormalizedEvent::ProtocolDiagnostic {
                                        message: format!("ACP session/prompt failed: {error}"),
                                    },
                                )?;
                            }
                        }
                        Ok(())
                    })?;
            }
            RuntimeCommand::SetConfigOption {
                option_id,
                value,
                response,
            } => {
                let result = connection
                    .send_request(SetSessionConfigOptionRequest::new(
                        SessionId::new(session_id.clone()),
                        option_id,
                        value.as_str(),
                    ))
                    .block_task()
                    .await
                    .map(|_| ())
                    .map_err(sdk_transport_error);
                let _ = response.send(result);
            }
            RuntimeCommand::Cancel => {
                connection.send_notification(CancelNotification::new(SessionId::new(
                    session_id.clone(),
                )))?;
            }
            RuntimeCommand::Close => {
                if capabilities.session_close {
                    let _ = connection
                        .send_request(CloseSessionRequest::new(SessionId::new(session_id.clone())))
                        .block_task()
                        .await?;
                }
                break;
            }
        }
    }
    Ok(())
}

async fn open_acp_session(
    connection: &agent_client_protocol::ConnectionTo<agent_client_protocol::Agent>,
    request: &AcpSessionOpenRequest,
    mcp_servers: Vec<McpServer>,
    capabilities: &AcpCapabilities,
) -> Result<(String, BTreeSet<String>), AcpRuntimeError> {
    match &request.mode {
        AcpSessionOpenMode::New => {
            let response = connection
                .send_request(
                    NewSessionRequest::new(request.working_directory.clone())
                        .mcp_servers(mcp_servers),
                )
                .block_task()
                .await
                .map_err(sdk_transport_error)?;
            Ok((
                response.session_id.to_string(),
                config_option_ids(response.config_options.as_deref()),
            ))
        }
        AcpSessionOpenMode::Load { acp_session_id } => {
            if !capabilities.session_load {
                return Err(AcpRuntimeError::Unsupported(
                    "agent did not negotiate session/load".to_owned(),
                ));
            }
            let response = connection
                .send_request(
                    LoadSessionRequest::new(
                        SessionId::new(acp_session_id.clone()),
                        request.working_directory.clone(),
                    )
                    .mcp_servers(mcp_servers),
                )
                .block_task()
                .await
                .map_err(sdk_transport_error)?;
            Ok((
                acp_session_id.clone(),
                config_option_ids(response.config_options.as_deref()),
            ))
        }
        AcpSessionOpenMode::Resume { acp_session_id } => {
            if !capabilities.session_resume {
                return Err(AcpRuntimeError::Unsupported(
                    "agent did not negotiate session/resume".to_owned(),
                ));
            }
            let response = connection
                .send_request(
                    ResumeSessionRequest::new(
                        SessionId::new(acp_session_id.clone()),
                        request.working_directory.clone(),
                    )
                    .mcp_servers(mcp_servers),
                )
                .block_task()
                .await
                .map_err(sdk_transport_error)?;
            Ok((
                acp_session_id.clone(),
                config_option_ids(response.config_options.as_deref()),
            ))
        }
    }
}

fn config_option_ids(
    options: Option<&[agent_client_protocol::schema::v1::SessionConfigOption]>,
) -> BTreeSet<String> {
    options
        .unwrap_or_default()
        .iter()
        .map(|option| option.id.to_string())
        .collect()
}

fn build_agent_config(descriptor: &AcpAgentDescriptor) -> Result<AcpAgentConfig, AcpRuntimeError> {
    let arguments = descriptor
        .arguments
        .iter()
        .map(|argument| {
            argument.to_str().map(str::to_owned).ok_or_else(|| {
                AcpRuntimeError::InvalidDescriptor(
                    "ACP subprocess arguments must be valid UTF-8 for the official SDK".to_owned(),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let environment = descriptor
        .environment
        .iter()
        .map(|(name, value)| {
            let name = name.to_str().ok_or_else(|| {
                AcpRuntimeError::InvalidDescriptor(
                    "ACP subprocess environment names must be valid UTF-8 for the official SDK"
                        .to_owned(),
                )
            })?;
            let value = value.to_str().ok_or_else(|| {
                AcpRuntimeError::InvalidDescriptor(
                    "ACP subprocess environment values must be valid UTF-8 for the official SDK"
                        .to_owned(),
                )
            })?;
            Ok((name.to_owned(), value.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>, AcpRuntimeError>>()?;

    Ok(AcpAgentConfig::new(PathBuf::from(
        descriptor.executable.clone(),
    ))
    .args(arguments)
    .envs(environment))
}

fn normalize_capabilities(capabilities: &AgentCapabilities) -> AcpCapabilities {
    AcpCapabilities {
        session_load: capabilities.load_session,
        session_resume: capabilities.session_capabilities.resume.is_some(),
        session_list: capabilities.session_capabilities.list.is_some(),
        session_close: capabilities.session_capabilities.close.is_some(),
        session_config_options: false,
        mcp_http: capabilities.mcp_capabilities.http,
        mcp_sse: capabilities.mcp_capabilities.sse,
        mcp_over_acp: false,
        extensions: Default::default(),
    }
}

fn validate_required_capabilities(
    required: &AcpCapabilities,
    negotiated: &AcpCapabilities,
) -> Result<(), AcpRuntimeError> {
    let checks = [
        (required.session_load, negotiated.session_load, "session/load"),
        (
            required.session_resume,
            negotiated.session_resume,
            "session/resume",
        ),
        (required.session_list, negotiated.session_list, "session/list"),
        (required.session_close, negotiated.session_close, "session/close"),
        (
            required.session_config_options,
            negotiated.session_config_options,
            "session configuration options",
        ),
        (required.mcp_http, negotiated.mcp_http, "HTTP MCP"),
        (required.mcp_sse, negotiated.mcp_sse, "SSE MCP"),
        (
            required.mcp_over_acp,
            negotiated.mcp_over_acp,
            "MCP-over-ACP",
        ),
    ];
    if let Some((_, _, capability)) = checks
        .into_iter()
        .find(|(required, negotiated, _)| *required && !*negotiated)
    {
        return Err(AcpRuntimeError::Unsupported(format!(
            "agent did not negotiate required ACP capability `{capability}`"
        )));
    }
    if !required.extensions.is_subset(&negotiated.extensions) {
        return Err(AcpRuntimeError::Unsupported(
            "agent did not negotiate all required ACP extensions".to_owned(),
        ));
    }
    Ok(())
}

fn normalize_runtime_identity(
    response: &agent_client_protocol::schema::v1::InitializeResponse,
) -> Result<AcpRuntimeIdentity, AcpRuntimeError> {
    let info = response.agent_info.as_ref().ok_or_else(|| {
        AcpRuntimeError::Protocol(
            "ACP initialize response omitted agentInfo required for runtime identity verification"
                .to_owned(),
        )
    })?;
    Ok(AcpRuntimeIdentity {
        protocol_version: ACP_STABLE_PROTOCOL_VERSION,
        agent_name: info.name.clone(),
        agent_version: Some(info.version.clone()),
    })
}

fn validate_runtime_identity(
    expected: &AcpRuntimeIdentity,
    actual: AcpRuntimeIdentity,
) -> Result<AcpRuntimeIdentity, AcpRuntimeError> {
    if expected.protocol_version != actual.protocol_version || expected.agent_name != actual.agent_name {
        return Err(AcpRuntimeError::Protocol(format!(
            "ACP runtime identity mismatch: expected protocol v{} agent `{}`, got protocol v{} agent `{}`",
            expected.protocol_version,
            expected.agent_name,
            actual.protocol_version,
            actual.agent_name
        )));
    }
    if let Some(expected_version) = expected.agent_version.as_deref()
        && actual.agent_version.as_deref() != Some(expected_version)
    {
        return Err(AcpRuntimeError::Protocol(format!(
            "ACP agent version mismatch: expected `{expected_version}`, got `{}`",
            actual.agent_version.as_deref().unwrap_or("<missing>")
        )));
    }
    Ok(actual)
}

fn mcp_server(binding: &AcpSessionBinding) -> McpServer {
    let mut headers = vec![HttpHeader::new(
        "Authorization",
        format!("Bearer {}", binding.mcp.authorization_token()),
    )];
    if binding.mcp.access == crate::acp_agent_runtime::AcpMcpAccessLevel::AgentRunBoundReadWrite {
        if let Some(run_id) = binding.gameengine_run_id.as_deref() {
            headers.push(HttpHeader::new(GAMEENGINE_AGENT_RUN_ID_HEADER, run_id));
        }
    }
    McpServer::Http(
        McpServerHttp::new(GAMEENGINE_MCP_SERVER_NAME, binding.mcp.endpoint()).headers(headers),
    )
}

fn handle_session_notification(
    notification: SessionNotification,
    active_session_id: &Arc<Mutex<Option<String>>>,
    tool_calls: &ToolCalls,
    events: &mpsc::Sender<AcpNormalizedEvent>,
) -> Result<(), AcpRuntimeError> {
    let session_id = notification.session_id.to_string();
    let expected = active_session_id
        .lock()
        .map_err(|_| AcpRuntimeError::Transport("ACP session state lock is poisoned".to_owned()))?
        .clone();
    if expected.as_deref() != Some(session_id.as_str()) {
        return Err(AcpRuntimeError::Protocol(format!(
            "received ACP update for unexpected session `{session_id}`"
        )));
    }

    match notification.update {
        SessionUpdate::AgentMessageChunk(ContentChunk {
            content: ContentBlock::Text(text),
            ..
        }) => send_normalized(events, AcpNormalizedEvent::AgentMessage { text: text.text }),
        SessionUpdate::AgentThoughtChunk(ContentChunk {
            content: ContentBlock::Text(text),
            ..
        }) => send_normalized(
            events,
            AcpNormalizedEvent::Progress {
                step: "agent_thought".to_owned(),
                detail: text.text,
            },
        ),
        SessionUpdate::UserMessageChunk(ContentChunk {
            content: ContentBlock::Text(text),
            ..
        }) => send_normalized(
            events,
            AcpNormalizedEvent::Progress {
                step: "user_message_echo".to_owned(),
                detail: text.text,
            },
        ),
        SessionUpdate::ToolCall(tool_call) => {
            let tool_call_id = tool_call.tool_call_id.to_string();
            let status = normalize_tool_status(tool_call.status);
            tool_calls
                .lock()
                .map_err(|_| {
                    AcpRuntimeError::Transport("ACP tool state lock is poisoned".to_owned())
                })?
                .insert(
                    tool_call_id.clone(),
                    TrackedToolCall {
                        title: tool_call.title.clone(),
                        kind: tool_call.kind,
                        status,
                    },
                );
            send_normalized(
                events,
                AcpNormalizedEvent::ToolCall {
                    tool_call_id,
                    title: tool_call.title,
                    status,
                },
            )
        }
        SessionUpdate::ToolCallUpdate(update) => {
            let tool_call_id = update.tool_call_id.to_string();
            let mut tools = tool_calls.lock().map_err(|_| {
                AcpRuntimeError::Transport("ACP tool state lock is poisoned".to_owned())
            })?;
            let tracked = tools.entry(tool_call_id.clone()).or_insert_with(|| TrackedToolCall {
                title: "ACP tool call".to_owned(),
                kind: ToolKind::Other,
                status: AcpToolCallStatus::Pending,
            });
            if let Some(title) = update.fields.title {
                tracked.title = title;
            }
            if let Some(kind) = update.fields.kind {
                tracked.kind = kind;
            }
            if let Some(status) = update.fields.status {
                tracked.status = normalize_tool_status(status);
            }
            let event = AcpNormalizedEvent::ToolCall {
                tool_call_id,
                title: tracked.title.clone(),
                status: tracked.status,
            };
            drop(tools);
            send_normalized(events, event)
        }
        SessionUpdate::Plan(plan) => send_normalized(
            events,
            AcpNormalizedEvent::Plan {
                entries: plan.entries.into_iter().map(|entry| entry.content).collect(),
            },
        ),
        SessionUpdate::SessionInfoUpdate(info) => {
            let title = serde_json::to_value(&info)
                .ok()
                .and_then(|value| value.get("title").and_then(|title| title.as_str()).map(str::to_owned));
            send_normalized(events, AcpNormalizedEvent::SessionInfo { title })
        }
        SessionUpdate::CurrentModeUpdate(_) => send_normalized(
            events,
            AcpNormalizedEvent::Progress {
                step: "session_mode".to_owned(),
                detail: "ACP session mode updated".to_owned(),
            },
        ),
        SessionUpdate::ConfigOptionUpdate(_) => send_normalized(
            events,
            AcpNormalizedEvent::Progress {
                step: "session_config".to_owned(),
                detail: "ACP session configuration updated".to_owned(),
            },
        ),
        SessionUpdate::AvailableCommandsUpdate(_) => send_normalized(
            events,
            AcpNormalizedEvent::Progress {
                step: "available_commands".to_owned(),
                detail: "ACP available commands updated".to_owned(),
            },
        ),
        SessionUpdate::UsageUpdate(_) => send_normalized(
            events,
            AcpNormalizedEvent::Progress {
                step: "usage".to_owned(),
                detail: "ACP usage updated".to_owned(),
            },
        ),
        _ => send_normalized(
            events,
            AcpNormalizedEvent::ProtocolDiagnostic {
                message: "ACP session update is not represented by the stable GameEngine adapter"
                    .to_owned(),
            },
        ),
    }
}

async fn handle_permission_request(
    request: RequestPermissionRequest,
    active_session_id: &Arc<Mutex<Option<String>>>,
    tool_calls: &ToolCalls,
    events: &mpsc::Sender<AcpNormalizedEvent>,
    pending_permissions: &PendingPermissions,
    sequence: &AtomicU64,
) -> Result<RequestPermissionResponse, AcpRuntimeError> {
    let session_id = request.session_id.to_string();
    let expected = active_session_id
        .lock()
        .map_err(|_| AcpRuntimeError::Transport("ACP session state lock is poisoned".to_owned()))?
        .clone();
    if expected.as_deref() != Some(session_id.as_str()) {
        return Err(AcpRuntimeError::Protocol(format!(
            "received ACP permission request for unexpected session `{session_id}`"
        )));
    }

    let tool_call_id = request.tool_call.tool_call_id.to_string();
    let tracked = {
        let tools = tool_calls.lock().map_err(|_| {
            AcpRuntimeError::Transport("ACP tool state lock is poisoned".to_owned())
        })?;
        tools.get(&tool_call_id).cloned()
    };
    let title = request
        .tool_call
        .fields
        .title
        .clone()
        .or_else(|| tracked.as_ref().map(|tool| tool.title.clone()))
        .unwrap_or_else(|| "ACP permission request".to_owned());
    let kind = request
        .tool_call
        .fields
        .kind
        .or_else(|| tracked.as_ref().map(|tool| tool.kind));
    let Some(required_capability) = kind.and_then(classify_tool_kind) else {
        send_normalized(
            events,
            AcpNormalizedEvent::ProtocolDiagnostic {
                message: format!(
                    "ACP permission request for tool `{tool_call_id}` could not be safely classified"
                ),
            },
        )?;
        return Ok(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ));
    };

    let request_id = format!("acp-permission-{}", sequence.fetch_add(1, Ordering::SeqCst));
    let options = request
        .options
        .iter()
        .map(|option| AcpPermissionOption {
            id: option.option_id.to_string(),
            name: option.name.clone(),
            kind: normalize_permission_kind(option.kind),
        })
        .collect::<Vec<_>>();
    let valid_option_ids = request
        .options
        .iter()
        .map(|option| option.option_id.to_string())
        .collect::<Vec<_>>();
    let (resolution_tx, resolution_rx) = oneshot::channel();
    pending_permissions
        .lock()
        .map_err(|_| AcpRuntimeError::Transport("ACP permission state lock is poisoned".to_owned()))?
        .insert(request_id.clone(), resolution_tx);

    send_normalized(
        events,
        AcpNormalizedEvent::PermissionRequest(AcpPermissionRequest {
            request_id: request_id.clone(),
            acp_session_id: session_id,
            tool_call_id,
            title,
            required_capability,
            options,
        }),
    )?;

    let outcome = resolution_rx.await.unwrap_or(AcpPermissionOutcome::Cancelled);
    pending_permissions
        .lock()
        .map_err(|_| AcpRuntimeError::Transport("ACP permission state lock is poisoned".to_owned()))?
        .remove(&request_id);

    match outcome {
        AcpPermissionOutcome::Cancelled => Ok(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        )),
        AcpPermissionOutcome::SelectedOption(option_id) => {
            if !valid_option_ids.iter().any(|candidate| candidate == &option_id) {
                return Err(AcpRuntimeError::Protocol(format!(
                    "permission resolution selected unknown ACP option `{option_id}`"
                )));
            }
            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id)),
            ))
        }
    }
}

fn classify_tool_kind(kind: ToolKind) -> Option<AgentCapability> {
    match kind {
        ToolKind::Read => Some(AgentCapability::RawWorkspaceFilesystem),
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => Some(AgentCapability::CodeWorkspaceApply),
        ToolKind::Execute => Some(AgentCapability::ArbitraryCommandExecution),
        ToolKind::Fetch => Some(AgentCapability::NetworkAccess),
        ToolKind::Search | ToolKind::Think | ToolKind::SwitchMode | ToolKind::Other => None,
        _ => None,
    }
}

fn normalize_tool_status(status: ToolCallStatus) -> AcpToolCallStatus {
    match status {
        ToolCallStatus::Pending => AcpToolCallStatus::Pending,
        ToolCallStatus::InProgress => AcpToolCallStatus::InProgress,
        ToolCallStatus::Completed => AcpToolCallStatus::Completed,
        ToolCallStatus::Failed => AcpToolCallStatus::Failed,
        _ => AcpToolCallStatus::Failed,
    }
}

fn normalize_permission_kind(kind: PermissionOptionKind) -> AcpPermissionOptionKind {
    match kind {
        PermissionOptionKind::AllowOnce => AcpPermissionOptionKind::AllowOnce,
        PermissionOptionKind::AllowAlways => AcpPermissionOptionKind::AllowAlways,
        PermissionOptionKind::RejectOnce => AcpPermissionOptionKind::RejectOnce,
        PermissionOptionKind::RejectAlways => AcpPermissionOptionKind::RejectAlways,
        _ => AcpPermissionOptionKind::Other(format!("{kind:?}")),
    }
}

fn stop_reason_label(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        StopReason::Refusal => "refusal",
        StopReason::Cancelled => "cancelled",
        _ => "unknown",
    }
}

fn send_normalized(
    events: &mpsc::Sender<AcpNormalizedEvent>,
    event: AcpNormalizedEvent,
) -> Result<(), AcpRuntimeError> {
    events
        .send(event)
        .map_err(|_| AcpRuntimeError::Transport("ACP event receiver is closed".to_owned()))
}

fn send_event(
    events: &mpsc::Sender<AcpNormalizedEvent>,
    event: AcpNormalizedEvent,
) -> agent_client_protocol::Result<()> {
    events
        .send(event)
        .map_err(|_| agent_client_protocol::Error::internal_error().data("ACP event receiver is closed"))
}

fn sdk_internal_error(error: AcpRuntimeError) -> agent_client_protocol::Error {
    agent_client_protocol::Error::internal_error().data(error.to_string())
}

fn sdk_transport_error(error: agent_client_protocol::Error) -> AcpRuntimeError {
    AcpRuntimeError::Transport(error.to_string())
}

fn classify_sdk_connection_error(error: agent_client_protocol::Error) -> AcpRuntimeError {
    if let Some(message) = error.data.as_ref().and_then(serde_json::Value::as_str) {
        if let Some(message) = message.strip_prefix("unsupported ACP operation: ") {
            return AcpRuntimeError::Unsupported(message.to_owned());
        }
        if let Some(message) = message.strip_prefix("ACP protocol error: ") {
            return AcpRuntimeError::Protocol(message.to_owned());
        }
        if let Some(message) = message.strip_prefix("ACP transport error: ") {
            return AcpRuntimeError::Transport(message.to_owned());
        }
    }
    match error.code {
        agent_client_protocol::schema::v1::ErrorCode::ParseError
        | agent_client_protocol::schema::v1::ErrorCode::InvalidRequest
        | agent_client_protocol::schema::v1::ErrorCode::MethodNotFound
        | agent_client_protocol::schema::v1::ErrorCode::InvalidParams => {
            AcpRuntimeError::Protocol(error.to_string())
        }
        _ => AcpRuntimeError::Transport(format!("ACP connection failed: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn capability_requirements_fail_closed() {
        let required = AcpCapabilities {
            session_resume: true,
            ..AcpCapabilities::default()
        };
        let negotiated = AcpCapabilities::default();
        assert!(matches!(
            validate_required_capabilities(&required, &negotiated),
            Err(AcpRuntimeError::Unsupported(message)) if message.contains("session/resume")
        ));
    }

    #[test]
    fn ambiguous_permission_kinds_are_not_classified() {
        assert_eq!(classify_tool_kind(ToolKind::Search), None);
        assert_eq!(classify_tool_kind(ToolKind::Other), None);
        assert_eq!(
            classify_tool_kind(ToolKind::Execute),
            Some(AgentCapability::ArbitraryCommandExecution)
        );
    }

    #[test]
    fn descriptor_environment_is_forwarded_without_provider_logic() {
        let descriptor = AcpAgentDescriptor {
            id: "test.agent".to_owned(),
            executable: OsString::from("test-agent"),
            arguments: vec![OsString::from("--acp")],
            environment: BTreeMap::from([(OsString::from("TEST_KEY"), OsString::from("value"))]),
            capabilities: AcpCapabilities::default(),
            runtime_identity: AcpRuntimeIdentity::stable("test-agent", None),
        };
        let config = build_agent_config(&descriptor).expect("descriptor should build ACP config");
        assert_eq!(config.arguments(), &["--acp"]);
        assert_eq!(config.environment().get("TEST_KEY"), Some(&"value".to_owned()));
    }

    #[test]
    fn stop_reason_is_semantic_turn_state_only() {
        let event = AcpNormalizedEvent::TurnFinished {
            stop_reason: stop_reason_label(StopReason::EndTurn).to_owned(),
        };
        assert_eq!(
            event.host_event_kind(),
            crate::agent_host::AgentEventKind::SemanticProgress
        );
    }
}
