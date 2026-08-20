//! Goose ACP adapter for GameEngine-managed local inference.
//!
//! Goose-specific process discovery, environment configuration, ACP transport,
//! and normalization terminate in this module. Agent Host authority and Managed
//! Local lifecycle remain owned by their existing layers.

use crate::acp_agent_runtime::{
    AcpAgentDescriptor, AcpAgentRuntime, AcpAgentSession, AcpCapabilities, AcpMcpAccessLevel,
    AcpNormalizedEvent, AcpPermissionOption, AcpPermissionOptionKind, AcpPermissionOutcome,
    AcpPermissionRequest, AcpPermissionResolution, AcpRuntimeError, AcpRuntimeIdentity,
    AcpSessionBinding, AcpToolCallStatus,
};
use crate::agent_host::AgentCapability;
use crate::managed_local_runtime::{
    ManagedExecutionEnvironment, ManagedLocalEndpointLease, ManagedLocalModelConfig,
    ManagedLocalRuntime, managed_context_tokens,
};
use engine_mcp::tool_is_mutating;
use agent_client_protocol::schema::{
    ProtocolVersion,
    v1::{
        CancelNotification, HttpHeader, InitializeRequest, McpServer, McpServerHttp,
        NewSessionRequest, PermissionOptionKind, RequestPermissionOutcome,
        RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
        SessionNotification,
    },
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{AcpAgent, AcpAgentConfig, Client, SessionMessage};
use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, StreamExt};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const GOOSE_AGENT_ID: &str = "goose.managed-local";
const GOOSE_EXECUTABLE_ENV: &str = "GAMEENGINE_GOOSE_EXECUTABLE";
const GOOSE_PROVIDER_ID: &str = "custom_gameengine_managed_local";
const GOOSE_PROVIDER_FILE: &str = "custom_gameengine_managed_local.json";
const GOOSE_PATH_ROOT_ENV: &str = "GOOSE_PATH_ROOT";
const GAMEENGINE_MCP_SERVER_NAME: &str = "gameengine_editor";
const GAMEENGINE_AGENT_RUN_ID_HEADER: &str = "X-GameEngine-Agent-Run-Id";
const GOOSE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const GOOSE_SESSION_START_TIMEOUT: Duration = Duration::from_secs(180);
static GOOSE_CONFIG_SEQUENCE: AtomicU64 = AtomicU64::new(1);
static PERMISSION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Adapter-owned identity linking ACP Goose identity to the exact managed model/runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GooseLocalRuntimeIdentity {
    pub(crate) acp: AcpRuntimeIdentity,
    pub(crate) managed_runtime: String,
    pub(crate) model_id: String,
    pub(crate) model_content_sha256: String,
    pub(crate) model_representation: Option<String>,
    pub(crate) execution_environment: ManagedExecutionEnvironment,
}

/// Machine-local configuration needed to open one Goose ACP runtime.
#[derive(Debug, Clone)]
pub(crate) struct GooseLocalAcpConfig {
    pub(crate) managed_model: ManagedLocalModelConfig,
    pub(crate) workspace_root: PathBuf,
}

impl GooseLocalAcpConfig {
    pub(crate) fn new(
        managed_model: ManagedLocalModelConfig,
        workspace_root: PathBuf,
    ) -> Result<Self, AcpRuntimeError> {
        if !workspace_root.is_absolute() {
            return Err(AcpRuntimeError::InvalidDescriptor(
                "Goose ACP workspace root must be absolute".to_owned(),
            ));
        }
        if !workspace_root.is_dir() {
            return Err(AcpRuntimeError::InvalidDescriptor(format!(
                "Goose ACP workspace root `{}` is not an existing directory",
                workspace_root.display()
            )));
        }
        if !managed_model.state_root.is_absolute() {
            return Err(AcpRuntimeError::InvalidDescriptor(
                "Managed Local state root must be absolute before Goose ACP can isolate its machine-local configuration"
                    .to_owned(),
            ));
        }
        Ok(Self {
            managed_model,
            workspace_root,
        })
    }
}

/// Concrete ACP runtime that delegates local agent behavior to `goose acp`.
pub(crate) struct GooseLocalAcpRuntime {
    descriptor: AcpAgentDescriptor,
    identity: GooseLocalRuntimeIdentity,
    config: GooseLocalAcpConfig,
    executable: PathBuf,
}

impl GooseLocalAcpRuntime {
    pub(crate) fn discover(config: GooseLocalAcpConfig) -> Result<Self, AcpRuntimeError> {
        let executable = discover_goose_executable()?;
        let version_output = command_output_with_timeout(&executable, &["--version"])?;
        let version = first_nonempty_output_line(&version_output).ok_or_else(|| {
            AcpRuntimeError::Transport(
                "Goose executable did not report a version; reinstall or set GAMEENGINE_GOOSE_EXECUTABLE to a working goose binary".to_owned(),
            )
        })?;
        command_output_with_timeout(&executable, &["acp", "--help"])?;

        let acp_identity = AcpRuntimeIdentity::stable("goose", Some(version.clone()));
        let identity = GooseLocalRuntimeIdentity {
            acp: acp_identity.clone(),
            managed_runtime: config.managed_model.benchmark_runtime_identity(),
            model_id: config.managed_model.model_id.clone(),
            model_content_sha256: config.managed_model.model_content_sha256.clone(),
            model_representation: config.managed_model.model_representation.clone(),
            execution_environment: config.managed_model.environment,
        };
        let descriptor = AcpAgentDescriptor {
            id: GOOSE_AGENT_ID.to_owned(),
            executable: executable.as_os_str().to_os_string(),
            arguments: vec![OsString::from("acp")],
            capabilities: AcpCapabilities {
                mcp_http: true,
                ..AcpCapabilities::default()
            },
            runtime_identity: acp_identity,
        };
        Ok(Self {
            descriptor,
            identity,
            config,
            executable,
        })
    }

