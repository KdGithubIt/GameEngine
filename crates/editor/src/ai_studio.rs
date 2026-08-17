//! Conversation-first AI Studio frontend.
//!
//! This module owns only presentation and direct user interaction. Agent
//! lifecycle, permissions, persistence, provider process management, and code
//! workspace rules live in the GUI-free `agent_host` module.

use crate::agent_host::{
    project_storage_key, AgentCapability, AgentEventKind, AgentHost, AgentProposal, AgentRunState,
    ApprovalScope, AuthoritativeStateSnapshot, CodeChange, CodeWorkspace, CompletionStatus,
    ConversationRole, ExternalAgentProcess, ManagedValidationAttemptStatus, PermissionCheck,
    ProcessStream, ResumeDisposition,
};
use crate::live_observation::{LiveObservationError, LiveObservationManager};
use crate::native_agent::{
    LocalModelConfig, ModelCapabilityProfile, NativeAnswer, NativeQuestionTask, QuestionMessage,
    QuestionRole, DEFAULT_LOCAL_MODEL_ENDPOINT,
};
use crate::resource_arbitration::{
    classify_workload, resolve_resource_plan, CapabilityAvailability, InferenceWorkload,
    MemoryPressure, PresentationPosture, QualityPreference, ReclaimLevel, ResourcePlan,
    WorkloadSignals,
};
use crate::remote_ai_studio::{
    events_json, frame_bytes, sessions_json, snapshot_json, RemoteAiStudioRequest,
    RemoteAiStudioResponse, RemoteAiStudioServer, RemoteOperation, RemotePermissionScope,
};
use eframe::egui;
use engine::{InputCommand, KeyCode, MouseButton};
use engine_authoring::ProjectRoot;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::PathBuf;

const PROVIDER_EVENT_PREFIX: &str = "GAMEENGINE_AGENT_EVENT ";
const MAX_AUTONOMOUS_SOURCE_REPAIRS: usize = 2;
const MAX_AUTONOMOUS_RUNTIME_REPAIRS: usize = 2;
const AI_STUDIO_PREFERENCES_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiStudioPreferences {
    schema_version: u32,
    #[serde(default)]
    quality_preference: QualityPreference,
    #[serde(default = "default_local_model_endpoint")]
    local_model_endpoint: String,
    #[serde(default)]
    local_model_name: String,
}

impl Default for AiStudioPreferences {
    fn default() -> Self {
        Self {
            schema_version: AI_STUDIO_PREFERENCES_SCHEMA_VERSION,
            quality_preference: QualityPreference::Auto,
            local_model_endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            local_model_name: String::new(),
        }
    }
}

fn default_local_model_endpoint() -> String {
    DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned()
}

/// Authoritative Editor identity captured across native-inference interruption boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiStudioAuthoritativeState {
    /// Unique Editor document revision observed at the resource-control boundary.
    pub document_revision: u64,
    /// Monotonic project game-code generation observed at the same boundary.
    pub game_code_generation: u64,
    /// Current document path, when a document is open.
    pub document_path: Option<PathBuf>,
    /// Whether the authoritative document has unsaved changes.
    pub document_dirty: bool,
}

/// Renderer-facing reclaim request that carries no agent/model identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiStudioReclaimLevel {
    /// Keep current presentation residency.
    None,
    /// Release view-local and transient recreatable presentation resources.
    Transient,
    /// Release additional reusable but recreatable presentation residency.
    Aggressive,
}

impl From<ReclaimLevel> for AiStudioReclaimLevel {
    fn from(value: ReclaimLevel) -> Self {
        match value {
            ReclaimLevel::None => Self::None,
            ReclaimLevel::Transient => Self::Transient,
            ReclaimLevel::Aggressive => Self::Aggressive,
        }
    }
}

impl From<AiStudioReclaimLevel> for ReclaimLevel {
    fn from(value: AiStudioReclaimLevel) -> Self {
        match value {
            AiStudioReclaimLevel::None => Self::None,
            AiStudioReclaimLevel::Transient => Self::Transient,
            AiStudioReclaimLevel::Aggressive => Self::Aggressive,
        }
    }
}