    pub(crate) fn runtime_identity(&self) -> &GooseLocalRuntimeIdentity {
        &self.identity
    }
}

impl AcpAgentRuntime for GooseLocalAcpRuntime {
    fn descriptor(&self) -> &AcpAgentDescriptor {
        &self.descriptor
    }

    fn open_session(
        &mut self,
        binding: AcpSessionBinding,
    ) -> Result<Box<dyn AcpAgentSession>, AcpRuntimeError> {
        let (command_tx, command_rx) = mpsc::unbounded();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (startup_abort_tx, startup_abort_rx) = oneshot::channel();
        let pending_permissions = Arc::new(Mutex::new(BTreeMap::new()));
        let worker_pending_permissions = Arc::clone(&pending_permissions);
        let executable = self.executable.clone();
        let workspace_root = self.config.workspace_root.clone();
        let managed_model = self.config.managed_model.clone();
        let worker_binding = binding.clone();

        thread::Builder::new()
            .name("gameengine-goose-acp".to_owned())
            .spawn(move || {
                let ready_failure_tx = ready_tx.clone();
                let result = (|| -> Result<(), AcpRuntimeError> {
                    let lease = ManagedLocalRuntime::lease_endpoint(&managed_model)
                        .map_err(|error| AcpRuntimeError::Transport(error.to_string()))?;
                    validate_managed_lease_identity(&lease, &managed_model)?;

                    let ephemeral = GooseEphemeralConfig::create(&managed_model, &lease).map_err(
                        |error| {
                            AcpRuntimeError::Transport(format!(
                                "could not create isolated Goose config: {error}"
                            ))
                        },
                    )?;
                    let environment = goose_environment(&lease, &ephemeral);
                    let mcp_server = gameengine_mcp_server(&worker_binding);
                    let session_result = pollster::block_on(run_goose_session_until_aborted(
                        executable,
                        environment,
                        workspace_root,
                        mcp_server,
                        command_rx,
                        event_tx.clone(),
                        ready_tx,
                        worker_pending_permissions,
                        startup_abort_rx,
                    ));
                    drop(ephemeral);
                    if let Err(error) = lease.release() {
                        let _ = event_tx.send(AcpNormalizedEvent::ProtocolDiagnostic {
                            message: format!(
                                "could not release Managed Local endpoint lease: {error}"
                            ),
                        });
                        if session_result.is_ok() {
                            return Err(AcpRuntimeError::Transport(format!(
                                "could not release Managed Local endpoint lease: {error}"
                            )));
                        }
                    }
                    session_result
                })();

                if let Err(error) = result {
                    let _ = ready_failure_tx.try_send(Err(error.clone()));
                    let _ = event_tx.send(AcpNormalizedEvent::ProtocolDiagnostic {
                        message: error.to_string(),
                    });
                }
            })
            .map_err(|error| {
                AcpRuntimeError::Transport(format!("could not start Goose ACP worker: {error}"))
            })?;

        let ready = match ready_rx.recv_timeout(GOOSE_SESSION_START_TIMEOUT) {
            Ok(ready) => ready?,
            Err(error) => {
                let _ = startup_abort_tx.send(());
                return Err(AcpRuntimeError::Transport(format!(
                    "Goose ACP did not finish initialize/session setup: {error}"
                )));
            }
        };

        self.descriptor.capabilities = ready.capabilities.clone();
        self.descriptor.runtime_identity = ready.identity.clone();
        self.identity.acp = ready.identity;

        Ok(Box::new(GooseLocalAcpSession {
            acp_session_id: ready.session_id,
            binding,
            command_tx,
            event_rx,
            pending_permissions,
            _startup_abort_guard: startup_abort_tx,
            closed: false,
        }))
    }
}

struct GooseSessionReady {
    session_id: String,
    capabilities: AcpCapabilities,
    identity: AcpRuntimeIdentity,
}

enum GooseSessionCommand {
    Prompt(String),
    Cancel,
    Close,
}

struct GooseLocalAcpSession {
    acp_session_id: String,
    binding: AcpSessionBinding,
    command_tx: mpsc::UnboundedSender<GooseSessionCommand>,
    event_rx: std::sync::mpsc::Receiver<AcpNormalizedEvent>,
    pending_permissions: Arc<Mutex<BTreeMap<String, oneshot::Sender<AcpPermissionOutcome>>>>,
    _startup_abort_guard: oneshot::Sender<()>,
    closed: bool,
}

impl AcpAgentSession for GooseLocalAcpSession {
    fn acp_session_id(&self) -> &str {
        &self.acp_session_id
    }

    fn binding(&self) -> &AcpSessionBinding {
        &self.binding
    }

    fn send_prompt(&mut self, prompt: &str) -> Result<(), AcpRuntimeError> {
        if self.closed {
            return Err(AcpRuntimeError::Transport(
                "Goose ACP session is already closed".to_owned(),
            ));
        }
        if prompt.trim().is_empty() {
            return Err(AcpRuntimeError::Protocol(
                "Goose ACP prompt must not be empty".to_owned(),
            ));
        }
        self.command_tx
            .unbounded_send(GooseSessionCommand::Prompt(prompt.to_owned()))
            .map_err(|_| AcpRuntimeError::Transport("Goose ACP worker exited".to_owned()))
    }

    fn try_next_event(&mut self) -> Result<Option<AcpNormalizedEvent>, AcpRuntimeError> {
        match self.event_rx.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) if self.closed => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(AcpRuntimeError::Transport(
                "Goose ACP event stream disconnected".to_owned(),
            )),
        }
    }

    fn resolve_permission(
        &mut self,
        resolution: AcpPermissionResolution,
    ) -> Result<(), AcpRuntimeError> {
        let sender = self
            .pending_permissions
            .lock()
            .map_err(|_| AcpRuntimeError::Transport("Goose permission state was poisoned".to_owned()))?
            .remove(&resolution.request_id)
            .ok_or_else(|| {
                AcpRuntimeError::Protocol(format!(
                    "Goose ACP permission request `{}` is not pending",
                    resolution.request_id
                ))
            })?;
        sender.send(resolution.outcome).map_err(|_| {
            AcpRuntimeError::Transport(
                "Goose ACP permission responder exited before resolution".to_owned(),
            )
        })
    }

    fn cancel(&mut self) -> Result<(), AcpRuntimeError> {
        cancel_pending_permissions(&self.pending_permissions)?;
        self.command_tx
            .unbounded_send(GooseSessionCommand::Cancel)
            .map_err(|_| AcpRuntimeError::Transport("Goose ACP worker exited".to_owned()))
    }

    fn close(&mut self) -> Result<(), AcpRuntimeError> {
        if self.closed {
            return Ok(());
        }
        cancel_pending_permissions(&self.pending_permissions)?;
        self.closed = true;
        self.command_tx
            .unbounded_send(GooseSessionCommand::Close)
            .map_err(|_| AcpRuntimeError::Transport("Goose ACP worker exited".to_owned()))
    }
}

impl Drop for GooseLocalAcpSession {
    fn drop(&mut self) {
        if !self.closed {
            let _ = cancel_pending_permissions(&self.pending_permissions);
            let _ = self.command_tx.unbounded_send(GooseSessionCommand::Close);
            self.closed = true;
        }
    }
}

fn cancel_pending_permissions(
    pending: &Arc<Mutex<BTreeMap<String, oneshot::Sender<AcpPermissionOutcome>>>>,
) -> Result<(), AcpRuntimeError> {
    let senders = {
        let mut pending = pending
            .lock()
            .map_err(|_| AcpRuntimeError::Transport("Goose permission state was poisoned".to_owned()))?;
        std::mem::take(&mut *pending)
    };
    for (_, sender) in senders {
        let _ = sender.send(AcpPermissionOutcome::Cancelled);
    }
    Ok(())
}

async fn run_goose_session_until_aborted(
    executable: PathBuf,
    environment: BTreeMap<String, String>,
    workspace_root: PathBuf,
    mcp_server: McpServer,
    command_rx: mpsc::UnboundedReceiver<GooseSessionCommand>,
    event_tx: std::sync::mpsc::Sender<AcpNormalizedEvent>,
    ready_tx: std::sync::mpsc::SyncSender<Result<GooseSessionReady, AcpRuntimeError>>,
    pending_permissions: Arc<Mutex<BTreeMap<String, oneshot::Sender<AcpPermissionOutcome>>>>,
    startup_abort_rx: oneshot::Receiver<()>,
) -> Result<(), AcpRuntimeError> {
    let session = run_goose_session(
        executable,
        environment,
        workspace_root,
        mcp_server,
        command_rx,
        event_tx,
        ready_tx,
        pending_permissions,
    )
    .fuse();
    let abort = async move {
        match startup_abort_rx.await {
            Ok(()) => Err(AcpRuntimeError::Transport(
                "Goose ACP startup was aborted after the bounded handshake timeout".to_owned(),
            )),
            Err(_) => futures::future::pending::<Result<(), AcpRuntimeError>>().await,
        }
    }
    .fuse();
    futures::pin_mut!(session, abort);
    futures::select_biased! {
        result = session => result,
        result = abort => result,
    }
}