impl From<AiStudioAuthoritativeState> for AuthoritativeStateSnapshot {
    fn from(value: AiStudioAuthoritativeState) -> Self {
        Self {
            document_revision: value.document_revision,
            game_code_generation: value.game_code_generation,
            document_path: value.document_path,
            document_dirty: value.document_dirty,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceRepairDecision {
    Wait,
    Retry(usize),
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeRepairDecision {
    Wait,
    Retry(usize),
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalAgentPurpose {
    BuildOrRepair,
    RuntimeEvaluation,
}

#[derive(Debug, Clone)]
struct ManagedRuntimeObservation {
    artifact_id: String,
    path: PathBuf,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ProviderAgentEvent {
    Progress { step: String, detail: String },
    ToolAction { tool: String, action: String, success: Option<bool> },
    CompletionGate { gate: String, status: CompletionStatus, message: String },
    PlaytestResult { launched: bool, interactions_passed: Option<bool>, message: String },
    RuntimeInput { input: ProviderRuntimeInput },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProviderRuntimeInput {
    Key { key: String, pressed: bool },
    MouseButton { button: String, pressed: bool },
    MouseMove { x: f32, y: f32 },
    MouseDelta { x: f64, y: f64 },
    MouseScroll { amount: f32 },
}

impl ProviderRuntimeInput {
    fn command(&self) -> Result<InputCommand, String> {
        match self {
            Self::Key { key, pressed } => Ok(InputCommand::Key {
                key: provider_key_code(key)?,
                pressed: *pressed,
            }),
            Self::MouseButton { button, pressed } => Ok(InputCommand::MouseButton {
                button: provider_mouse_button(button)?,
                pressed: *pressed,
            }),
            Self::MouseMove { x, y } if x.is_finite() && y.is_finite() => {
                Ok(InputCommand::MouseMove { position: (*x, *y) })
            }
            Self::MouseDelta { x, y } if x.is_finite() && y.is_finite() => {
                Ok(InputCommand::MouseDelta { delta: (*x, *y) })
            }
            Self::MouseScroll { amount } if amount.is_finite() => {
                Ok(InputCommand::MouseScroll { amount: *amount })
            }
            Self::MouseMove { .. } | Self::MouseDelta { .. } | Self::MouseScroll { .. } => {
                Err("runtime input numeric values must be finite".to_owned())
            }
        }
    }
}

/// Ephemeral Editor-owned MCP connection injected into compatible agent runtimes.
///
/// The authorization token is intentionally private and is never serialized by
/// AI Studio. It is exposed to a launched child only through that child's
/// process environment.
pub struct AiStudioConnection {
    endpoint: String,
    authorization_token: String,
}

impl AiStudioConnection {
    /// Creates an in-memory connection descriptor for the active Editor MCP host.
    pub fn new(endpoint: impl Into<String>, authorization_token: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            authorization_token: authorization_token.into(),
        }
    }
}

/// Managed Editor-runtime operation requested by AI Studio after authorization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AiStudioRuntimeAction {
    /// Suspend optional Editor presentation and release recreatable GPU resources.
    EnterInferenceFocused {
        /// Reclaim level selected by the application-layer resource broker.
        reclaim: AiStudioReclaimLevel,
    },
    /// Restore normal Editor presentation; completion is reported after one drawn frame.
    RestoreEditorPresentation,
    /// Capture current authoritative Editor revision/generation before resuming a run.
    InspectAuthoritativeState,
    /// Start the normal Editor Play path for the active project.
    StartPlaytest,
    /// Queue one provider-planned command through the normal AI Agent virtual-input source.
    SendInput(InputCommand),
    /// Capture the currently rendered Game View through the engine frame-capture path.
    CaptureFrame,
    /// Stop the managed Editor Play session.
    StopPlaytest,
}

/// Result of one managed Editor-runtime operation returned to AI Studio.
pub enum AiStudioRuntimeResult {
    /// Optional Editor presentation is suspended at the requested reclaim level.
    InferenceFocusedEntered {
        /// Reclaim level that was applied before native inference.
        reclaim: AiStudioReclaimLevel,
    },
    /// Restore was requested and authoritative state was captured before manual editing.
    EditorRestorePending {
        /// Authoritative Editor identity captured before presentation restore completes.
        state: AiStudioAuthoritativeState,
    },
    /// A normal Editor frame was drawn after restoration.
    EditorRestored,
    /// Current authoritative Editor identity was inspected for Resume.
    AuthoritativeState(AiStudioAuthoritativeState),
    /// Play is running and runtime observation is available.
    PlayStarted,
    /// Play start is waiting for an engine-managed game-code build.
    PlayStartPending,
    /// One AI Agent input command was accepted by the normal runtime input queue.
    RuntimeInputQueued(InputCommand),
    /// The managed Play session stopped.
    PlayStopped,
    /// A Game View frame was captured by the runtime renderer.
    FrameCaptured(crate::FrameCapture),
    /// The requested operation failed without bypassing the normal runtime path.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PendingPermissionAction {
    LaunchExternalAgent,
    LaunchRuntimeEvaluation,
    ApplyCodeChanges,
    LaunchPlaytest,
    SendRuntimeInput(InputCommand),
    CaptureFrame,
}

struct PendingPermission {
    run_id: String,
    capability: AgentCapability,
    action: PendingPermissionAction,
}

/// Project-scoped conversation-first AI Studio window.
///
/// The panel persists sessions outside canonical project data by default,
/// snapshots proposal versions before starting a run, and routes code writes
/// through a reviewable isolated workspace. Project-shared history is explicit.
pub struct AiStudioPanel {
    project_root: PathBuf,
    project_id: String,
    connection: AiStudioConnection,
    host: AgentHost,
    remote_server: Option<RemoteAiStudioServer>,
    remote_requests: Option<std::sync::mpsc::Receiver<RemoteAiStudioRequest>>,
    live_observation: LiveObservationManager,
    selected_session: String,
    proposal_draft: AgentProposal,
    message_draft: String,
    preferences_path: PathBuf,
    quality_preference: QualityPreference,
    local_model_endpoint: String,
    local_model_name: String,
    resolved_workload: InferenceWorkload,
    resource_plan: ResourcePlan,
    editing_interrupted: bool,
    restore_for_editing: bool,
    interrupt_snapshot: Option<AiStudioAuthoritativeState>,
    native_question: Option<NativeQuestionTask>,
    pending_native_question_start: Option<(LocalModelConfig, Vec<QuestionMessage>, String)>,
    native_question_session: Option<String>,
    provider_program: String,
    provider_args: String,
    open: bool,
    active_run_id: Option<String>,
    process: Option<ExternalAgentProcess>,
    process_purpose: Option<ExternalAgentPurpose>,
    code_workspace: Option<CodeWorkspace>,
    pending_code_changes: Vec<CodeChange>,
    pending_permission: Option<PendingPermission>,
    pending_runtime_action: Option<AiStudioRuntimeAction>,
    managed_input_plan: VecDeque<InputCommand>,
    managed_input_recipe: Vec<InputCommand>,
    managed_candidate_input_recipe: Vec<InputCommand>,
    managed_playtest_requested: bool,
    managed_capture_requested: bool,
    managed_repair_requested: bool,
    managed_runtime_repairs: usize,
    managed_runtime_observation: Option<ManagedRuntimeObservation>,
    managed_evaluation_requested: bool,
    managed_playtest_started_at: Option<std::time::Instant>,
    last_captured_frame: Option<(egui::TextureHandle, String, u32, u32)>,
    status: Option<String>,
}

impl AiStudioPanel {
    /// Opens the project-scoped AI Studio state for an Editor project.
    pub fn new(project: &ProjectRoot, connection: AiStudioConnection) -> Result<Self, String> {
        let data_root = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("GameEngine")
            .join("ai")
            .join(project_storage_key(project.project_id().as_str(), project.path()));
        let preferences_path = data_root.join("preferences.json");
        let preferences = load_ai_studio_preferences(&preferences_path);
        let mut host = AgentHost::open(project.path().to_path_buf(), data_root)
            .map_err(|error| error.to_string())?;
        let selected_session = match host.session_ids().into_iter().next_back() {
            Some(id) => id,
            None => host
                .create_session("New AI Studio session")
                .map_err(|error| error.to_string())?,
        };
        let proposal_draft = host
            .session(&selected_session)
            .map_err(|error| error.to_string())?
            .proposal
            .clone();
        let active_run_id = host.active_writer_run_id().map(str::to_owned);
        Ok(Self {
            project_root: project.path().to_path_buf(),
            project_id: project.project_id().as_str().to_owned(),
            connection,
            host,
            remote_server: None,
            remote_requests: None,
            live_observation: LiveObservationManager::default(),
            selected_session,
            proposal_draft,
            message_draft: String::new(),
            preferences_path,
            quality_preference: preferences.quality_preference,
            local_model_endpoint: preferences.local_model_endpoint,
            local_model_name: preferences.local_model_name,
            resolved_workload: InferenceWorkload::InteractiveReasoning,
            resource_plan: resolve_resource_plan(
                InferenceWorkload::InteractiveReasoning,
                preferences.quality_preference,
                MemoryPressure::Unknown,
                Default::default(),
            ),
            editing_interrupted: false,
            restore_for_editing: false,
            interrupt_snapshot: None,
            native_question: None,
            pending_native_question_start: None,
            native_question_session: None,
            provider_program: String::new(),
            provider_args: String::new(),
            open: true,
            active_run_id,
            process: None,
            process_purpose: None,
            code_workspace: None,
            pending_code_changes: Vec::new(),
            pending_permission: None,
            pending_runtime_action: None,
            managed_input_plan: VecDeque::new(),
            managed_input_recipe: Vec::new(),
            managed_candidate_input_recipe: Vec::new(),
            managed_playtest_requested: false,
            managed_capture_requested: false,
            managed_repair_requested: false,
            managed_runtime_repairs: 0,
            managed_runtime_observation: None,
            managed_evaluation_requested: false,
            managed_playtest_started_at: None,
            last_captured_frame: None,
            status: None,
        })
    }

    /// Makes the AI Studio window visible.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Takes one authorized managed runtime action for the Editor shell to execute.
    pub fn take_runtime_action(&mut self) -> Option<AiStudioRuntimeAction> {
        self.pending_runtime_action.take()
    }

    /// Returns whether AI Studio is waiting for the normal Editor Play path to become active.
    pub fn waiting_for_playtest_start(&self) -> bool {
        self.managed_playtest_requested && self.managed_playtest_started_at.is_none()
    }

    /// Takes one rate-bounded live-observation capture request for the Editor shell.
    pub fn take_live_observation_capture_request(&mut self) -> bool {
        for run_id in self.live_observation.run_ids() {
            let keep = self.host.run(&run_id).is_ok_and(|run| {
                !matches!(
                    run.state,
                    AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
                )
            });
            if !keep {
                self.live_observation.remove_run(&run_id);
            }
        }
        if !self.live_observation.begin_capture() {
            return false;
        }
        self.resolved_workload = InferenceWorkload::RuntimeObservation;
        self.resource_plan = resolve_resource_plan(
            InferenceWorkload::RuntimeObservation,
            self.quality_preference,
            MemoryPressure::Unknown,
            Default::default(),
        );
        true
    }

    /// Reports one renderer-owned Game View readback to the transient live-media path.
    pub fn report_live_observation_capture(
        &mut self,
        result: Result<crate::FrameCapture, String>,
        readback: std::time::Duration,
    ) {
        match result {
            Ok(capture) => {
                if let Err(error) = self.live_observation.report_capture(&capture, readback) {
                    self.status = Some(error.to_string());
                }
            }
            Err(error) => {
                self.live_observation.report_capture_failure();
                self.status = Some(format!("Live Game View observation is unavailable: {error}"));
            }
        }
    }

    /// Records the result of an Editor-owned managed runtime action.
    pub fn report_runtime_result(&mut self, context: &egui::Context, result: AiStudioRuntimeResult) {
        match &result {
            AiStudioRuntimeResult::InferenceFocusedEntered { reclaim } => {
                self.status = Some(format!(
                    "InferenceFocused: optional Editor presentation suspended ({reclaim:?} reclaim)."
                ));
                if let Some((config, conversation, session_id)) =
                    self.pending_native_question_start.take()
                {
                    self.spawn_native_question(config, conversation, session_id);
                }
                return;
            }
            AiStudioRuntimeResult::EditorRestorePending { state } => {
                if self.restore_for_editing {
                    self.interrupt_snapshot = Some(state.clone());
                    self.status = Some(
                        "Restoring Editor presentation before manual editing...".to_owned(),
                    );
                } else {
                    self.status = Some("Restoring Editor presentation after native inference...".to_owned());
                }
                return;
            }
            AiStudioRuntimeResult::EditorRestored => {
                if self.restore_for_editing {
                    if let (Some(run_id), Some(snapshot)) =
                        (self.active_run_id.clone(), self.interrupt_snapshot.take())
                        && let Err(error) = self
                            .host
                            .interrupt_for_editing(&run_id, snapshot.into())
                    {
                        self.status = Some(error.to_string());
                        self.restore_for_editing = false;
                        return;
                    }
                    self.editing_interrupted = true;
                    self.status = Some(
                        "AI paused for editing. Editor presentation is restored; Resume will re-inspect authoritative state."
                            .to_owned(),
                    );
                } else {
                    self.status = Some("Editor presentation restored after native inference.".to_owned());
                }
                self.restore_for_editing = false;
                return;
            }
            AiStudioRuntimeResult::AuthoritativeState(state) => {
                if let Some(run_id) = self.active_run_id.clone() {
                    match self.host.resume_after_editing(&run_id, state.clone().into()) {
                        Ok(ResumeDisposition::ResumedUnchanged) => {
                            self.status = Some("Authoritative state is unchanged; the run may resume.".to_owned());
                        }
                        Ok(ResumeDisposition::ReinspectRequired) => {
                            self.status = Some("User edits changed authoritative state; the run returned to inspection.".to_owned());
                        }
                        Ok(ResumeDisposition::RepairRequired) => {
                            self.status = Some("User edits changed acceptance-relevant state; the run returned to repair.".to_owned());
                        }
                        Err(error) => {
                            self.status = Some(error.to_string());
                            return;
                        }
                    }
                } else {
                    self.status = Some("Authoritative Editor state re-inspected.".to_owned());
                }
                self.editing_interrupted = false;
                return;
            }
            _ => {}
        }
        let Some(run_id) = self.active_run_id.clone() else { return; };
        match result {
            AiStudioRuntimeResult::InferenceFocusedEntered { .. }
            | AiStudioRuntimeResult::EditorRestorePending { .. }
            | AiStudioRuntimeResult::EditorRestored
            | AiStudioRuntimeResult::AuthoritativeState(_) => unreachable!(
                "resource-control results are handled before run-scoped runtime results"
            ),
            AiStudioRuntimeResult::PlayStarted => {
                if self.managed_playtest_started_at.is_none() {
                    self.managed_playtest_started_at = Some(std::time::Instant::now());
                    if let Err(error) = self.host.record_playtest_result(
                        &run_id,
                        true,
                        None,
                        "Managed Editor Play launched successfully.",
                    ) {
                        self.status = Some(error.to_string());
                    } else if self.managed_input_plan.is_empty() {
                        self.request_managed_frame_capture_if_ready(&run_id);
                    }
                }
            }
            AiStudioRuntimeResult::PlayStartPending => {
                self.status = Some("Managed Play is waiting for the engine-managed game-code build.".to_owned());
            }
            AiStudioRuntimeResult::RuntimeInputQueued(command) => {
                if let Err(error) = self
                    .host
                    .record_managed_runtime_input(&run_id, format!("{command:?}"))
                {
                    self.status = Some(error.to_string());
                } else {
                    self.status = Some(format!(
                        "Queued managed AI Agent runtime input; {} planned command(s) remain.",
                        self.managed_input_plan.len()
                    ));
                    if self.managed_input_plan.is_empty() {
                        self.request_managed_frame_capture_if_ready(&run_id);
                    }
                }
            }
            AiStudioRuntimeResult::PlayStopped => {
                self.managed_playtest_started_at = None;
                self.status = Some("Managed Play stopped.".to_owned());
            }
            AiStudioRuntimeResult::FrameCaptured(capture) => {
                match encode_agent_frame_png(&capture)
                    .map_err(|error| error.to_string())
                    .and_then(|png| self.host.store_captured_frame_artifact(
                        &run_id, capture.width, capture.height, &png,
                    ).map_err(|error| error.to_string()))
                {
                    Ok((artifact_id, path)) => {
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [capture.width as usize, capture.height as usize],
                            &capture.rgba8,
                        );
                        let texture = context.load_texture(
                            format!("ai-studio-{artifact_id}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.last_captured_frame = Some((texture, artifact_id.clone(), capture.width, capture.height));
                        self.managed_runtime_observation = Some(ManagedRuntimeObservation {
                            artifact_id: artifact_id.clone(),
                            path,
                            width: capture.width,
                            height: capture.height,
                        });
                        self.managed_capture_requested = false;
                        self.status = Some(format!("Captured managed Play frame {artifact_id}; scheduling provider evaluation."));
                        self.request_managed_runtime_evaluation_if_ready(&run_id);
                    }
                    Err(error) => {
                        self.managed_capture_requested = false;
                        let message = format!("Managed frame capture could not be stored: {error}");
                        if let Err(host_error) = self
                            .host
                            .record_frame_capture_failure(&run_id, message.clone())
                        {
                            self.status = Some(host_error.to_string());
                        } else {
                            self.status = Some(message);
                        }
                    }
                }
            }
            AiStudioRuntimeResult::Failed(message) => {
                let result = if self.managed_playtest_started_at.is_none() {
                    self.host
                        .record_playtest_result(&run_id, false, Some(false), message.clone())
                } else if self.managed_capture_requested {
                    self.managed_capture_requested = false;
                    self.host
                        .record_frame_capture_failure(&run_id, message.clone())
                } else {
                    self.host
                        .record_playtest_result(&run_id, true, Some(false), message.clone())
                };
                if let Err(error) = result {
                    self.status = Some(error.to_string());
                } else {
                    self.status = Some(message);
                }
            }
        }
    }

    /// Draws the AI Studio window and advances any active external agent process.
    pub fn show(&mut self, context: &egui::Context) {
        self.ensure_remote_gateway(context);
        self.poll_remote_requests();
        self.poll_native_question(context);
        self.poll_external_process(context);
        self.poll_managed_validation(context);
        self.request_managed_source_repair_if_ready();
        self.request_managed_runtime_repair_if_ready();
        self.request_managed_playtest_if_ready();
        self.request_next_managed_runtime_input_if_ready();
        self.poll_managed_playtest_timeout();
        let mut open = self.open;
        egui::Window::new("AI Studio")
            .id(egui::Id::new("gameengine_ai_studio"))
            .open(&mut open)
            .default_pos(egui::pos2(940.0, 84.0))
            .default_size(egui::vec2(600.0, 760.0))
            .min_width(460.0)
            .min_height(520.0)
            .resizable(true)
            .show(context, |ui| self.show_contents(ui));
        self.open = open;
    }

    fn ensure_remote_gateway(&mut self, context: &egui::Context) {
        if self.remote_server.is_some() {
            return;
        }
        match RemoteAiStudioServer::start(context.clone()) {
            Ok((server, requests)) => {
                self.remote_server = Some(server);
                self.remote_requests = Some(requests);
            }
            Err(error) => {
                self.status = Some(format!("Remote AI Studio gateway unavailable: {error}"));
            }
        }
    }

    fn poll_remote_requests(&mut self) {
        let requests = self
            .remote_requests
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for request in requests {
            let response = self.handle_remote_operation(request.operation().clone());
            request.respond(response);
        }
    }

    fn handle_remote_operation(&mut self, operation: RemoteOperation) -> RemoteAiStudioResponse {
        match operation {
            RemoteOperation::Sessions => {
                RemoteAiStudioResponse::json(sessions_json(&self.host, &self.project_id))
            }
            RemoteOperation::Snapshot { session_id } => {
                let pending = self.pending_permission.as_ref().map(|permission| {
                    (permission.run_id.as_str(), permission.capability)
                });
                match snapshot_json(&self.host, &self.project_id, &session_id, pending) {
                    Ok(snapshot) => RemoteAiStudioResponse::json(snapshot),
                    Err(error) => RemoteAiStudioResponse::error(404, "session_not_found", error, false),
                }
            }
            RemoteOperation::Message { session_id, text, .. } => {
                match self.host.append_message(&session_id, ConversationRole::User, text) {
                    Ok(()) => RemoteAiStudioResponse::json(serde_json::json!({"accepted": true})),
                    Err(error) => RemoteAiStudioResponse::error(404, "message_rejected", error.to_string(), false),
                }
            }
            RemoteOperation::Go {
                session_id,
                proposal_version,
                ..
            } => {
                let proposal = match self.host.session(&session_id) {
                    Ok(session) if session.proposal.version == proposal_version => session.proposal.clone(),
                    Ok(session) => {
                        return RemoteAiStudioResponse::error(
                            409,
                            "stale_proposal",
                            format!("Proposal version {proposal_version} is stale; current version is {}.", session.proposal.version),
                            false,
                        );
                    }
                    Err(error) => {
                        return RemoteAiStudioResponse::error(404, "session_not_found", error.to_string(), false);
                    }
                };
                self.selected_session = session_id;
                self.proposal_draft = proposal;
                match self.begin_run_authorized(proposal_version) {
                    Ok(run_id) => RemoteAiStudioResponse::json(serde_json::json!({"run_id": run_id})),
                    Err(error) => RemoteAiStudioResponse::error(409, "go_rejected", error, false),
                }
            }
            RemoteOperation::Stop { run_id, .. } => match self.stop_run_exact(&run_id) {
                Ok(()) => RemoteAiStudioResponse::json(serde_json::json!({"stopped": true, "run_id": run_id})),
                Err(error) => RemoteAiStudioResponse::error(409, "stop_rejected", error, false),
            },
            RemoteOperation::AwaitingUser { run_id, text, .. } => {
                let state = match self.host.run(&run_id) {
                    Ok(run) => run.state,
                    Err(error) => return RemoteAiStudioResponse::error(404, "run_not_found", error.to_string(), false),
                };
                if state != AgentRunState::AwaitingUser {
                    return RemoteAiStudioResponse::error(409, "not_awaiting_user", "The run is no longer waiting for user input.", false);
                }
                let session_id = self.host.session_ids().into_iter().find(|session_id| {
                    self.host.session(session_id).is_ok_and(|session| {
                        session.runs.iter().any(|run| run.id == run_id)
                    })
                });
                let Some(session_id) = session_id else {
                    return RemoteAiStudioResponse::error(404, "run_not_found", "The run session was not found.", false);
                };
                if let Err(error) = self.host.append_message(&session_id, ConversationRole::User, text) {
                    return RemoteAiStudioResponse::error(409, "response_rejected", error.to_string(), false);
                }
                match self.host.transition_run(&run_id, AgentRunState::Executing, "User response received; execution may continue.") {
                    Ok(()) => RemoteAiStudioResponse::json(serde_json::json!({"accepted": true, "run_id": run_id})),
                    Err(error) => RemoteAiStudioResponse::error(409, "response_rejected", error.to_string(), false),
                }
            }
            RemoteOperation::Permission {
                run_id, capability, scope, ..
            } => {
                let Some(pending) = self.pending_permission.as_ref() else {
                    return RemoteAiStudioResponse::error(409, "permission_not_pending", "No permission decision is pending.", false);
                };
                if pending.run_id != run_id || pending.capability != capability {
                    return RemoteAiStudioResponse::error(409, "permission_stale", "The permission request no longer matches the active decision.", false);
                }
                let action = pending.action;
                let approval = match scope {
                    RemotePermissionScope::Once => ApprovalScope::Once,
                    RemotePermissionScope::Run => ApprovalScope::Run,
                    RemotePermissionScope::Project => ApprovalScope::Project,
                    RemotePermissionScope::Deny => ApprovalScope::Deny,
                };
                self.resolve_pending_permission(&run_id, capability, action, approval);
                RemoteAiStudioResponse::json(serde_json::json!({"resolved": true, "run_id": run_id}))
            }
            RemoteOperation::Events { run_id, after } => match events_json(&self.host, &run_id, after) {
                Ok(events) => RemoteAiStudioResponse::sse(events),
                Err(error) => RemoteAiStudioResponse::error(404, "run_not_found", error, false),
            },
            RemoteOperation::Frame { run_id, artifact_id } => match frame_bytes(&self.host, &run_id, &artifact_id) {
                Ok(bytes) => RemoteAiStudioResponse::png(bytes),
                Err(error) => RemoteAiStudioResponse::error(404, "frame_not_found", error, false),
            },
            RemoteOperation::StartLiveObservation { run_id, max_fps, .. } => {
                let run = match self.host.run(&run_id) {
                    Ok(run) => run,
                    Err(error) => {
                        return RemoteAiStudioResponse::error(
                            404,
                            "run_not_found",
                            error.to_string(),
                            false,
                        );
                    }
                };
                if matches!(
                    run.state,
                    AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
                ) || self.active_run_id.as_deref() != Some(run_id.as_str())
                {
                    return RemoteAiStudioResponse::error(
                        409,
                        "live_observation_stale_run",
                        "Live observation can be started only for the current non-terminal AgentRun.",
                        false,
                    );
                }
                match self.live_observation.start(&run_id, max_fps) {
                    Ok(started) => RemoteAiStudioResponse::json(serde_json::json!({
                        "media_session_id": started.media_session_id,
                        "media_token": started.media_token,
                        "run_id": started.run_id,
                        "source": "game_view",
                        "codec": "png",
                        "max_fps": started.max_fps,
                        "max_dimensions": [1280, 720],
                        "retention": "latest_frame_only",
                    })),
                    Err(error) => live_observation_error_response(error),
                }
            }
            RemoteOperation::LiveObservationStatus {
                media_session_id,
                media_token,
            } => match self
                .live_observation
                .status_json(&media_session_id, &media_token)
            {
                Ok(status) => RemoteAiStudioResponse::json(status),
                Err(error) => live_observation_error_response(error),
            },
            RemoteOperation::LiveObservationFrame {
                media_session_id,
                media_token,
                sequence,
            } => match self
                .live_observation
                .frame_bytes(&media_session_id, &media_token, sequence)
            {
                Ok(bytes) => RemoteAiStudioResponse::png(bytes),
                Err(error) => live_observation_error_response(error),
            },
            RemoteOperation::StopLiveObservation {
                media_session_id,
                media_token,
                ..
            } => match self.live_observation.stop(&media_session_id, &media_token) {
                Ok(()) => RemoteAiStudioResponse::json(serde_json::json!({
                    "stopped": true,
                    "media_session_id": media_session_id,
                })),
                Err(error) => live_observation_error_response(error),
            },
        }
    }

    fn stop_run_exact(&mut self, run_id: &str) -> Result<(), String> {
        if self.active_run_id.as_deref() != Some(run_id) {
            return Err("The requested run is not the active run; stale Stop was rejected.".to_owned());
        }
        if self.host.run(run_id).is_ok_and(|run| matches!(run.state, AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled)) {
            return Ok(());
        }
        if let Some(process) = self.process.as_mut() {
            process.cancel().map_err(|error| format!("Could not stop agent process: {error}"))?;
        }
        self.process = None;
        self.process_purpose = None;
        self.host.cancel_run(run_id).map_err(|error| error.to_string())
    }

    fn show_remote_companion(&mut self, ui: &mut egui::Ui) {
        let Some(server) = self.remote_server.as_ref() else {
            return;
        };
        egui::CollapsingHeader::new("Remote companion")
            .default_open(false)
            .show(ui, |ui| {
                ui.small("Loopback-only companion gateway. Expose it only through a trusted private overlay or local reverse proxy. Remote authentication is separate from Agent Host permissions; MCP is never exposed remotely.");
                ui.label(format!("Gateway: {}", server.endpoint()));
                ui.monospace(server.companion_url());
            });
    }

    fn show_contents(&mut self, ui: &mut egui::Ui) {
        self.show_remote_companion(ui);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.strong("Conversation-first project agent");
            ui.separator();
            ui.label("Structured authoring stays on the Editor MCP host.");
        });
        ui.small(
            "External agent processes are application-level integrations, not an OS sandbox. Code is prepared in an isolated managed workspace and must be reviewed before apply.",
        );
        ui.separator();

        self.show_session_header(ui);
        self.show_conversation(ui);
        ui.separator();
        self.show_proposal(ui);
        ui.separator();
        self.show_provider(ui);
        self.show_permission_prompt(ui);
        self.show_code_changes(ui);
        self.show_run_timeline(ui);

        if let Some(status) = &self.status {
            ui.separator();
            ui.label(status);
        }
    }

    fn show_session_header(&mut self, ui: &mut egui::Ui) {
        let session_ids = self.host.session_ids();
        let current_title = self
            .host
            .session(&self.selected_session)
            .map(|session| session.title.clone())
            .unwrap_or_else(|_| "Unavailable session".to_owned());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("ai_studio_session")
                .selected_text(current_title)
                .width(260.0)
                .show_ui(ui, |ui| {
                    for id in session_ids {
                        let title = self
                            .host
                            .session(&id)
                            .map(|session| session.title.as_str())
                            .unwrap_or("Unavailable session");
                        if ui
                            .selectable_value(&mut self.selected_session, id.clone(), title)
                            .changed()
                            && let Ok(session) = self.host.session(&id)
                        {
                            self.proposal_draft = session.proposal.clone();
                        }
                    }
                });
            if ui.button("New session").clicked() {
                match self.host.create_session("New AI Studio session") {
                    Ok(id) => {
                        self.selected_session = id;
                        self.proposal_draft = AgentProposal::default();
                        self.status = Some("Created a private local AI session.".to_owned());
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            if ui.button("Share with project").clicked() {
                match self.host.export_shared_session(&self.selected_session) {
                    Ok(path) => {
                        let relative = path
                            .strip_prefix(&self.project_root)
                            .unwrap_or(path.as_path());
                        self.status = Some(format!(
                            "Wrote sanitized project-shared history to {}.",
                            relative.display()
                        ));
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
        });
    }

    fn show_conversation(&mut self, ui: &mut egui::Ui) {
        ui.heading("Conversation");
        let messages = self
            .host
            .session(&self.selected_session)
            .map(|session| session.messages.clone())
            .unwrap_or_default();
        egui::ScrollArea::vertical()
            .id_salt("ai_studio_conversation")
            .max_height(180.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if messages.is_empty() {
                    ui.weak("Describe what you want to build, change, inspect, or validate.");
                }
                for message in messages {
                    ui.group(|ui| {
                        ui.strong(match message.role {
                            ConversationRole::User => "You",
                            ConversationRole::Assistant => "Agent",
                            ConversationRole::System => "System",
                        });
                        ui.label(message.text);
                    });
                }
            });
        self.show_local_model_settings(ui);
        ui.add(
            egui::TextEdit::multiline(&mut self.message_draft)
                .desired_rows(2)
                .hint_text("Ask a question, add a constraint, or continue the same conversation…"),
        );
        ui.horizontal(|ui| {
            let can_send = !self.message_draft.trim().is_empty() && self.native_question.is_none();
            if ui.add_enabled(can_send, egui::Button::new("Send")).clicked() {
                let text = self.message_draft.trim().to_owned();
                match self.host.append_message(
                    &self.selected_session,
                    ConversationRole::User,
                    text,
                ) {
                    Ok(()) => {
                        self.message_draft.clear();
                        self.start_native_question();
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            if self.native_question.is_some() {
                ui.spinner();
                ui.small("Reading current GameEngine/project evidence…");
            } else if self.local_model_name.trim().is_empty() {
                ui.small("Set an installed local model to receive read-only answers.");
            } else {
                ui.small("Questions use the read-only native harness; Go remains explicit for writes.");
            }
        });
    }

    fn show_local_model_settings(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Local model · read-only questions")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Quality");
                    let previous = self.quality_preference;
                    for quality in QualityPreference::ALL {
                        ui.selectable_value(
                            &mut self.quality_preference,
                            quality,
                            quality.label(),
                        );
                    }
                    if self.quality_preference != previous {
                        self.save_preferences();
                    }
                });
                ui.small(
                    "Quality is a machine-local latency/reasoning preference. It never exposes GPU layers, quantization, token budgets, or VRAM reservations.",
                );
                ui.horizontal(|ui| {
                    ui.label("Endpoint");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.local_model_endpoint)
                                .desired_width(250.0),
                        )
                        .changed()
                    {
                        self.save_preferences();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Installed model");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut self.local_model_name)
                                .desired_width(250.0)
                                .hint_text("model:tag"),
                        )
                        .changed()
                    {
                        self.save_preferences();
                    }
                });
                let profile = LocalModelConfig {
                    endpoint: self.local_model_endpoint.clone(),
                    model: self.local_model_name.clone(),
                }
                .capability_profile();
                ui.small(model_capability_summary(&profile));
                ui.small(format!(
                    "Resource controls: unload/reload {} · GPU residency {} · memory telemetry {}",
                    capability_label(profile.resource_capabilities.unload_reload),
                    capability_label(profile.resource_capabilities.gpu_residency),
                    capability_label(profile.resource_capabilities.backend_memory_telemetry),
                ));
                ui.small(format!(
                    "Resource posture: {:?} · workload {:?} · reclaim {:?}",
                    self.resource_plan.presentation,
                    self.resolved_workload,
                    self.resource_plan.reclaim
                ));
                ui.small(
                    "The initial backend accepts loopback HTTP only. Unsupported controls remain unavailable; exact VRAM and TTFT are never fabricated.",
                );
            });
    }

    fn start_native_question(&mut self) {
        if self.local_model_name.trim().is_empty() {
            return;
        }
        let session_id = self.selected_session.clone();
        let conversation = match self.host.session(&session_id) {
            Ok(session) => session
                .messages
                .iter()
                .map(|message| QuestionMessage {
                    role: match message.role {
                        ConversationRole::User => QuestionRole::User,
                        ConversationRole::Assistant => QuestionRole::Assistant,
                        ConversationRole::System => QuestionRole::System,
                    },
                    text: message.text.clone(),
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                self.status = Some(error.to_string());
                return;
            }
        };
        let config = LocalModelConfig {
            endpoint: self.local_model_endpoint.clone(),
            model: self.local_model_name.clone(),
        };
        let profile = config.capability_profile();
        let has_non_trivial_work = !self.proposal_draft.planned_code_changes.is_empty()
            || !self.proposal_draft.planned_project_changes.is_empty()
            || !self.proposal_draft.planned_assets.is_empty();
        self.resolved_workload = classify_workload(WorkloadSignals {
            strong_reasoning_required: has_non_trivial_work
                && matches!(
                    self.quality_preference,
                    QualityPreference::Balanced | QualityPreference::Deep
                ),
            model_judgement_required: true,
            ..WorkloadSignals::default()
        });
        self.resource_plan = resolve_resource_plan(
            self.resolved_workload,
            self.quality_preference,
            MemoryPressure::Unknown,
            profile.resource_capabilities,
        );
        if self.resource_plan.presentation == PresentationPosture::InferenceFocused {
            self.pending_native_question_start = Some((config, conversation, session_id));
            self.pending_runtime_action = Some(AiStudioRuntimeAction::EnterInferenceFocused {
                reclaim: self.resource_plan.reclaim.into(),
            });
            self.status = Some(
                "Preparing InferenceFocused presentation before native inference.".to_owned(),
            );
        } else {
            self.spawn_native_question(config, conversation, session_id);
        }
    }

    fn spawn_native_question(
        &mut self,
        config: LocalModelConfig,
        conversation: Vec<QuestionMessage>,
        session_id: String,
    ) {
        match NativeQuestionTask::spawn(config, self.project_root.clone(), conversation) {
            Ok(task) => {
                self.native_question = Some(task);
                self.native_question_session = Some(session_id);
                self.status = Some(format!(
                    "Native inference started with {:?} workload and {:?} quality.",
                    self.resolved_workload, self.quality_preference
                ));
            }
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn save_preferences(&mut self) {
        let preferences = AiStudioPreferences {
            schema_version: AI_STUDIO_PREFERENCES_SCHEMA_VERSION,
            quality_preference: self.quality_preference,
            local_model_endpoint: self.local_model_endpoint.clone(),
            local_model_name: self.local_model_name.clone(),
        };
        match serde_json::to_vec_pretty(&preferences) {
            Ok(bytes) => {
                if let Some(parent) = self.preferences_path.parent()
                    && let Err(error) = fs::create_dir_all(parent)
                {
                    self.status = Some(format!("Could not save AI Studio preferences: {error}"));
                    return;
                }
                if let Err(error) = fs::write(&self.preferences_path, bytes) {
                    self.status = Some(format!("Could not save AI Studio preferences: {error}"));
                }
            }
            Err(error) => {
                self.status = Some(format!("Could not serialize AI Studio preferences: {error}"));
            }
        }
    }

    fn poll_native_question(&mut self, context: &egui::Context) {
        let Some(task) = self.native_question.as_ref() else {
            return;
        };
        let Some(result) = task.poll() else {
            context.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        self.native_question = None;
        let restore_after_inference = self.resource_plan.presentation
            == PresentationPosture::InferenceFocused
            && !self.editing_interrupted
            && self.pending_runtime_action.is_none();
        let session_id = self
            .native_question_session
            .take()
            .unwrap_or_else(|| self.selected_session.clone());
        match result {
            Ok(answer) => {
                let message = format_native_answer(&answer);
                match self.host.append_message(
                    &session_id,
                    ConversationRole::Assistant,
                    message,
                ) {
                    Ok(()) => {
                        self.status = Some(format!(
                            "Local model answered with {} retrieved evidence source(s) in {} ms.",
                            answer.sources.len(), answer.metrics.elapsed_ms
                        ));
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            Err(error) => self.status = Some(error.to_string()),
        }
        if restore_after_inference {
            self.pending_runtime_action = Some(AiStudioRuntimeAction::RestoreEditorPresentation);
        }
    }

    fn show_proposal(&mut self, ui: &mut egui::Ui) {
        let current_version = self
            .host
            .session(&self.selected_session)
            .map(|session| session.proposal.version)
            .unwrap_or(self.proposal_draft.version);
        egui::CollapsingHeader::new(format!("Structured proposal · v{current_version}"))
            .default_open(true)
            .show(ui, |ui| {
                ui.label("Goal");
                ui.text_edit_singleline(&mut self.proposal_draft.goal);
                edit_lines(ui, "Requirements", &mut self.proposal_draft.requirements);
                edit_lines(ui, "Assumptions", &mut self.proposal_draft.assumptions);
                edit_lines(
                    ui,
                    "Acceptance criteria",
                    &mut self.proposal_draft.acceptance_criteria,
                );
                edit_lines(
                    ui,
                    "Planned project changes",
                    &mut self.proposal_draft.planned_project_changes,
                );
                edit_lines(
                    ui,
                    "Planned code changes",
                    &mut self.proposal_draft.planned_code_changes,
                );
                edit_lines(ui, "Planned assets", &mut self.proposal_draft.planned_assets);
                edit_lines(ui, "Validation plan", &mut self.proposal_draft.validation_plan);
                edit_lines(ui, "Playtest plan", &mut self.proposal_draft.playtest_plan);
                if ui.button("Save proposal version").clicked() {
                    match self
                        .host
                        .update_proposal(&self.selected_session, self.proposal_draft.clone())
                    {
                        Ok(version) => {
                            self.proposal_draft.version = version;
                            self.status = Some(format!("Saved proposal version {version}."));
                        }
                        Err(error) => self.status = Some(error.to_string()),
                    }
                }
            });
    }

    fn show_provider(&mut self, ui: &mut egui::Ui) {
        ui.heading("Run");
        ui.horizontal(|ui| {
            ui.label("Compatible agent program");
            ui.text_edit_singleline(&mut self.provider_program);
        });
        ui.horizontal(|ui| {
            ui.label("Arguments");
            ui.text_edit_singleline(&mut self.provider_args);
        });
        ui.small(
            "The write-capable Go path launches an external AgentRuntime directly without a shell and injects the immutable proposal plus ephemeral Editor MCP connection. The local question backend above is a separate ModelBackend; a native write-capable tool loop is not enabled by this slice.",
        );
        let mut stop_requested = false;
        let mut interrupt_requested = false;
        let mut resume_requested = false;
        ui.horizontal_wrapped(|ui| {
            let can_go = self.process.is_none()
                && self.pending_permission.is_none()
                && !self.provider_program.trim().is_empty();
            if ui.add_enabled(can_go, egui::Button::new("Go")).clicked() {
                self.begin_run();
            }
            let can_stop = self.active_run_id.as_ref().is_some_and(|run_id| {
                self.host.run(run_id).is_ok_and(|run| {
                    !matches!(
                        run.state,
                        AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
                    )
                })
            });
            if ui.add_enabled(can_stop, egui::Button::new("Stop")).clicked() {
                stop_requested = true;
            }
            let can_interrupt = !self.editing_interrupted
                && self.pending_runtime_action.is_none()
                && (self.native_question.is_some()
                    || self.resource_plan.presentation == PresentationPosture::InferenceFocused
                    || self.active_run_id.as_ref().is_some_and(|run_id| {
                        self.host.run(run_id).is_ok_and(|run| {
                            !matches!(
                                run.state,
                                AgentRunState::Completed
                                    | AgentRunState::Failed
                                    | AgentRunState::Cancelled
                                    | AgentRunState::InterruptedForEditing
                            )
                        })
                    }));
            if ui
                .add_enabled(can_interrupt, egui::Button::new("Interrupt for Editing"))
                .clicked()
            {
                interrupt_requested = true;
            }
            if ui
                .add_enabled(
                    self.editing_interrupted && self.pending_runtime_action.is_none(),
                    egui::Button::new("Resume"),
                )
                .clicked()
            {
                resume_requested = true;
            }
        });
        if stop_requested {
            let run_id = self.active_run_id.clone();
            if let Some(task) = self.native_question.as_ref() {
                task.interrupt();
            }
            self.pending_native_question_start = None;
            if let Some(process) = self.process.as_mut()
                && let Err(error) = process.cancel()
            {
                self.status = Some(format!("Could not stop agent process: {error}"));
            }
            self.process = None;
            self.process_purpose = None;
            if let Some(run_id) = run_id
                && let Err(error) = self.host.cancel_run(&run_id)
            {
                self.status = Some(error.to_string());
            }
        }
        if interrupt_requested {
            if let Some(task) = self.native_question.as_ref() {
                task.interrupt();
            }
            self.pending_native_question_start = None;
            self.restore_for_editing = true;
            self.pending_runtime_action = Some(AiStudioRuntimeAction::RestoreEditorPresentation);
            self.status = Some(
                "Stopping inference at a safe backend boundary and restoring Editor presentation..."
                    .to_owned(),
            );
        }
        if resume_requested {
            self.pending_runtime_action = Some(AiStudioRuntimeAction::InspectAuthoritativeState);
            self.status = Some(
                "Re-inspecting authoritative Editor state before Resume...".to_owned(),
            );
        }
    }

    fn show_permission_prompt(&mut self, ui: &mut egui::Ui) {
        let Some(pending) = self.pending_permission.as_ref() else {
            return;
        };
        let run_id = pending.run_id.clone();
        let capability = pending.capability;
        let action = pending.action;
        ui.separator();
        ui.group(|ui| {
            ui.strong("Approval required");
            ui.label(capability.label());
            ui.small("Project-level approval persists as user application state; credentials never do.");
            ui.horizontal(|ui| {
                for (label, scope) in [
                    ("Allow once", ApprovalScope::Once),
                    ("This run", ApprovalScope::Run),
                    ("This project", ApprovalScope::Project),
                    ("Deny", ApprovalScope::Deny),
                ] {
                    if ui.button(label).clicked() {
                        self.resolve_pending_permission(&run_id, capability, action, scope);
                    }
                }
            });
        });
    }

    fn show_code_changes(&mut self, ui: &mut egui::Ui) {
        if self.pending_code_changes.is_empty() {
            return;
        }
        ui.separator();
        egui::CollapsingHeader::new(format!(
            "Managed code diff · {} file(s)",
            self.pending_code_changes.len()
        ))
        .default_open(true)
        .show(ui, |ui| {
            for change in &self.pending_code_changes {
                ui.horizontal(|ui| {
                    ui.monospace(change.relative_path.display().to_string());
                    ui.weak(change_summary(change));
                });
            }
            let can_apply = self.pending_permission.is_none()
                && self.code_workspace.is_some()
                && self.active_run_id.is_some();
            if ui
                .add_enabled(can_apply, egui::Button::new("Review complete — apply code changes"))
                .clicked()
            {
                self.request_code_apply();
            }
            ui.small(
                "Only game/** and assets/scripts/{rust,rhai}/** are eligible. Deletions and stale live files are rejected rather than forced.",
            );
        });
    }

    fn show_run_timeline(&mut self, ui: &mut egui::Ui) {
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let Ok(run) = self.host.run(&run_id).cloned() else {
            return;
        };
        ui.separator();
        egui::CollapsingHeader::new(format!("Run timeline · {:?}", run.state))
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("Proposal v{}", run.proposal_snapshot.version));
                    ui.label(format!("Provider: {}", run.provider_label));
                });
                egui::ScrollArea::vertical()
                    .id_salt("ai_studio_timeline")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for event in run.events.iter().rev().take(120).rev() {
                            ui.horizontal_wrapped(|ui| {
                                ui.monospace(format!("#{:03}", event.sequence));
                                ui.strong(format!("{:?}", event.kind));
                                ui.label(&event.message);
                            });
                        }
                    });
                self.show_completion_contract(ui, &run_id, run.completion);
            });
    }

    fn show_completion_contract(
        &mut self,
        ui: &mut egui::Ui,
        run_id: &str,
        report: crate::agent_host::CompletionReport,
    ) {
        ui.separator();
        ui.strong("Completion contract");
        completion_row(ui, "Acceptance criteria", report.acceptance_criteria);
        completion_row(ui, "Authoring validation", report.authoring_validation);
        completion_row(ui, "Source validation", report.source_validation);
        completion_row(ui, "Play launch", report.play_launch);
        completion_row(ui, "Frame capture", report.frame_capture);
        completion_row(ui, "Visual evaluation", report.visual_evaluation);
        completion_row(ui, "Interaction scenarios", report.interaction_scenarios);
        ui.small("Completion evidence is owned by the agent host and managed engine services.");
        ui.horizontal(|ui| {
            let can_capture = self.managed_playtest_started_at.is_some()
                && !self.managed_capture_requested
                && self.pending_permission.is_none()
                && self.pending_runtime_action.is_none();
            if ui.add_enabled(can_capture, egui::Button::new("Capture managed frame")).clicked() {
                self.managed_capture_requested = true;
                self.request_permission(run_id.to_owned(), AgentCapability::FrameCapture, PendingPermissionAction::CaptureFrame);
            }
            if ui.add_enabled(self.managed_playtest_started_at.is_some(), egui::Button::new("Stop managed Play")).clicked() {
                self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
            }
        });
        if let Some((texture, artifact_id, width, height)) = &self.last_captured_frame {
            ui.group(|ui| {
                ui.strong(format!("Captured frame · {artifact_id} · {width}x{height}"));
                ui.add(egui::Image::new(texture).fit_to_exact_size(egui::vec2(480.0, 270.0)).maintain_aspect_ratio(true));
            });
        }
        if ui.button("Complete run").clicked() {
            match self.host.complete_run(run_id) {
                Ok(()) => self.status = Some("Run completion contract satisfied.".to_owned()),
                Err(error) => self.status = Some(error.to_string()),
            }
        }
    }

    fn begin_run(&mut self) {
        let authorized_proposal_version = match self
            .host
            .update_proposal(&self.selected_session, self.proposal_draft.clone())
        {
            Ok(version) => {
                self.proposal_draft.version = version;
                version
            }
            Err(error) => {
                self.status = Some(error.to_string());
                return;
            }
        };
        if let Err(error) = self.begin_run_authorized(authorized_proposal_version) {
            self.status = Some(error);
        }
    }

    fn begin_run_authorized(&mut self, authorized_proposal_version: u64) -> Result<String, String> {
        let provider = self.provider_program.trim().to_owned();
        if provider.is_empty() {
            return Err("Configure a compatible agent program locally before Go.".to_owned());
        }
        let run_id = self
            .host
            .start_run_authorized(&self.selected_session, authorized_proposal_version, provider)
            .map_err(|error| error.to_string())?;
        self.active_run_id = Some(run_id.clone());
        self.pending_runtime_action = None;
        self.managed_input_plan.clear();
        self.managed_input_recipe.clear();
        self.managed_candidate_input_recipe.clear();
        self.managed_playtest_requested = false;
        self.managed_capture_requested = false;
        self.managed_repair_requested = false;
        self.managed_runtime_repairs = 0;
        self.managed_runtime_observation = None;
        self.managed_evaluation_requested = false;
        self.managed_playtest_started_at = None;
        self.last_captured_frame = None;
        self.request_permission(
            run_id.clone(),
            AgentCapability::ExternalAgentProcess,
            PendingPermissionAction::LaunchExternalAgent,
        );
        Ok(run_id)
    }

    fn request_code_apply(&mut self) {
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        self.request_permission(
            run_id,
            AgentCapability::CodeWorkspaceApply,
            PendingPermissionAction::ApplyCodeChanges,
        );
    }

    fn request_permission(
        &mut self,
        run_id: String,
        capability: AgentCapability,
        action: PendingPermissionAction,
    ) {
        match self.host.check_permission(&run_id, capability) {
            Ok(PermissionCheck::Granted) => self.execute_permission_action(&run_id, action),
            Ok(PermissionCheck::RequiresApproval) => {
                self.pending_permission = Some(PendingPermission {
                    run_id,
                    capability,
                    action,
                });
            }
            Ok(PermissionCheck::Denied) => {
                self.status = Some(format!("Permission denied: {}.", capability.label()));
            }
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn resolve_pending_permission(
        &mut self,
        run_id: &str,
        capability: AgentCapability,
        action: PendingPermissionAction,
        scope: ApprovalScope,
    ) {
        self.pending_permission = None;
        if let Err(error) = self
            .host
            .resolve_permission(run_id, capability, scope)
        {
            self.status = Some(error.to_string());
            return;
        }
        if scope == ApprovalScope::Deny {
            self.status = Some(format!("Denied {}.", capability.label()));
            return;
        }
        match self.host.check_permission(run_id, capability) {
            Ok(PermissionCheck::Granted) => self.execute_permission_action(run_id, action),
            Ok(_) => self.status = Some("Permission was not granted.".to_owned()),
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn execute_permission_action(&mut self, run_id: &str, action: PendingPermissionAction) {
        match action {
            PendingPermissionAction::LaunchExternalAgent => {
                self.launch_external_agent(run_id, ExternalAgentPurpose::BuildOrRepair)
            }
            PendingPermissionAction::LaunchRuntimeEvaluation => {
                self.launch_external_agent(run_id, ExternalAgentPurpose::RuntimeEvaluation)
            }
            PendingPermissionAction::ApplyCodeChanges => self.apply_code_changes(run_id),
            PendingPermissionAction::LaunchPlaytest => {
                self.pending_runtime_action = Some(AiStudioRuntimeAction::StartPlaytest);
            }
            PendingPermissionAction::SendRuntimeInput(command) => {
                self.pending_runtime_action = Some(AiStudioRuntimeAction::SendInput(command));
            }
            PendingPermissionAction::CaptureFrame => {
                self.pending_runtime_action = Some(AiStudioRuntimeAction::CaptureFrame);
            }
        }
    }

    fn launch_external_agent(&mut self, run_id: &str, purpose: ExternalAgentPurpose) {
        let (workspace_root, baseline_path) = match self.host.workspace_paths(run_id) {
            Ok(paths) => paths,
            Err(error) => {
                self.fail_run(run_id, format!("Could not resolve code workspace: {error}"));
                return;
            }
        };
        let workspace = match CodeWorkspace::open_or_create(
            &self.project_root,
            workspace_root,
            baseline_path,
        ) {
            Ok(workspace) => workspace,
            Err(error) => {
                self.fail_run(run_id, format!("Could not prepare code workspace: {error}"));
                return;
            }
        };
        if let Err(error) = self.host.record_event(
            run_id,
            AgentEventKind::CodeWorkspacePrepared,
            "Prepared isolated managed code workspace.",
        ) {
            self.status = Some(error.to_string());
        }
        let proposal_json = match self.host.run(run_id).and_then(|run| {
            serde_json::to_string(&run.proposal_snapshot).map_err(Into::into)
        }) {
            Ok(json) => json,
            Err(error) => {
                self.fail_run(run_id, format!("Could not serialize proposal: {error}"));
                return;
            }
        };
        if purpose == ExternalAgentPurpose::BuildOrRepair {
            self.managed_candidate_input_recipe.clear();
        }
        let repair_context = (purpose == ExternalAgentPurpose::BuildOrRepair)
            .then(|| {
                self.host.run(run_id).ok().and_then(|run| {
                    (run.state == AgentRunState::Repairing).then(|| {
                        managed_repair_context(run, self.managed_runtime_observation.as_ref())
                    })
                })
            })
            .flatten();
        let runtime_observation = if purpose == ExternalAgentPurpose::RuntimeEvaluation {
            match self.managed_runtime_observation.clone() {
                Some(observation) => Some(observation),
                None => {
                    self.managed_evaluation_requested = false;
                    self.status = Some("Runtime evaluation requires a host-captured frame artifact.".to_owned());
                    return;
                }
            }
        } else {
            None
        };
        let mut environment = vec![
            (
                OsString::from("GAMEENGINE_MCP_ENDPOINT"),
                OsString::from(&self.connection.endpoint),
            ),
            (
                OsString::from("GAMEENGINE_MCP_AUTH_TOKEN"),
                OsString::from(&self.connection.authorization_token),
            ),
            (
                OsString::from("GAMEENGINE_AGENT_RUN_ID"),
                OsString::from(run_id),
            ),
            (
                OsString::from("GAMEENGINE_AGENT_PROPOSAL_JSON"),
                OsString::from(proposal_json),
            ),
            (
                OsString::from("GAMEENGINE_AGENT_AUTHORING_CONTRACT"),
                OsString::from(
                    "Use the injected Editor MCP endpoint for persisted authoring changes. Use this isolated workspace only for project code and return code changes for review.",
                ),
            ),
            (
                OsString::from("GAMEENGINE_AGENT_EVENT_PROTOCOL"),
                OsString::from(
                    "Emit semantic events as one stdout line prefixed GAMEENGINE_AGENT_EVENT followed by JSON. Supported types: progress, tool_action, completion_gate, playtest_result, runtime_input. runtime_input is a provider-planned interaction and is executed later only through the authorized Editor Play virtual-input path. Never emit credentials or bearer tokens.",
                ),
            ),
        ];
        if let Some(repair_context) = repair_context {
            environment.push((
                OsString::from("GAMEENGINE_AGENT_REPAIR_CONTEXT"),
                OsString::from(repair_context),
            ));
        }
        if let Some(observation) = runtime_observation {
            environment.push((
                OsString::from("GAMEENGINE_AGENT_CAPTURE_PATH"),
                observation.path.as_os_str().to_os_string(),
            ));
            environment.push((
                OsString::from("GAMEENGINE_AGENT_RUNTIME_EVALUATION_CONTEXT"),
                OsString::from(format!(
                    "Evaluate host-captured managed Play frame {} ({}x{}). Inspect the image at GAMEENGINE_AGENT_CAPTURE_PATH. Do not mutate project or workspace state during this evaluation. Emit completion_gate with gate=visual_evaluation and status=passed or failed before any failing playtest_result, then emit playtest_result for the exercised interaction scenario when evidence supports it. A pass without this host-owned frame is rejected.",
                    observation.artifact_id, observation.width, observation.height
                )),
            ));
        }
        let args = split_direct_args(&self.provider_args);
        match ExternalAgentProcess::spawn(
            OsStr::new(self.provider_program.trim()),
            &args,
            workspace.root(),
            &environment,
        ) {
            Ok(process) => {
                self.code_workspace = Some(workspace);
                self.process = Some(process);
                self.process_purpose = Some(purpose);
                match purpose {
                    ExternalAgentPurpose::BuildOrRepair => {
                        if let Err(error) = self.host.transition_run(
                            run_id,
                            AgentRunState::Executing,
                            "External agent runtime started in the isolated code workspace.",
                        ) {
                            self.status = Some(error.to_string());
                        } else {
                            self.status = Some("External agent runtime started.".to_owned());
                        }
                    }
                    ExternalAgentPurpose::RuntimeEvaluation => {
                        if let Err(error) = self.host.record_semantic_progress(
                            run_id,
                            "runtime_evaluation",
                            "External agent runtime started against the host-captured managed Play frame.",
                        ) {
                            self.status = Some(error.to_string());
                        } else {
                            self.status = Some("External agent runtime is evaluating the captured managed Play frame.".to_owned());
                        }
                    }
                }
            }
            Err(error) => match purpose {
                ExternalAgentPurpose::BuildOrRepair => {
                    self.fail_run(run_id, format!("Could not launch external agent: {error}"));
                }
                ExternalAgentPurpose::RuntimeEvaluation => {
                    self.record_runtime_evaluation_failure(
                        run_id,
                        format!("Could not launch runtime evaluator: {error}"),
                    );
                }
            },
        }
    }

    fn poll_external_process(&mut self, context: &egui::Context) {
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let output = self
            .process
            .as_ref()
            .map(ExternalAgentProcess::drain_output)
            .unwrap_or_default();
        for line in output {
            if line.stream == ProcessStream::Stdout
                && let Some(payload) = line.text.strip_prefix(PROVIDER_EVENT_PREFIX)
            {
                match serde_json::from_str::<ProviderAgentEvent>(payload) {
                    Ok(event) => {
                        if let Err(error) = self.record_provider_semantic_event(&run_id, event) {
                            self.status = Some(error);
                        }
                        continue;
                    }
                    Err(error) => {
                        self.status = Some(format!("Provider emitted an invalid semantic AgentEvent: {error}"));
                    }
                }
            }
            let stream = match line.stream {
                ProcessStream::Stdout => "stdout",
                ProcessStream::Stderr => "stderr",
            };
            if let Err(error) = self.host.record_event(
                &run_id,
                AgentEventKind::ProviderOutput,
                format!("{stream} output received; raw provider text omitted from persisted history."),
            ) {
                self.status = Some(error.to_string());
            }
        }
        let exit = match self.process.as_mut() {
            Some(process) => process.poll_exit(),
            None => return,
        };
        match exit {
            Ok(None) => context.request_repaint_after(std::time::Duration::from_millis(100)),
            Ok(Some(status)) => {
                self.process = None;
                let purpose = self
                    .process_purpose
                    .take()
                    .unwrap_or(ExternalAgentPurpose::BuildOrRepair);
                if status.success() {
                    match purpose {
                        ExternalAgentPurpose::BuildOrRepair => {
                            self.finish_provider_execution(&run_id, status.code());
                        }
                        ExternalAgentPurpose::RuntimeEvaluation => {
                            self.finish_runtime_evaluation(&run_id, status.code());
                        }
                    }
                } else {
                    let message = format!(
                        "External agent exited unsuccessfully with {:?}.",
                        status.code()
                    );
                    match purpose {
                        ExternalAgentPurpose::BuildOrRepair => self.fail_run(&run_id, message),
                        ExternalAgentPurpose::RuntimeEvaluation => {
                            self.record_runtime_evaluation_failure(&run_id, message);
                        }
                    }
                }
            }
            Err(error) => {
                self.process = None;
                let purpose = self
                    .process_purpose
                    .take()
                    .unwrap_or(ExternalAgentPurpose::BuildOrRepair);
                let message = format!("Could not poll external agent: {error}");
                match purpose {
                    ExternalAgentPurpose::BuildOrRepair => self.fail_run(&run_id, message),
                    ExternalAgentPurpose::RuntimeEvaluation => {
                        self.record_runtime_evaluation_failure(&run_id, message);
                    }
                }
            }
        }
    }

    fn request_managed_frame_capture_if_ready(&mut self, run_id: &str) {
        if self.managed_capture_requested
            || self.managed_evaluation_requested
            || self.process.is_some()
            || self.pending_permission.is_some()
            || self.pending_runtime_action.is_some()
            || self.managed_playtest_started_at.is_none()
        {
            return;
        }
        self.managed_capture_requested = true;
        self.request_permission(
            run_id.to_owned(),
            AgentCapability::FrameCapture,
            PendingPermissionAction::CaptureFrame,
        );
    }

    fn request_managed_runtime_evaluation_if_ready(&mut self, run_id: &str) {
        if self.managed_evaluation_requested
            || self.process.is_some()
            || self.pending_permission.is_some()
            || self.managed_runtime_observation.is_none()
        {
            return;
        }
        if !self
            .host
            .run(run_id)
            .is_ok_and(|run| run.state == AgentRunState::Evaluating)
        {
            return;
        }
        self.managed_evaluation_requested = true;
        self.request_permission(
            run_id.to_owned(),
            AgentCapability::ExternalAgentProcess,
            PendingPermissionAction::LaunchRuntimeEvaluation,
        );
    }

    fn request_managed_source_repair_if_ready(&mut self) {
        if self.managed_repair_requested
            || self.process.is_some()
            || self.pending_permission.is_some()
        {
            return;
        }
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let decision = match self.host.run(&run_id) {
            Ok(run) => source_repair_decision(
                run.state,
                run.completion.source_validation,
                run.validation_attempts
                    .iter()
                    .filter(|attempt| attempt.status == ManagedValidationAttemptStatus::Failed)
                    .count(),
            ),
            Err(error) => {
                self.status = Some(error.to_string());
                return;
            }
        };
        match decision {
            SourceRepairDecision::Wait => {}
            SourceRepairDecision::Retry(repair_number) => {
                self.managed_repair_requested = true;
                if let Err(error) = self.host.record_semantic_progress(
                    &run_id,
                    "managed_source_repair",
                    format!(
                        "Starting autonomous source repair {repair_number}/{MAX_AUTONOMOUS_SOURCE_REPAIRS} from the latest managed validation failure."
                    ),
                ) {
                    self.status = Some(error.to_string());
                }
                self.request_permission(
                    run_id,
                    AgentCapability::ExternalAgentProcess,
                    PendingPermissionAction::LaunchExternalAgent,
                );
            }
            SourceRepairDecision::Exhausted => {
                self.managed_repair_requested = true;
                let message = format!(
                    "Autonomous source repair budget exhausted after {MAX_AUTONOMOUS_SOURCE_REPAIRS} repair attempt(s); source validation remains failed."
                );
                let _ = self.host.record_semantic_progress(
                    &run_id,
                    "managed_source_repair_exhausted",
                    message.clone(),
                );
                self.status = Some(message);
            }
        }
    }

    fn request_managed_runtime_repair_if_ready(&mut self) {
        if self.managed_repair_requested
            || self.process.is_some()
            || self.pending_permission.is_some()
            || self.pending_runtime_action.is_some()
            || self.managed_playtest_started_at.is_some()
        {
            return;
        }
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let decision = match self.host.run(&run_id) {
            Ok(run) => runtime_repair_decision(
                run.state,
                run.completion.source_validation,
                run.completion.play_launch,
                run.completion.frame_capture,
                run.completion.visual_evaluation,
                run.completion.interaction_scenarios,
                self.managed_runtime_repairs,
            ),
            Err(error) => {
                self.status = Some(error.to_string());
                return;
            }
        };
        match decision {
            RuntimeRepairDecision::Wait => {}
            RuntimeRepairDecision::Retry(repair_number) => {
                self.managed_runtime_repairs = repair_number;
                self.managed_repair_requested = true;
                self.managed_playtest_requested = false;
                self.managed_capture_requested = false;
                self.managed_evaluation_requested = false;
                if let Err(error) = self.host.record_semantic_progress(
                    &run_id,
                    "managed_runtime_repair",
                    format!(
                        "Starting autonomous runtime repair {repair_number}/{MAX_AUTONOMOUS_RUNTIME_REPAIRS} after managed Play or visual evaluation failure."
                    ),
                ) {
                    self.status = Some(error.to_string());
                }
                self.request_permission(
                    run_id,
                    AgentCapability::ExternalAgentProcess,
                    PendingPermissionAction::LaunchExternalAgent,
                );
            }
            RuntimeRepairDecision::Exhausted => {
                self.managed_repair_requested = true;
                let message = format!(
                    "Autonomous runtime repair budget exhausted after {MAX_AUTONOMOUS_RUNTIME_REPAIRS} repair attempt(s); managed Play or visual evaluation remains failed."
                );
                let _ = self.host.record_semantic_progress(
                    &run_id,
                    "managed_runtime_repair_exhausted",
                    message.clone(),
                );
                self.status = Some(message);
            }
        }
    }

    fn request_managed_playtest_if_ready(&mut self) {
        if self.managed_playtest_requested || self.pending_runtime_action.is_some() {
            return;
        }
        let Some(run_id) = self.active_run_id.clone() else { return; };
        if !self.host.run(&run_id).is_ok_and(|run| run.state == AgentRunState::Playtesting) {
            return;
        }
        self.managed_input_plan = self.managed_input_recipe.iter().cloned().collect();
        self.managed_capture_requested = false;
        self.managed_evaluation_requested = false;
        self.managed_runtime_observation = None;
        self.managed_playtest_requested = true;
        self.request_permission(run_id, AgentCapability::RuntimeLaunch, PendingPermissionAction::LaunchPlaytest);
    }

    fn request_next_managed_runtime_input_if_ready(&mut self) {
        if self.managed_playtest_started_at.is_none()
            || self.pending_permission.is_some()
            || self.pending_runtime_action.is_some()
        {
            return;
        }
        let Some(command) = self.managed_input_plan.pop_front() else {
            return;
        };
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        self.request_permission(
            run_id,
            AgentCapability::RuntimeInputControl,
            PendingPermissionAction::SendRuntimeInput(command),
        );
    }

    fn poll_managed_playtest_timeout(&mut self) {
        let Some(started_at) = self.managed_playtest_started_at else { return; };
        if started_at.elapsed() < std::time::Duration::from_secs(120) {
            return;
        }
        let Some(run_id) = self.active_run_id.clone() else { return; };
        let state = self.host.run(&run_id).map(|run| run.state).ok();
        if matches!(state, Some(AgentRunState::Playtesting | AgentRunState::Evaluating)) {
            let _ = self.host.record_playtest_result(
                &run_id,
                true,
                Some(false),
                "Managed playtest exceeded the 120 second first-release timeout before required evidence completed.",
            );
        }
        self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
        self.managed_playtest_started_at = None;
        self.status = Some("Managed Play stopped at the 120 second timeout.".to_owned());
    }

    fn poll_managed_validation(&mut self, context: &egui::Context) {
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        match self.host.poll_managed_validation(&run_id) {
            Ok(true) => context.request_repaint_after(std::time::Duration::from_millis(100)),
            Ok(false) => {}
            Err(error) => self.fail_run(
                &run_id,
                format!("Could not advance engine-managed validation: {error}"),
            ),
        }
    }

    fn record_runtime_evaluation_failure(&mut self, run_id: &str, message: String) {
        self.managed_evaluation_requested = false;
        if let Err(error) = self.host.record_completion_gate(
            run_id,
            "visual_evaluation",
            CompletionStatus::Failed,
            message.clone(),
        ) {
            self.status = Some(error.to_string());
        } else {
            self.status = Some(message);
        }
        if self.managed_playtest_started_at.is_some() && self.pending_runtime_action.is_none() {
            self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
        }
    }

    fn finish_runtime_evaluation(&mut self, run_id: &str, exit_code: Option<i32>) {
        self.managed_evaluation_requested = false;
        let visual_status = self
            .host
            .run(run_id)
            .map(|run| run.completion.visual_evaluation)
            .unwrap_or(CompletionStatus::Pending);
        match visual_status {
            CompletionStatus::Passed => {
                self.status = Some(format!(
                    "Runtime evaluator finished with {exit_code:?}; host-captured visual evaluation passed."
                ));
            }
            CompletionStatus::Failed => {
                self.status = Some(format!(
                    "Runtime evaluator finished with {exit_code:?}; visual evaluation failed and repair is required."
                ));
            }
            CompletionStatus::Pending | CompletionStatus::NotApplicable => {
                self.record_runtime_evaluation_failure(
                    run_id,
                    format!(
                        "Runtime evaluator finished with {exit_code:?} without resolving visual_evaluation from the host-captured frame; runtime repair is required."
                    ),
                );
            }
        }
        if self.managed_playtest_started_at.is_some() && self.pending_runtime_action.is_none() {
            self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
        }
    }

    fn finish_provider_execution(&mut self, run_id: &str, exit_code: Option<i32>) {
        let changes = match self.code_workspace.as_ref() {
            Some(workspace) => match workspace.collect_changes() {
                Ok(changes) => changes,
                Err(error) => {
                    self.fail_run(run_id, format!("Could not inspect code workspace: {error}"));
                    return;
                }
            },
            None => Vec::new(),
        };
        if let Err(error) = self.host.record_code_checkpoint(run_id, &changes) {
            self.status = Some(error.to_string());
        }
        let has_code_changes = !changes.is_empty();
        self.pending_code_changes = changes;
        if !self.managed_candidate_input_recipe.is_empty() {
            self.managed_input_recipe = std::mem::take(&mut self.managed_candidate_input_recipe);
        }
        self.managed_input_plan = self.managed_input_recipe.iter().cloned().collect();
        self.managed_repair_requested = false;
        if let Err(error) = self
            .host
            .begin_managed_validation(run_id, has_code_changes)
        {
            self.status = Some(error.to_string());
        } else {
            self.status = Some(format!(
                "Provider execution finished with {exit_code:?}; engine-managed validation is active or has recorded its result."
            ));
        }
    }

    fn apply_code_changes(&mut self, run_id: &str) {
        let Some(workspace) = self.code_workspace.as_mut() else {
            self.status = Some("No managed code workspace is available.".to_owned());
            return;
        };
        match workspace.apply_changes(&self.pending_code_changes) {
            Ok(()) => {
                let count = self.pending_code_changes.len();
                self.pending_code_changes.clear();
                if let Err(error) = self.host.record_event(
                    run_id,
                    AgentEventKind::CodeChangesApplied,
                    format!("Applied {count} reviewed code file change(s) after stale checks."),
                ) {
                    self.status = Some(error.to_string());
                } else {
                    self.status = Some(format!("Applied {count} reviewed code file change(s)."));
                }
            }
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn record_provider_semantic_event(&mut self, run_id: &str, event: ProviderAgentEvent) -> Result<(), String> {
        match event {
            ProviderAgentEvent::Progress { step, detail } => self.host.record_semantic_progress(run_id, step, detail).map_err(|error| error.to_string()),
            ProviderAgentEvent::ToolAction { tool, action, success } => self.host.record_tool_action(run_id, tool, action, success).map_err(|error| error.to_string()),
            ProviderAgentEvent::CompletionGate { gate, status, message } => self.host.record_completion_gate(run_id, &gate, status, message).map_err(|error| error.to_string()),
            ProviderAgentEvent::PlaytestResult { launched, interactions_passed, message } => {
                self.host.record_playtest_result(run_id, launched, interactions_passed, message).map_err(|error| error.to_string())?;
                if launched
                    && interactions_passed == Some(true)
                    && self.managed_playtest_started_at.is_some()
                    && self.host.run(run_id).is_ok_and(|run| run.audit.managed_runtime_inputs > 0)
                    && !self.managed_capture_requested
                {
                    self.managed_capture_requested = true;
                    self.request_permission(run_id.to_owned(), AgentCapability::FrameCapture, PendingPermissionAction::CaptureFrame);
                }
                Ok(())
            }
            ProviderAgentEvent::RuntimeInput { input } => {
                if !self
                    .host
                    .run(run_id)
                    .is_ok_and(|run| run.state == AgentRunState::Executing)
                {
                    return Err("runtime_input planning is accepted only during provider execution, before managed Play evaluation".to_owned());
                }
                let command = input.command()?;
                self.managed_candidate_input_recipe.push(command);
                self.host
                    .record_semantic_progress(
                        run_id,
                        "runtime_input_plan",
                        format!("Queued provider-planned runtime input {command:?} for managed Editor Play."),
                    )
                    .map_err(|error| error.to_string())
            }
        }
    }

    fn fail_run(&mut self, run_id: &str, message: String) {
        let _ = self
            .host
            .record_event(run_id, AgentEventKind::Failure, message.clone());
        let _ = self
            .host
            .transition_run(run_id, AgentRunState::Failed, message.clone());
        self.status = Some(message);
    }
}

fn live_observation_error_response(error: LiveObservationError) -> RemoteAiStudioResponse {
    match error {
        LiveObservationError::InvalidFps => RemoteAiStudioResponse::error(
            400,
            "live_observation_invalid_fps",
            error.to_string(),
            false,
        ),
        LiveObservationError::TooManySessions => RemoteAiStudioResponse::error(
            409,
            "live_observation_capacity",
            error.to_string(),
            true,
        ),
        LiveObservationError::NotFound => RemoteAiStudioResponse::error(
            404,
            "live_observation_not_found",
            error.to_string(),
            false,
        ),
        LiveObservationError::Unauthorized => RemoteAiStudioResponse::error(
            401,
            "live_observation_unauthorized",
            error.to_string(),
            false,
        ),
        LiveObservationError::Random(_) => RemoteAiStudioResponse::error(
            500,
            "live_observation_session_failed",
            error.to_string(),
            true,
        ),
        LiveObservationError::InvalidFrame(_) | LiveObservationError::Encode(_) => {
            RemoteAiStudioResponse::error(
                503,
                "live_observation_frame_failed",
                error.to_string(),
                true,
            )
        }
    }
}

fn source_repair_decision(
    state: AgentRunState,
    source_validation: CompletionStatus,
    failed_attempts: usize,
) -> SourceRepairDecision {
    if state != AgentRunState::Repairing
        || source_validation != CompletionStatus::Failed
        || failed_attempts == 0
    {
        return SourceRepairDecision::Wait;
    }
    if failed_attempts > MAX_AUTONOMOUS_SOURCE_REPAIRS {
        SourceRepairDecision::Exhausted
    } else {
        SourceRepairDecision::Retry(failed_attempts)
    }
}

fn runtime_repair_decision(
    state: AgentRunState,
    source_validation: CompletionStatus,
    play_launch: CompletionStatus,
    frame_capture: CompletionStatus,
    visual_evaluation: CompletionStatus,
    interaction_scenarios: CompletionStatus,
    repair_attempts: usize,
) -> RuntimeRepairDecision {
    let runtime_failed = play_launch == CompletionStatus::Failed
        || frame_capture == CompletionStatus::Failed
        || visual_evaluation == CompletionStatus::Failed
        || interaction_scenarios == CompletionStatus::Failed;
    if state != AgentRunState::Repairing
        || source_validation == CompletionStatus::Failed
        || !runtime_failed
    {
        return RuntimeRepairDecision::Wait;
    }
    if repair_attempts >= MAX_AUTONOMOUS_RUNTIME_REPAIRS {
        RuntimeRepairDecision::Exhausted
    } else {
        RuntimeRepairDecision::Retry(repair_attempts + 1)
    }
}

fn managed_repair_context(
    run: &crate::agent_host::AgentRun,
    runtime_observation: Option<&ManagedRuntimeObservation>,
) -> String {
    if run.completion.source_validation == CompletionStatus::Failed {
        return managed_source_repair_context(run);
    }
    let observation = runtime_observation
        .map(|observation| {
            format!(
                " Last host-captured frame: {} ({}x{}) at {}.",
                observation.artifact_id,
                observation.width,
                observation.height,
                observation.path.display()
            )
        })
        .unwrap_or_default();
    format!(
        "Managed runtime evidence failed (play_launch={:?}, frame_capture={:?}, visual_evaluation={:?}, interaction_scenarios={:?}). Repair the existing isolated workspace in place without expanding the immutable proposal scope. Preserve or replace the provider-planned runtime interaction recipe as appropriate; GameEngine will rerun managed source validation, start a fresh Editor Play session, replay the resulting interaction recipe, capture a fresh frame, and evaluate again.{observation}",
        run.completion.play_launch,
        run.completion.frame_capture,
        run.completion.visual_evaluation,
        run.completion.interaction_scenarios,
    )
}

fn managed_source_repair_context(run: &crate::agent_host::AgentRun) -> String {
    let Some(attempt) = run
        .validation_attempts
        .iter()
        .rev()
        .find(|attempt| attempt.status == ManagedValidationAttemptStatus::Failed)
    else {
        return "Managed source validation failed without a recorded failed attempt. Re-inspect the existing workspace and repair only the source-validation failure without expanding proposal scope.".to_owned();
    };
    let failures = attempt
        .gate_results
        .iter()
        .filter_map(|result| {
            result.failure.as_ref().map(|failure| {
                format!("{:?}: {}", result.gate, failure.message)
            })
        })
        .collect::<Vec<_>>();
    let detail = if failures.is_empty() {
        "no gate-specific failure message was recorded".to_owned()
    } else {
        failures.join(" | ")
    };
    format!(
        "Managed validation attempt {} failed: {detail}. Repair the existing isolated workspace in place, preserve successful work, do not expand the immutable proposal scope, and return normally so GameEngine can run managed validation again.",
        attempt.id
    )
}

fn provider_key_code(key: &str) -> Result<KeyCode, String> {
    let normalized = key.trim().to_ascii_lowercase();
    let key = match normalized.as_str() {
        "a" | "keya" => KeyCode::KeyA,
        "b" | "keyb" => KeyCode::KeyB,
        "c" | "keyc" => KeyCode::KeyC,
        "d" | "keyd" => KeyCode::KeyD,
        "e" | "keye" => KeyCode::KeyE,
        "f" | "keyf" => KeyCode::KeyF,
        "g" | "keyg" => KeyCode::KeyG,
        "h" | "keyh" => KeyCode::KeyH,
        "i" | "keyi" => KeyCode::KeyI,
        "j" | "keyj" => KeyCode::KeyJ,
        "k" | "keyk" => KeyCode::KeyK,
        "l" | "keyl" => KeyCode::KeyL,
        "m" | "keym" => KeyCode::KeyM,
        "n" | "keyn" => KeyCode::KeyN,
        "o" | "keyo" => KeyCode::KeyO,
        "p" | "keyp" => KeyCode::KeyP,
        "q" | "keyq" => KeyCode::KeyQ,
        "r" | "keyr" => KeyCode::KeyR,
        "s" | "keys" => KeyCode::KeyS,
        "t" | "keyt" => KeyCode::KeyT,
        "u" | "keyu" => KeyCode::KeyU,
        "v" | "keyv" => KeyCode::KeyV,
        "w" | "keyw" => KeyCode::KeyW,
        "x" | "keyx" => KeyCode::KeyX,
        "y" | "keyy" => KeyCode::KeyY,
        "z" | "keyz" => KeyCode::KeyZ,
        "0" | "digit0" => KeyCode::Digit0,
        "1" | "digit1" => KeyCode::Digit1,
        "2" | "digit2" => KeyCode::Digit2,
        "3" | "digit3" => KeyCode::Digit3,
        "4" | "digit4" => KeyCode::Digit4,
        "5" | "digit5" => KeyCode::Digit5,
        "6" | "digit6" => KeyCode::Digit6,
        "7" | "digit7" => KeyCode::Digit7,
        "8" | "digit8" => KeyCode::Digit8,
        "9" | "digit9" => KeyCode::Digit9,
        "arrowup" | "up" => KeyCode::ArrowUp,
        "arrowdown" | "down" => KeyCode::ArrowDown,
        "arrowleft" | "left" => KeyCode::ArrowLeft,
        "arrowright" | "right" => KeyCode::ArrowRight,
        "space" => KeyCode::Space,
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "shiftleft" | "shift" => KeyCode::ShiftLeft,
        "controlleft" | "control" | "ctrl" => KeyCode::ControlLeft,
        _ => return Err(format!("unsupported managed runtime key `{key}`")),
    };
    Ok(key)
}

fn provider_mouse_button(button: &str) -> Result<MouseButton, String> {
    match button.trim().to_ascii_lowercase().as_str() {
        "left" | "primary" => Ok(MouseButton::Left),
        "right" | "secondary" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        _ => Err(format!("unsupported managed runtime mouse button `{button}`")),
    }
}

fn load_ai_studio_preferences(path: &std::path::Path) -> AiStudioPreferences {
    let Ok(bytes) = fs::read(path) else {
        return AiStudioPreferences::default();
    };
    let Ok(preferences) = serde_json::from_slice::<AiStudioPreferences>(&bytes) else {
        return AiStudioPreferences::default();
    };
    if preferences.schema_version == AI_STUDIO_PREFERENCES_SCHEMA_VERSION {
        preferences
    } else {
        AiStudioPreferences::default()
    }
}

fn capability_label(value: CapabilityAvailability) -> &'static str {
    match value {
        CapabilityAvailability::Available => "available",
        CapabilityAvailability::Unavailable => "unavailable",
    }
}

fn model_capability_summary(profile: &ModelCapabilityProfile) -> String {
    format!(
        "Backend: {} · Model: {} · structured: {} · tools: {} · images: {} · reasoning: {} · context: {} · benchmark: {}",
        profile.backend_id,
        if profile.model_id.is_empty() {
            "not selected"
        } else {
            profile.model_id.as_str()
        },
        capability_flag(profile.structured_output),
        capability_flag(profile.tool_use),
        capability_flag(profile.image_input),
        capability_flag(profile.reasoning),
        profile
            .context_limit
            .map(|limit| limit.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
        if profile.benchmark_verified {
            "verified"
        } else {
            "unverified"
        }
    )
}

fn capability_flag(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "unknown",
    }
}

fn format_native_answer(answer: &NativeAnswer) -> String {
    let mut message = answer.text.trim().to_owned();
    message.push_str("\n\nSources / provenance:\n");
    if answer.sources.is_empty() {
        message.push_str(
            "- No matching GameEngine repository or project source was retrieved; general model knowledge may have been used.\n",
        );
    } else {
        for source in &answer.sources {
            message.push_str(&format!(
                "- {}:{}\n",
                source.kind.label(),
                source.path
            ));
        }
        message.push_str(
            "- General model knowledge may supplement the retrieved evidence where the answer says so.\n",
        );
    }
    message.push_str("Harness evidence:\n");
    message.push_str(&format!(
        "- {} · backend={} · model={} · turns={} · retrieved_chunks={} · prompt_chars={} · response_chars={} · elapsed_ms={} · prompt_tokens={} · response_tokens={} · backend_ms={} · load_ms={} · prompt_eval_ms={} · generation_ms={} · generation_mtokens_per_s={} · ttft_ms={}",
        answer.metrics.harness_version,
        answer.metrics.backend_id,
        answer.metrics.model_id,
        answer.metrics.model_turns,
        answer.metrics.retrieval_chunks,
        answer.metrics.prompt_chars,
        answer.metrics.response_chars,
        answer.metrics.elapsed_ms,
        optional_metric(answer.metrics.prompt_eval_tokens),
        optional_metric(answer.metrics.response_tokens),
        optional_metric(answer.metrics.backend_duration_ms),
        optional_metric(answer.metrics.load_latency_ms),
        optional_metric(answer.metrics.prompt_eval_duration_ms),
        optional_metric(answer.metrics.generation_duration_ms),
        optional_metric(answer.metrics.generation_tokens_per_second_milli),
        optional_metric(answer.metrics.ttft_ms),
    ));
    message
}

fn optional_metric(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "n/a".to_owned())
}

fn edit_lines(ui: &mut egui::Ui, label: &str, values: &mut Vec<String>) {
    let mut text = values.join("\n");
    ui.label(label);
    if ui
        .add(
            egui::TextEdit::multiline(&mut text)
                .desired_rows(2)
                .hint_text("One item per line"),
        )
        .changed()
    {
        *values = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToOwned::to_owned)
            .collect();
    }
}

fn encode_agent_frame_png(capture: &crate::FrameCapture) -> Result<Vec<u8>, png::EncodingError> {
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, capture.width, capture.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&capture.rgba8)?;
    }
    Ok(png_bytes)
}

fn split_direct_args(text: &str) -> Vec<String> {
    text.split_whitespace().map(ToOwned::to_owned).collect()
}

fn change_summary(change: &CodeChange) -> &'static str {
    match (&change.before, &change.after) {
        (None, Some(_)) => "new",
        (Some(_), None) => "delete (apply blocked)",
        (Some(_), Some(_)) => "modified",
        (None, None) => "unchanged",
    }
}

fn completion_row(ui: &mut egui::Ui, label: &str, status: CompletionStatus) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.strong(completion_label(status));
    });
}