async fn run_goose_session(
    executable: PathBuf,
    environment: BTreeMap<String, String>,
    workspace_root: PathBuf,
    mcp_server: McpServer,
    mut command_rx: mpsc::UnboundedReceiver<GooseSessionCommand>,
    event_tx: std::sync::mpsc::Sender<AcpNormalizedEvent>,
    ready_tx: std::sync::mpsc::SyncSender<Result<GooseSessionReady, AcpRuntimeError>>,
    pending_permissions: Arc<Mutex<BTreeMap<String, oneshot::Sender<AcpPermissionOutcome>>>>,
) -> Result<(), AcpRuntimeError> {
    let agent = AcpAgent::new(
        environment
            .into_iter()
            .fold(AcpAgentConfig::new(executable).arg("acp"), |config, (name, value)| {
                config.env(name, value)
            }),
    );
    let permission_events = event_tx.clone();
    let permission_state = Arc::clone(&pending_permissions);

    Client
        .builder()
        .name("GameEngine")
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                let request_id = format!(
                    "goose-permission-{}",
                    PERMISSION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                );
                let Some(permission_request) =
                    normalize_permission_request(&request_id, &request)
                else {
                    let _ = permission_events.send(AcpNormalizedEvent::ProtocolDiagnostic {
                        message: format!(
                            "Goose ACP permission `{request_id}` could not be classified into an existing GameEngine capability; request cancelled"
                        ),
                    });
                    return responder.respond(RequestPermissionResponse::new(
                        RequestPermissionOutcome::Cancelled,
                    ));
                };

                let (decision_tx, decision_rx) = oneshot::channel();
                {
                    let mut pending = permission_state
                        .lock()
                        .map_err(|_| agent_client_protocol::Error::internal_error())?;
                    pending.insert(request_id.clone(), decision_tx);
                }
                permission_events
                    .send(AcpNormalizedEvent::PermissionRequest(permission_request))
                    .map_err(|_| agent_client_protocol::Error::internal_error())?;

                let outcome = match decision_rx.await {
                    Ok(AcpPermissionOutcome::SelectedOption(option_id)) => request
                        .options
                        .iter()
                        .find(|option| option.option_id.0.as_str() == option_id)
                        .map(|option| {
                            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                                option.option_id.clone(),
                            ))
                        })
                        .unwrap_or(RequestPermissionOutcome::Cancelled),
                    Ok(AcpPermissionOutcome::Cancelled) | Err(_) => {
                        RequestPermissionOutcome::Cancelled
                    }
                };
                responder.respond(RequestPermissionResponse::new(outcome))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_with(agent, async move |connection| {
            let initialize = match connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await
            {
                Ok(initialize) => initialize,
                Err(error) => {
                    return Err(report_start_failure(
                        &ready_tx,
                        acp_sdk_error(error),
                    ));
                }
            };
            if initialize.protocol_version != ProtocolVersion::V1 {
                return Err(report_start_failure(
                    &ready_tx,
                    AcpRuntimeError::Protocol(format!(
                        "Goose negotiated unsupported ACP protocol version {:?}",
                        initialize.protocol_version
                    )),
                ));
            }

            let capabilities = normalize_capabilities(&initialize.agent_capabilities);
            if !capabilities.mcp_http {
                return Err(report_start_failure(
                    &ready_tx,
                    AcpRuntimeError::Unsupported(
                        "installed Goose ACP runtime does not advertise HTTP MCP support".to_owned(),
                    ),
                ));
            }
            let identity = initialize
                .agent_info
                .as_ref()
                .map(|info| {
                    AcpRuntimeIdentity::stable(info.name.clone(), Some(info.version.clone()))
                })
                .unwrap_or_else(|| AcpRuntimeIdentity::stable("goose", None));

            let session_request =
                NewSessionRequest::new(workspace_root).mcp_servers(vec![mcp_server]);
            let mut session = match connection
                .build_session_from(session_request)
                .block_task()
                .start_session()
                .await
            {
                Ok(session) => session,
                Err(error) => {
                    return Err(report_start_failure(
                        &ready_tx,
                        acp_sdk_error(error),
                    ));
                }
            };
            let session_id = session.session_id().to_string();
            ready_tx
                .send(Ok(GooseSessionReady {
                    session_id,
                    capabilities,
                    identity,
                }))
                .map_err(|_| {
                    agent_client_protocol::Error::internal_error()
                        .data("Goose ACP session owner disappeared")
                })?;

            let mut turn_active = false;
            loop {
                if !turn_active {
                    match command_rx.next().await {
                        Some(GooseSessionCommand::Prompt(prompt)) => {
                            session.send_prompt(prompt)?;
                            turn_active = true;
                        }
                        Some(GooseSessionCommand::Cancel) => {
                            session.connection().send_notification(CancelNotification::new(
                                session.session_id().clone(),
                            ))?;
                        }
                        Some(GooseSessionCommand::Close) | None => break,
                    }
                    continue;
                }

                enum Selected {
                    Command(Option<GooseSessionCommand>),
                    Update(Result<SessionMessage, agent_client_protocol::Error>),
                }
                let selected = {
                    let next_command = command_rx.next().fuse();
                    let next_update = session.read_update().fuse();
                    futures::pin_mut!(next_command, next_update);
                    futures::select_biased! {
                        command = next_command => Selected::Command(command),
                        update = next_update => Selected::Update(update),
                    }
                };

                match selected {
                    Selected::Command(Some(GooseSessionCommand::Cancel)) => {
                        session.connection().send_notification(CancelNotification::new(
                            session.session_id().clone(),
                        ))?;
                    }
                    Selected::Command(Some(GooseSessionCommand::Close) | None) => break,
                    Selected::Command(Some(GooseSessionCommand::Prompt(_))) => {
                        let _ = event_tx.send(AcpNormalizedEvent::ProtocolDiagnostic {
                            message: "Goose ACP already has a prompt turn in progress".to_owned(),
                        });
                    }
                    Selected::Update(Ok(SessionMessage::SessionMessage(dispatch))) => {
                        let normalized_events = Arc::new(Mutex::new(Vec::new()));
                        let sink = Arc::clone(&normalized_events);
                        MatchDispatch::new(dispatch)
                            .if_notification(async move |notification: SessionNotification| {
                                if let Ok(value) = serde_json::to_value(&notification.update) {
                                    if let Ok(mut events) = sink.lock() {
                                        *events = normalize_session_update_value(&value);
                                    }
                                }
                                Ok(())
                            })
                            .await
                            .otherwise_ignore()?;
                        if let Ok(mut events) = normalized_events.lock() {
                            for event in events.drain(..) {
                                let _ = event_tx.send(event);
                            }
                        }
                    }
                    Selected::Update(Ok(SessionMessage::StopReason(stop_reason))) => {
                        turn_active = false;
                        let _ = event_tx.send(AcpNormalizedEvent::TurnFinished {
                            stop_reason: format!("{stop_reason:?}"),
                        });
                    }
                    Selected::Update(Err(error)) => return Err(error),
                    Selected::Update(Ok(_)) => {}
                }
            }
            Ok(())
        })
        .await
        .map_err(acp_sdk_error)
}

fn report_start_failure(
    ready_tx: &std::sync::mpsc::SyncSender<Result<GooseSessionReady, AcpRuntimeError>>,
    error: AcpRuntimeError,
) -> agent_client_protocol::Error {
    let message = error.to_string();
    let _ = ready_tx.try_send(Err(error));
    agent_client_protocol::Error::internal_error().data(message)
}

fn acp_sdk_error(error: agent_client_protocol::Error) -> AcpRuntimeError {
    AcpRuntimeError::Transport(format!("Goose ACP transport failed: {error}"))
}

fn normalize_capabilities(
    capabilities: &agent_client_protocol::schema::v1::AgentCapabilities,
) -> AcpCapabilities {
    AcpCapabilities {
        session_load: capabilities.load_session,
        session_resume: capabilities.session_capabilities.resume.is_some(),
        session_list: capabilities.session_capabilities.list.is_some(),
        session_config_options: false,
        mcp_http: capabilities.mcp_capabilities.http,
        mcp_sse: capabilities.mcp_capabilities.sse,
        mcp_over_acp: false,
        extensions: Default::default(),
    }
}

fn normalize_permission_request(
    request_id: &str,
    request: &RequestPermissionRequest,
) -> Option<AcpPermissionRequest> {
    let tool_value = serde_json::to_value(&request.tool_call).unwrap_or(Value::Null);
    let tool_call_id = first_string(&tool_value, &["toolCallId", "tool_call_id", "id"])
        .unwrap_or_else(|| "unknown-tool-call".to_owned());
    let title = first_string(&tool_value, &["title", "name"])
        .unwrap_or_else(|| "Goose tool request".to_owned());
    let required_capability = classify_tool_capability(&tool_value)?;
    let options = request
        .options
        .iter()
        .map(|option| AcpPermissionOption {
            id: option.option_id.0.to_string(),
            name: option.name.clone(),
            kind: match option.kind {
                PermissionOptionKind::AllowOnce => AcpPermissionOptionKind::AllowOnce,
                PermissionOptionKind::AllowAlways => AcpPermissionOptionKind::AllowAlways,
                PermissionOptionKind::RejectOnce => AcpPermissionOptionKind::RejectOnce,
                PermissionOptionKind::RejectAlways => AcpPermissionOptionKind::RejectAlways,
                _ => AcpPermissionOptionKind::Other(format!("{:?}", option.kind)),
            },
        })
        .collect();

    Some(AcpPermissionRequest {
        request_id: request_id.to_owned(),
        acp_session_id: request.session_id.to_string(),
        tool_call_id,
        title,
        required_capability,
        options,
    })
}

fn classify_tool_capability(tool: &Value) -> Option<AgentCapability> {
    let title = first_string_recursive(tool, &["title", "name"])?;
    let normalized = title.trim().to_ascii_lowercase();

    if let Some(tool_name) = normalized.strip_prefix("gameengine editor: ") {
        let tool_name = tool_name.replace(' ', "_");
        return Some(if tool_is_mutating(&tool_name) {
            AgentCapability::CodeWorkspaceApply
        } else {
            AgentCapability::RawWorkspaceFilesystem
        });
    }

    if normalized.starts_with("developer: read")
        || normalized.starts_with("developer: search")
        || normalized.starts_with("developer: list")
    {
        Some(AgentCapability::RawWorkspaceFilesystem)
    } else if normalized.starts_with("developer: write")
        || normalized.starts_with("developer: edit")
        || normalized.starts_with("developer: patch")
    {
        Some(AgentCapability::CodeWorkspaceApply)
    } else if normalized.starts_with("developer: shell")
        || normalized.starts_with("developer: execute")
    {
        Some(AgentCapability::ArbitraryCommandExecution)
    } else if normalized.contains("fetch")
        || normalized.starts_with("web:")
        || normalized.starts_with("browser:")
    {
        Some(AgentCapability::NetworkAccess)
    } else {
        None
    }
}