fn completion_label(status: CompletionStatus) -> &'static str {
    match status {
        CompletionStatus::Pending => "Pending",
        CompletionStatus::Passed => "Passed",
        CompletionStatus::Failed => "Failed",
        CompletionStatus::NotApplicable => "N/A",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_args_do_not_invoke_shell_parsing() {
        assert_eq!(
            split_direct_args("--flag value ; echo nope"),
            ["--flag", "value", ";", "echo", "nope"]
        );
    }

    #[test]
    fn provider_semantic_progress_is_structured_json() {
        let event: ProviderAgentEvent = serde_json::from_str(
            r#"{"type":"progress","step":"inspect","detail":"scene"}"#,
        ).expect("semantic event");
        assert!(matches!(
            event,
            ProviderAgentEvent::Progress { step, detail } if step == "inspect" && detail == "scene"
        ));
    }

    #[test]
    fn provider_runtime_input_maps_to_engine_input_command() {
        let event: ProviderAgentEvent = serde_json::from_str(
            r#"{"type":"runtime_input","input":{"kind":"key","key":"W","pressed":true}}"#,
        )
        .expect("runtime input event");
        let ProviderAgentEvent::RuntimeInput { input } = event else {
            panic!("runtime input event");
        };
        assert_eq!(
            input.command().expect("command"),
            InputCommand::Key {
                key: KeyCode::KeyW,
                pressed: true,
            }
        );
    }

    #[test]
    fn provider_runtime_input_rejects_non_finite_numbers() {
        let input = ProviderRuntimeInput::MouseMove {
            x: f32::NAN,
            y: 1.0,
        };
        assert!(input.command().is_err());
    }

    #[test]
    fn code_change_summary_keeps_deletion_blocked() {
        let change = CodeChange {
            relative_path: PathBuf::from("game/src/lib.rs"),
            before: Some("old".to_owned()),
            after: None,
        };
        assert_eq!(change_summary(&change), "delete (apply blocked)");
    }

    #[test]
    fn autonomous_source_repair_is_bounded_and_source_failure_only() {
        assert_eq!(
            source_repair_decision(
                AgentRunState::Repairing,
                CompletionStatus::Failed,
                1,
            ),
            SourceRepairDecision::Retry(1)
        );
        assert_eq!(
            source_repair_decision(
                AgentRunState::Repairing,
                CompletionStatus::Failed,
                2,
            ),
            SourceRepairDecision::Retry(2)
        );
        assert_eq!(
            source_repair_decision(
                AgentRunState::Repairing,
                CompletionStatus::Failed,
                3,
            ),
            SourceRepairDecision::Exhausted
        );
        assert_eq!(
            source_repair_decision(
                AgentRunState::Repairing,
                CompletionStatus::Pending,
                1,
            ),
            SourceRepairDecision::Wait
        );
        assert_eq!(
            source_repair_decision(
                AgentRunState::Evaluating,
                CompletionStatus::Failed,
                1,
            ),
            SourceRepairDecision::Wait
        );
    }

    #[test]
    fn autonomous_runtime_repair_includes_frame_capture_failure() {
        assert_eq!(
            runtime_repair_decision(
                AgentRunState::Repairing,
                CompletionStatus::Passed,
                CompletionStatus::Passed,
                CompletionStatus::Failed,
                CompletionStatus::Pending,
                CompletionStatus::Pending,
                0,
            ),
            RuntimeRepairDecision::Retry(1)
        );
        assert_eq!(
            runtime_repair_decision(
                AgentRunState::Repairing,
                CompletionStatus::Passed,
                CompletionStatus::Passed,
                CompletionStatus::Failed,
                CompletionStatus::Pending,
                CompletionStatus::Pending,
                MAX_AUTONOMOUS_RUNTIME_REPAIRS,
            ),
            RuntimeRepairDecision::Exhausted
        );
    }
}