fn normalize_session_update_value(value: &Value) -> Vec<AcpNormalizedEvent> {
    let kind = first_string(value, &["sessionUpdate", "session_update", "type"])
        .unwrap_or_default();
    match kind.as_str() {
        "agent_message_chunk" => recursive_text(value)
            .map(|text| vec![AcpNormalizedEvent::AgentMessage { text }])
            .unwrap_or_default(),
        "plan" => {
            let entries = collect_plan_entries(value);
            (!entries.is_empty())
                .then_some(AcpNormalizedEvent::Plan { entries })
                .into_iter()
                .collect()
        }
        "tool_call" | "tool_call_update" => {
            let tool_call_id =
                first_string_recursive(value, &["toolCallId", "tool_call_id", "id"])
                    .unwrap_or_else(|| "unknown-tool-call".to_owned());
            let title = first_string_recursive(value, &["title", "name"])
                .unwrap_or_else(|| "Goose tool call".to_owned());
            let status = first_string_recursive(value, &["status"])
                .map(|status| match status.as_str() {
                    "pending" => AcpToolCallStatus::Pending,
                    "in_progress" | "inProgress" => AcpToolCallStatus::InProgress,
                    "completed" => AcpToolCallStatus::Completed,
                    "failed" => AcpToolCallStatus::Failed,
                    _ => AcpToolCallStatus::InProgress,
                })
                .unwrap_or(AcpToolCallStatus::InProgress);
            vec![AcpNormalizedEvent::ToolCall {
                tool_call_id,
                title,
                status,
            }]
        }
        "session_info_update" => vec![AcpNormalizedEvent::SessionInfo {
            title: first_string_recursive(value, &["title"]),
        }],
        "agent_thought_chunk" => recursive_text(value)
            .map(|detail| {
                vec![AcpNormalizedEvent::Progress {
                    step: "goose-reasoning".to_owned(),
                    detail,
                }]
            })
            .unwrap_or_default(),
        "user_message_chunk"
        | "available_commands_update"
        | "current_mode_update"
        | "config_option_update"
        | "usage_update" => Vec::new(),
        "" => vec![AcpNormalizedEvent::ProtocolDiagnostic {
            message: "Goose ACP session update omitted its discriminator".to_owned(),
        }],
        other => vec![AcpNormalizedEvent::ProtocolDiagnostic {
            message: format!("unrecognized Goose ACP session update `{other}`"),
        }],
    }
}

fn recursive_text(value: &Value) -> Option<String> {
    first_string_recursive(value, &["text"])
}

fn collect_plan_entries(value: &Value) -> Vec<String> {
    let mut entries = Vec::new();
    collect_strings_for_keys(value, &["content", "title"], &mut entries);
    entries.sort();
    entries.dedup();
    entries
}

fn collect_strings_for_keys(value: &Value, keys: &[&str], output: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if keys.contains(&key.as_str()) {
                    if let Some(text) = child.as_str() {
                        if !text.trim().is_empty() {
                            output.push(text.to_owned());
                        }
                    }
                }
                collect_strings_for_keys(child, keys, output);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_strings_for_keys(child, keys, output);
            }
        }
        _ => {}
    }
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn first_string_recursive(value: &Value, keys: &[&str]) -> Option<String> {
    if let Some(found) = first_string(value, keys) {
        return Some(found);
    }
    match value {
        Value::Object(object) => object
            .values()
            .find_map(|child| first_string_recursive(child, keys)),
        Value::Array(values) => values
            .iter()
            .find_map(|child| first_string_recursive(child, keys)),
        _ => None,
    }
}

fn gameengine_mcp_server(binding: &AcpSessionBinding) -> McpServer {
    let mut headers = vec![HttpHeader::new(
        "Authorization",
        format!("Bearer {}", binding.mcp.authorization_token()),
    )];
    if binding.mcp.access == AcpMcpAccessLevel::AgentRunBoundReadWrite {
        if let Some(run_id) = binding.gameengine_run_id.as_deref() {
            headers.push(HttpHeader::new(GAMEENGINE_AGENT_RUN_ID_HEADER, run_id));
        }
    }
    McpServer::Http(
        McpServerHttp::new(GAMEENGINE_MCP_SERVER_NAME, binding.mcp.endpoint()).headers(headers),
    )
}

fn validate_managed_lease_identity(
    lease: &ManagedLocalEndpointLease,
    config: &ManagedLocalModelConfig,
) -> Result<(), AcpRuntimeError> {
    let identity = lease.identity();
    if identity.model_id != config.model_id
        || identity.model_content_sha256 != config.model_content_sha256
        || identity.model_representation != config.model_representation
        || identity.runtime_identity != config.benchmark_runtime_identity()
        || identity.execution_environment != config.environment
        || !identity.endpoint_url.starts_with("http://127.0.0.1:")
    {
        return Err(AcpRuntimeError::Transport(
            "Managed Local endpoint lease identity does not match the frozen Goose configuration"
                .to_owned(),
        ));
    }
    Ok(())
}

fn goose_environment(
    lease: &ManagedLocalEndpointLease,
    ephemeral: &GooseEphemeralConfig,
) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("GOOSE_PROVIDER".to_owned(), GOOSE_PROVIDER_ID.to_owned()),
        ("GOOSE_MODEL".to_owned(), lease.identity().model_id.clone()),
        ("GOOSE_MODE".to_owned(), "approve".to_owned()),
        (
            GOOSE_PATH_ROOT_ENV.to_owned(),
            ephemeral.root.to_string_lossy().into_owned(),
        ),
        ("GOOSE_TELEMETRY_ENABLED".to_owned(), "false".to_owned()),
        (
            "GOOSE_PROJECT_TRACKER_ENABLED".to_owned(),
            "false".to_owned(),
        ),
    ])
}

fn goose_provider_document(
    lease: &ManagedLocalEndpointLease,
    config: &ManagedLocalModelConfig,
) -> Value {
    json!({
        "name": GOOSE_PROVIDER_ID,
        "engine": "openai",
        "display_name": "GameEngine Managed Local",
        "description": "Ephemeral GameEngine-managed local inference endpoint",
        "api_key_env": "",
        "base_url": lease.identity().endpoint_url.clone(),
        "models": [{
            "name": lease.identity().model_id.clone(),
            "context_limit": managed_context_tokens(config) as usize,
            "input_token_cost": null,
            "output_token_cost": null,
            "currency": null,
            "supports_cache_control": null,
            "reasoning": false
        }],
        "headers": null,
        "timeout_seconds": 900,
        "supports_streaming": false,
        "requires_auth": false,
        "catalog_provider_id": null,
        "base_path": "v1/chat/completions",
        "env_vars": null,
        "dynamic_models": false,
        "skip_canonical_filtering": true,
        "model_doc_link": null,
        "setup_steps": [],
        "fast_model": null,
        "preserves_thinking": false,
        "emit_clear_thinking": false,
        "setup": null
    })
}

struct GooseEphemeralConfig {
    root: PathBuf,
}

impl GooseEphemeralConfig {
    fn create(
        config: &ManagedLocalModelConfig,
        lease: &ManagedLocalEndpointLease,
    ) -> std::io::Result<Self> {
        let id = GOOSE_CONFIG_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = config
            .state_root
            .join("goose-acp")
            .join(format!("{}-{id}", std::process::id()));
        if !root.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "GOOSE_PATH_ROOT must be an absolute machine-local path",
            ));
        }
        if root.exists() {
            fs::remove_dir_all(&root)?;
        }
        let provider_dir = root.join("config").join("custom_providers");
        fs::create_dir_all(&provider_dir)?;
        let provider = serde_json::to_vec_pretty(&goose_provider_document(lease, config))
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        fs::write(provider_dir.join(GOOSE_PROVIDER_FILE), provider)?;
        Ok(Self { root })
    }
}

impl Drop for GooseEphemeralConfig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn discover_goose_executable() -> Result<PathBuf, AcpRuntimeError> {
    if let Some(configured) = std::env::var_os(GOOSE_EXECUTABLE_ENV) {
        let configured = PathBuf::from(configured);
        if configured.is_file() {
            return Ok(configured);
        }
        return Err(AcpRuntimeError::Transport(format!(
            "{GOOSE_EXECUTABLE_ENV} points to `{}` but that file does not exist",
            configured.display()
        )));
    }

    for candidate in goose_executable_candidates() {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(AcpRuntimeError::Transport(
        "Goose executable was not found. Install Goose, add it to PATH, or set GAMEENGINE_GOOSE_EXECUTABLE to the machine-local goose executable".to_owned(),
    ))
}

fn goose_executable_candidates() -> Vec<PathBuf> {
    let executable_name = if cfg!(windows) { "goose.exe" } else { "goose" };
    let mut candidates = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(executable_name))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let home = PathBuf::from(home);
        if cfg!(windows) {
            candidates.push(home.join("goose").join("goose.exe"));
            candidates.push(home.join(".local").join("bin").join("goose.exe"));
        } else {
            candidates.push(home.join(".local").join("bin").join("goose"));
        }
    }
    candidates
}

fn command_output_with_timeout(
    executable: &Path,
    arguments: &[&str],
) -> Result<Output, AcpRuntimeError> {
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            AcpRuntimeError::Transport(format!(
                "could not execute Goose `{}`: {error}",
                executable.display()
            ))
        })?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().map_err(|error| {
                    AcpRuntimeError::Transport(format!("could not collect Goose output: {error}"))
                })?;
                if !status.success() {
                    return Err(AcpRuntimeError::Transport(format!(
                        "Goose command `{}` failed: {}",
                        arguments.join(" "),
                        command_output_text(&output)
                    )));
                }
                return Ok(output);
            }
            Ok(None) if started.elapsed() < GOOSE_PROBE_TIMEOUT => {
                thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AcpRuntimeError::Transport(format!(
                    "Goose command `{}` timed out",
                    arguments.join(" "),
                )));
            }
            Err(error) => {
                return Err(AcpRuntimeError::Transport(format!(
                    "could not inspect Goose command status: {error}"
                )));
            }
        }
    }
}

fn command_output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{} {}", stdout.trim(), stderr.trim())
        .trim()
        .to_owned()
}

fn first_nonempty_output_line(output: &Output) -> Option<String> {
    output
        .stdout
        .split(|byte| *byte == b'\n')
        .chain(output.stderr.split(|byte| *byte == b'\n'))
        .map(|line| String::from_utf8_lossy(line).trim().to_owned())
        .find(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_agent_runtime::AcpMcpConnection;

    fn managed_model() -> ManagedLocalModelConfig {
        ManagedLocalModelConfig {
            state_root: PathBuf::from("state"),
            environment: crate::managed_local_runtime::ManagedExecutionEnvironment::WindowsNative,
            model_id: "gguf:test".to_owned(),
            model_content_sha256: "a".repeat(64),
            model_path: PathBuf::from("model.gguf"),
            model_size_bytes: 1,
            quantization: Some("Q4_K_M".to_owned()),
            model_representation: Some("gguf".to_owned()),
            capability: Default::default(),
            projector_path: None,
            runtime_tag: "tag".to_owned(),
            runtime_revision: "revision".to_owned(),
            runtime_artifact_sha256: "b".repeat(64),
            runtime_compatibility_version: "compat".to_owned(),
        }
    }

    #[test]
    fn managed_identity_stays_attached_to_goose_identity() {
        let model = managed_model();
        let identity = GooseLocalRuntimeIdentity {
            acp: AcpRuntimeIdentity::stable("goose", Some("1.0".to_owned())),
            managed_runtime: model.benchmark_runtime_identity(),
            model_id: model.model_id.clone(),
            model_content_sha256: model.model_content_sha256.clone(),
            model_representation: model.model_representation.clone(),
            execution_environment: model.environment,
        };
        assert_eq!(identity.model_id, "gguf:test");
        assert_eq!(identity.model_content_sha256, "a".repeat(64));
        assert!(identity.managed_runtime.contains("revision"));
    }

    #[test]
    fn run_bound_mcp_declaration_carries_ephemeral_run_authority() {
        let binding = AcpSessionBinding {
            gameengine_session_id: "session".to_owned(),
            gameengine_run_id: Some("run-42".to_owned()),
            mcp: AcpMcpConnection::new(
                "http://127.0.0.1:4321/mcp",
                "secret",
                AcpMcpAccessLevel::AgentRunBoundReadWrite,
            )
            .expect("mcp connection"),
        };
        let value = serde_json::to_value(gameengine_mcp_server(&binding)).expect("mcp json");
        assert_eq!(value["type"], "http");
        assert!(value.to_string().contains("Bearer secret"));
        assert!(value.to_string().contains("run-42"));
    }

    #[test]
    fn read_only_mcp_declaration_does_not_invent_agent_run_identity() {
        let binding = AcpSessionBinding {
            gameengine_session_id: "session".to_owned(),
            gameengine_run_id: None,
            mcp: AcpMcpConnection::new(
                "http://127.0.0.1:4321/mcp",
                "secret",
                AcpMcpAccessLevel::ReadOnly,
            )
            .expect("mcp connection"),
        };
        let value = serde_json::to_value(gameengine_mcp_server(&binding)).expect("mcp json");
        assert!(!value.to_string().contains(GAMEENGINE_AGENT_RUN_ID_HEADER));
    }

    #[test]
    fn session_update_normalizes_message_and_tool_call() {
        let message = serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "hello"}
        });
        assert_eq!(
            normalize_session_update_value(&message),
            vec![AcpNormalizedEvent::AgentMessage {
                text: "hello".to_owned()
            }]
        );

        let tool = serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-1",
            "title": "gameengine_editor.apply",
            "status": "in_progress"
        });
        assert_eq!(
            normalize_session_update_value(&tool),
            vec![AcpNormalizedEvent::ToolCall {
                tool_call_id: "call-1".to_owned(),
                title: "gameengine_editor.apply".to_owned(),
                status: AcpToolCallStatus::InProgress,
            }]
        );
    }

    #[test]
    fn provider_document_is_process_local_and_has_no_secret() {
        let provider = json!({
            "name": GOOSE_PROVIDER_ID,
            "engine": "openai",
            "base_url": "http://127.0.0.1:8080",
            "models": [{"name": "gguf:test"}],
            "requires_auth": false,
            "dynamic_models": false,
        });
        let text = provider.to_string();
        assert_eq!(provider["name"], GOOSE_PROVIDER_ID);
        assert_eq!(provider["requires_auth"], false);
        assert_eq!(provider["dynamic_models"], false);
        assert!(!text.contains("OPENAI_API_KEY"));
        assert!(!text.contains("Authorization"));
    }

    #[test]
    fn tool_permission_classification_is_specific_and_fails_closed() {
        let gameengine_write =
            serde_json::json!({"title": "gameengine editor: authoring.apply"});
        let gameengine_read =
            serde_json::json!({"title": "gameengine editor: authoring.inspect"});
        let shell = serde_json::json!({"title": "developer: shell"});
        let unknown = serde_json::json!({"title": "mystery extension: mutate everything"});
        assert_eq!(
            classify_tool_capability(&gameengine_write),
            Some(AgentCapability::CodeWorkspaceApply)
        );
        assert_eq!(
            classify_tool_capability(&gameengine_read),
            Some(AgentCapability::RawWorkspaceFilesystem)
        );
        assert_eq!(
            classify_tool_capability(&shell),
            Some(AgentCapability::ArbitraryCommandExecution)
        );
        assert_eq!(classify_tool_capability(&unknown), None);
    }
}
