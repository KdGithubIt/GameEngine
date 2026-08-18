//! Conversation-first AI Studio frontend.
//!
//! This module owns only presentation and direct user interaction. Agent
//! lifecycle, permissions, persistence, provider process management, and code
//! workspace rules live in the GUI-free `agent_host` module.

use crate::agent_benchmark::{
    agent_run_record, benchmark_task, read_question_record, AgentRunBenchmarkIdentity,
    BenchmarkHardwareIdentity, BenchmarkRecord, BenchmarkStore, BenchmarkTaskKind, CatalogProfile,
    CuratedModelCatalog, BENCHMARK_CORPUS_VERSION, BENCHMARK_TASKS,
};
use crate::agent_host::{
    project_storage_key, AgentCapability, AgentConfinementNetworkPolicy, AgentConfinementRequest,
    AgentConfinementRequirement, AgentEventKind, AgentHost, AgentHostError, AgentProposal,
    AgentRunState,
    AgentWorkClaim,
    ApprovalScope, AuthoritativeStateSnapshot, CodeChange, CodeWorkspace, CompletionStatus,
    ConversationRole, ExternalAgentProcess, ManagedValidationAttemptStatus, PermissionCheck,
    ProcessStream, ResumeDisposition,
};
use crate::external_agent_provider::{
    build_launch_plan, probe_provider, translate_provider_line, ExternalAgentDiagnostics,
    ExternalAgentProviderKind, ExternalAgentProviderStatus, ExternalAgentSemanticEvent,
};
use crate::hosted_model_backend;
use crate::hosted_model_backend::{HostedAuthMode, HostedModelConfig};
use crate::live_observation::{LiveObservationError, LiveObservationManager};
use crate::managed_local_runtime::{
    ManagedExecutionEnvironment, ManagedLocalModelConfig, ManagedLocalRuntime,
    ManagedSetupOperation, ManagedSetupResult, ManagedSetupStatus, ManagedSetupTask,
    PINNED_LLAMA_CPP_REVISION, PINNED_LLAMA_CPP_TAG,
};
use crate::model_router::{ModelRoutingPolicy, MODEL_ROUTER_POLICY_VERSION};
use crate::native_agent::{
    InstalledLocalModel, InstalledModelDiscoveryTask, InstalledModelInventory,
    LocalModelConfig, LocalModelResourceConfig, ModelCapabilityProfile, ModelResourceTask,
    NativeAnswer, NativeMetrics, NativeModelConfig, NativeQuestionTask, QuestionMessage,
    QuestionRole, DEFAULT_LOCAL_MODEL_ENDPOINT,
};
use crate::native_agent_runtime::{mcp_write, NativeAgentAction, NativeAgentRuntime, NativeMcpTask};
use crate::resource_arbitration::{
    classify_workload, resolve_resource_plan, resource_operation_for_residency_request,
    CapabilityAvailability, InferenceWorkload, MemoryPressure, ModelResidencyRequest,
    ModelResourceOperation, ModelResourceTelemetry, PresentationPosture, QualityPreference,
    ReclaimLevel, ResourcePlan, TelemetryValue, WorkloadSignals,
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
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

const PROVIDER_EVENT_PREFIX: &str = "GAMEENGINE_AGENT_EVENT ";
const MAX_AUTONOMOUS_SOURCE_REPAIRS: usize = 2;
const MAX_AUTONOMOUS_RUNTIME_REPAIRS: usize = 2;
const AI_STUDIO_PREFERENCES_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum ModelBackendPreference {
    #[default]
    Local,
    ManagedLocal,
    HostedApi,
    Enterprise,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiStudioPreferences {
    schema_version: u32,
    #[serde(default)]
    quality_preference: QualityPreference,
    #[serde(default)]
    confinement_requirement: AgentConfinementRequirement,
    #[serde(default)]
    external_agent_provider: ExternalAgentProviderKind,
    #[serde(default)]
    model_backend: ModelBackendPreference,
    #[serde(default)]
    managed_execution_environment: ManagedExecutionEnvironment,
    #[serde(default)]
    managed_model_id: String,
    #[serde(default = "default_local_model_endpoint")]
    local_model_endpoint: String,
    #[serde(default)]
    local_model_name: String,
    #[serde(default)]
    hosted_model_endpoint: String,
    #[serde(default)]
    hosted_model_name: String,
}

impl Default for AiStudioPreferences {
    fn default() -> Self {
        Self {
            schema_version: AI_STUDIO_PREFERENCES_SCHEMA_VERSION,
            quality_preference: QualityPreference::Auto,
            confinement_requirement: AgentConfinementRequirement::default(),
            external_agent_provider: ExternalAgentProviderKind::default(),
            model_backend: ModelBackendPreference::ManagedLocal,
            managed_execution_environment: ManagedExecutionEnvironment::WindowsNative,
            managed_model_id: String::new(),
            local_model_endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            local_model_name: String::new(),
            hosted_model_endpoint: String::new(),
            hosted_model_name: String::new(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentRuntimeMode {
    External,
    Native,
}

enum ModelResourceContinuation {
    RestoreForEditing,
    LaunchManagedPlay { run_id: String },
    ResumeAfterEditing {
        run_id: Option<String>,
        state: AiStudioAuthoritativeState,
    },
}

#[derive(Debug, Clone)]
struct ManagedRuntimeObservation {
    artifact_id: String,
    path: PathBuf,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone)]
struct NativeQuestionBenchmarkPolicy {
    task_id: String,
    quality: QualityPreference,
    workload: InferenceWorkload,
    hardware: BenchmarkHardwareIdentity,
    inventory: Option<InstalledModelInventory>,
}

#[derive(Debug, Clone)]
struct NativeQuestionBenchmarkSnapshot {
    metrics: NativeMetrics,
    policy: NativeQuestionBenchmarkPolicy,
}

#[derive(Debug, Clone)]
struct NativeRunBenchmarkContext {
    run_id: String,
    task_id: String,
    backend_id: String,
    model_id: String,
    quality: QualityPreference,
    workload: InferenceWorkload,
    hardware: BenchmarkHardwareIdentity,
    inventory: Option<InstalledModelInventory>,
    routed: bool,
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
    StartNativeAgent,
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

struct PendingQuestionPermission {
    session_id: String,
    config: NativeModelConfig,
    conversation: Vec<QuestionMessage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiStudioPresentationMode {
    Embedded,
    Detached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AiStudioPresentationState {
    mode: AiStudioPresentationMode,
    open: bool,
}

impl Default for AiStudioPresentationState {
    fn default() -> Self {
        Self {
            mode: AiStudioPresentationMode::Embedded,
            open: true,
        }
    }
}

impl AiStudioPresentationState {
    fn open(&mut self) {
        self.open = true;
    }

    fn close(&mut self) {
        self.open = false;
    }

    fn detach(&mut self) {
        self.mode = AiStudioPresentationMode::Detached;
        self.open = true;
    }

    fn reattach(&mut self) {
        self.mode = AiStudioPresentationMode::Embedded;
        self.open = true;
    }
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
    confinement_requirement: AgentConfinementRequirement,
    external_provider_kind: ExternalAgentProviderKind,
    external_provider_status: ExternalAgentProviderStatus,
    model_backend: ModelBackendPreference,
    managed_local_runtime: ManagedLocalRuntime,
    managed_execution_environment: ManagedExecutionEnvironment,
    managed_model_id: String,
    managed_setup_task: Option<ManagedSetupTask>,
    local_model_endpoint: String,
    local_model_name: String,
    benchmark_store: BenchmarkStore,
    benchmark_records: Vec<BenchmarkRecord>,
    model_catalog: CuratedModelCatalog,
    benchmark_task_id: String,
    benchmark_hardware: BenchmarkHardwareIdentity,
    benchmark_hardware_probe_attempted: bool,
    installed_model_inventory: Option<InstalledModelInventory>,
    model_discovery: Option<InstalledModelDiscoveryTask>,
    native_question_benchmark_policy: Option<NativeQuestionBenchmarkPolicy>,
    last_native_question_benchmark: Option<NativeQuestionBenchmarkSnapshot>,
    native_run_benchmark_context: Option<NativeRunBenchmarkContext>,
    hosted_model_endpoint: String,
    hosted_model_name: String,
    hosted_secret_path: PathBuf,
    hosted_secret_draft: String,
    resolved_workload: InferenceWorkload,
    resource_plan: ResourcePlan,
    model_resource_task: Option<ModelResourceTask>,
    model_resource_continuation: Option<ModelResourceContinuation>,
    last_model_resource_telemetry: ModelResourceTelemetry,
    editing_interrupted: bool,
    restore_for_editing: bool,
    interrupt_snapshot: Option<AiStudioAuthoritativeState>,
    native_question: Option<NativeQuestionTask>,
    pending_native_question_start: Option<(NativeModelConfig, Vec<QuestionMessage>, String)>,
    pending_question_permission: Option<PendingQuestionPermission>,
    native_question_session: Option<String>,
    native_agent_runtime: Option<NativeAgentRuntime>,
    native_mcp_task: Option<NativeMcpTask>,
    pending_native_mcp_tool: Option<String>,
    active_runtime_mode: Option<AgentRuntimeMode>,
    active_external_provider: Option<ExternalAgentProviderKind>,
    active_external_program: Option<String>,
    active_external_args: Option<String>,
    provider_program: String,
    provider_args: String,
    presentation: AiStudioPresentationState,
    #[cfg(feature = "visual-validation")]
    detached_visual_frames: u8,
    active_run_id: Option<String>,
    process: Option<ExternalAgentProcess>,
    process_purpose: Option<ExternalAgentPurpose>,
    external_provider_diagnostics: ExternalAgentDiagnostics,
    pending_external_work_owner: Option<(ExternalAgentPurpose, String)>,
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
        let ai_root = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("GameEngine")
            .join("ai");
        let data_root =
            ai_root.join(project_storage_key(project.project_id().as_str(), project.path()));
        let preferences_path = data_root.join("preferences.json");
        let hosted_secret_path = data_root.join("secrets").join("hosted-api-key.dpapi");
        let preferences = load_ai_studio_preferences(&preferences_path);
        let managed_local_runtime = ManagedLocalRuntime::open(ai_root.join("managed-local"))
            .map_err(|error| error.to_string())?;
        let benchmark_store = BenchmarkStore::open(ai_root.join("benchmark"))?;
        let (benchmark_records, benchmark_status) = match benchmark_store.load() {
            Ok(records) => (records, None),
            Err(error) => (
                Vec::new(),
                Some(format!("Benchmark records unavailable: {error}")),
            ),
        };
        let model_catalog = CuratedModelCatalog::from_bundled_manifest(&benchmark_records)?;
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
            confinement_requirement: preferences.confinement_requirement,
            external_provider_kind: preferences.external_agent_provider,
            external_provider_status: ExternalAgentProviderStatus::unchecked(
                preferences.external_agent_provider,
            ),
            model_backend: preferences.model_backend,
            managed_local_runtime,
            managed_execution_environment: preferences.managed_execution_environment,
            managed_model_id: preferences.managed_model_id,
            managed_setup_task: None,
            local_model_endpoint: preferences.local_model_endpoint,
            local_model_name: preferences.local_model_name,
            benchmark_store,
            benchmark_records,
            model_catalog,
            benchmark_task_id: BENCHMARK_TASKS[0].id.to_owned(),
            benchmark_hardware: BenchmarkHardwareIdentity::default(),
            benchmark_hardware_probe_attempted: false,
            installed_model_inventory: None,
            model_discovery: None,
            native_question_benchmark_policy: None,
            last_native_question_benchmark: None,
            native_run_benchmark_context: None,
            hosted_model_endpoint: preferences.hosted_model_endpoint,
            hosted_model_name: preferences.hosted_model_name,
            hosted_secret_path,
            hosted_secret_draft: String::new(),
            resolved_workload: InferenceWorkload::InteractiveReasoning,
            resource_plan: resolve_resource_plan(
                InferenceWorkload::InteractiveReasoning,
                preferences.quality_preference,
                MemoryPressure::Unknown,
                Default::default(),
            ),
            model_resource_task: None,
            model_resource_continuation: None,
            last_model_resource_telemetry: ModelResourceTelemetry::default(),
            editing_interrupted: false,
            restore_for_editing: false,
            interrupt_snapshot: None,
            native_question: None,
            pending_native_question_start: None,
            pending_question_permission: None,
            native_question_session: None,
            native_agent_runtime: None,
            native_mcp_task: None,
            pending_native_mcp_tool: None,
            active_runtime_mode: None,
            active_external_provider: None,
            active_external_program: None,
            active_external_args: None,
            provider_program: String::new(),
            provider_args: String::new(),
            presentation: AiStudioPresentationState::default(),
            #[cfg(feature = "visual-validation")]
            detached_visual_frames: 0,
            active_run_id,
            process: None,
            process_purpose: None,
            external_provider_diagnostics: ExternalAgentDiagnostics::default(),
            pending_external_work_owner: None,
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
            status: benchmark_status,
        })
    }

    /// Makes the AI Studio presentation visible without changing its current placement.
    pub fn open(&mut self) {
        self.presentation.open();
    }

    /// Moves AI Studio into an independent native viewport while preserving the same host state.
    pub fn detach(&mut self) {
        self.presentation.detach();
    }

    /// Captures the active Editor adapter and reliable machine memory identity once.
    ///
    /// Unsupported or ambiguous platform telemetry remains explicitly unavailable.
    pub fn observe_benchmark_hardware(&mut self, frame: &eframe::Frame) {
        if self.benchmark_hardware_probe_attempted {
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let adapter = render_state.adapter.get_info();
        self.benchmark_hardware = BenchmarkHardwareIdentity::from_editor_adapter(
            &adapter.name,
            adapter.vendor,
            adapter.device,
        );
        self.benchmark_hardware_probe_attempted = true;
    }

    #[cfg(feature = "visual-validation")]
    /// Selects a deterministic hosted-backend state for screenshot validation.
    pub fn prepare_hosted_backend_visual_validation(&mut self) {
        self.model_backend = ModelBackendPreference::HostedApi;
        self.hosted_model_endpoint = "https://provider.example/v1/chat/completions".to_owned();
        self.hosted_model_name = "example-hosted-model".to_owned();
        self.hosted_secret_draft.clear();
        self.external_provider_kind = ExternalAgentProviderKind::ClaudeCode;
        self.external_provider_status =
            ExternalAgentProviderStatus::visual_fixture(ExternalAgentProviderKind::ClaudeCode);
    }

    #[cfg(feature = "visual-validation")]
    /// Returns whether the detached native viewport has completed two rendered frames.
    pub fn detached_visual_validation_capture_ready(&self) -> bool {
        self.detached_visual_frames >= 2
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
                self.begin_model_reload_after_authoritative_inspection(state.clone());
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

    fn retry_external_work_wait_if_ready(&mut self) {
        let Some((purpose, owner_run_id)) = self.pending_external_work_owner.clone() else {
            return;
        };
        let owner_finished = match self.host.run(&owner_run_id) {
            Ok(run) => matches!(
                run.state,
                AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
            ),
            Err(_) => true,
        };
        if !owner_finished || self.process.is_some() || self.pending_permission.is_some() {
            return;
        }
        let Some(run_id) = self.active_run_id.clone() else {
            self.pending_external_work_owner = None;
            return;
        };
        self.pending_external_work_owner = None;
        self.launch_external_agent(&run_id, purpose);
    }

    /// Draws the current AI Studio presentation and advances host-owned work.
    pub fn show(&mut self, context: &egui::Context) {
        self.ensure_remote_gateway(context);
        self.poll_remote_requests();
        self.poll_model_discovery(context);
        self.poll_managed_setup(context);
        self.poll_native_question(context);
        self.poll_model_resource_task(context);
        self.poll_native_mcp(context);
        self.poll_native_agent_runtime(context);
        self.retry_external_work_wait_if_ready();
        self.poll_external_process(context);
        self.poll_managed_validation(context);
        self.request_managed_source_repair_if_ready();
        self.request_managed_runtime_repair_if_ready();
        self.request_managed_playtest_if_ready();
        self.request_next_managed_runtime_input_if_ready();
        self.poll_managed_playtest_timeout();

        if !self.presentation.open {
            return;
        }
        match self.presentation.mode {
            AiStudioPresentationMode::Embedded => self.show_embedded(context),
            AiStudioPresentationMode::Detached => self.show_detached(context),
        }
    }

    fn show_embedded(&mut self, context: &egui::Context) {
        let mut open = self.presentation.open;
        let mut detach_requested = false;
        egui::Window::new("AI Studio")
            .id(egui::Id::new("gameengine_ai_studio"))
            .open(&mut open)
            .default_pos(egui::pos2(940.0, 84.0))
            .default_size(egui::vec2(600.0, 760.0))
            .min_width(460.0)
            .min_height(520.0)
            .resizable(true)
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Detach").clicked() {
                        detach_requested = true;
                    }
                    ui.small("Open AI Studio in its own OS window.");
                });
                ui.separator();
                self.show_contents(ui);
            });
        self.presentation.open = open;
        if detach_requested {
            self.presentation.detach();
        }
    }

    fn show_detached(&mut self, context: &egui::Context) {
        let mut reattach_requested = false;
        let mut close_requested = false;
        context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("gameengine_ai_studio_detached"),
            egui::ViewportBuilder::default()
                .with_title("AI Studio")
                .with_inner_size([600.0, 680.0])
                .with_min_inner_size([460.0, 520.0])
                .with_resizable(true),
            |ui, _class| {
                close_requested = ui.input(|input| input.viewport().close_requested());
                #[cfg(feature = "visual-validation")]
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                ui.horizontal(|ui| {
                    if ui.button("Reattach").clicked() {
                        reattach_requested = true;
                    }
                    ui.small("Same project Agent Host · detached presentation");
                });
                ui.separator();
                let scroll_area = egui::ScrollArea::vertical()
                    .id_salt("ai_studio_detached_contents")
                    .auto_shrink([false, false]);
                #[cfg(feature = "visual-validation")]
                let scroll_area = scroll_area.vertical_scroll_offset(480.0);
                scroll_area.show(ui, |ui| self.show_contents(ui));
            },
        );

        #[cfg(feature = "visual-validation")]
        {
            self.detached_visual_frames = self.detached_visual_frames.saturating_add(1);
        }

        if reattach_requested {
            self.presentation.reattach();
        } else if close_requested {
            self.presentation.close();
        }
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
                    Ok(mut snapshot) => {
                        if let Some(object) = snapshot.as_object_mut() {
                            let posture = match self.model_backend {
                                ModelBackendPreference::Local => serde_json::json!({
                                    "kind": "external_local",
                                    "remote_processing": false
                                }),
                                ModelBackendPreference::ManagedLocal => serde_json::json!({
                                    "kind": "managed_local",
                                    "remote_processing": false,
                                    "execution_environment": self.managed_execution_environment.benchmark_id(),
                                    "runtime_family": "llama.cpp"
                                }),
                                ModelBackendPreference::HostedApi => serde_json::json!({
                                    "kind": "hosted_api",
                                    "remote_processing": true,
                                    "credential": "configured_or_missing"
                                }),
                                ModelBackendPreference::Enterprise => serde_json::json!({
                                    "kind": "enterprise",
                                    "remote_processing": true,
                                    "credential": "organization_managed"
                                }),
                            };
                            object.insert("processing_posture".to_owned(), posture);
                            object.insert(
                                "external_agent_provider".to_owned(),
                                self.current_external_provider_status().remote_json(),
                            );
                        }
                        RemoteAiStudioResponse::json(snapshot)
                    }
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
        self.pending_external_work_owner = None;
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
            let can_send = !self.message_draft.trim().is_empty()
                && self.native_question.is_none()
                && self.pending_question_permission.is_none();
            if ui.add_enabled(can_send, egui::Button::new("Send")).clicked() {
                let text = self.message_draft.trim().to_owned();
                match self.host.append_message(
                    &self.selected_session,
                    ConversationRole::User,
                    text,
                ) {
                    Ok(()) => {
                        self.message_draft.clear();
                        let awaiting_native = self.active_runtime_mode == Some(AgentRuntimeMode::Native)
                            && self.active_run_id.as_ref().is_some_and(|run_id| {
                                self.host.run(run_id).is_ok_and(|run| run.state == AgentRunState::AwaitingUser)
                            });
                        if awaiting_native {
                            if let Some(run_id) = self.active_run_id.clone() {
                                match self.host.transition_run(&run_id, AgentRunState::Executing, "User response received; native execution may continue.") {
                                    Ok(()) => {
                                        if let Err(error) = self.start_native_agent_turn(&run_id, Some("User answered the pending product question. Re-read current conversation and continue without expanding the immutable proposal.".to_owned()), Vec::new()) {
                                            self.fail_run(&run_id, error);
                                        }
                                    }
                                    Err(error) => self.status = Some(error.to_string()),
                                }
                            }
                        } else {
                            self.start_native_question();
                        }
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            if self.native_question.is_some() {
                ui.spinner();
                ui.small("Reading current GameEngine/project evidence…");
            } else if self.selected_native_model_config().is_err() {
                ui.small("Configure the selected model backend to receive read-only answers.");
            } else {
                ui.small("Questions use the read-only native harness; Go remains explicit for writes.");
            }
        });
    }

    fn show_local_model_settings(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Model backend · questions and native runs")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Backend");
                    let previous = self.model_backend;
                    egui::ComboBox::from_id_salt("ai_studio_model_backend")
                        .selected_text(match self.model_backend {
                            ModelBackendPreference::Local => "External local (Ollama-compatible)",
                            ModelBackendPreference::ManagedLocal => "Managed Local AI",
                            ModelBackendPreference::HostedApi => "Hosted API",
                            ModelBackendPreference::Enterprise => "Enterprise",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.model_backend,
                                ModelBackendPreference::ManagedLocal,
                                "Managed Local AI",
                            );
                            ui.selectable_value(
                                &mut self.model_backend,
                                ModelBackendPreference::Local,
                                "External local (Ollama-compatible)",
                            );
                            ui.selectable_value(
                                &mut self.model_backend,
                                ModelBackendPreference::HostedApi,
                                "Hosted API",
                            );
                            ui.selectable_value(
                                &mut self.model_backend,
                                ModelBackendPreference::Enterprise,
                                "Enterprise",
                            );
                        });
                    if self.model_backend != previous {
                        self.save_preferences();
                    }
                });
                ui.small(match self.model_backend {
                    ModelBackendPreference::Local => "Processing posture: external loopback local runtime; existing Ollama-compatible settings retain their original meaning.",
                    ModelBackendPreference::ManagedLocal => "Processing posture: GameEngine-managed llama.cpp on this machine; the inference server remains loopback-only and never gains authoring authority.",
                    ModelBackendPreference::HostedApi => "Processing posture: selected task context is sent to the configured remote HTTPS provider only after Network access approval.",
                    ModelBackendPreference::Enterprise => "Processing posture: selected task context is sent to the configured enterprise HTTPS endpoint only after Network access approval.",
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Quality");
                    let previous = self.quality_preference;
                    for quality in QualityPreference::ALL {
                        ui.selectable_value(&mut self.quality_preference, quality, quality.label());
                    }
                    if self.quality_preference != previous {
                        self.save_preferences();
                    }
                });
                ui.small(
                    "Quality is a machine-local latency/reasoning preference. Remote GPU controls are never projected as local residency controls.",
                );
                match self.model_backend {
                    ModelBackendPreference::ManagedLocal => {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Execution environment");
                            let previous = self.managed_execution_environment;
                            egui::ComboBox::from_id_salt("ai_studio_managed_environment")
                                .selected_text(self.managed_execution_environment.label())
                                .show_ui(ui, |ui| {
                                    for environment in ManagedExecutionEnvironment::ALL {
                                        ui.selectable_value(
                                            &mut self.managed_execution_environment,
                                            environment,
                                            environment.label(),
                                        );
                                    }
                                });
                            if self.managed_execution_environment != previous {
                                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                                self.save_preferences();
                            }
                        });
                        ui.small(
                            "Windows native is the ordinary first-release default. WSL2 is an advanced characterization/fallback environment and uses the dedicated GameEngine-LocalAI distribution only.",
                        );
                        let setup_status = self
                            .managed_local_runtime
                            .setup_status(self.managed_execution_environment);
                        let setup_busy = self.managed_setup_task.is_some();
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Runtime");
                            ui.monospace(format!(
                                "llama.cpp {PINNED_LLAMA_CPP_TAG} @ {PINNED_LLAMA_CPP_REVISION}"
                            ));
                            match &setup_status {
                                ManagedSetupStatus::Ready => {
                                    ui.strong("Ready");
                                }
                                ManagedSetupStatus::RuntimeNotInstalled => {
                                    if ui
                                        .add_enabled(
                                            !setup_busy,
                                            egui::Button::new("Set up Local AI"),
                                        )
                                        .clicked()
                                    {
                                        self.start_managed_setup(
                                            ManagedSetupOperation::InstallRuntime(
                                                self.managed_execution_environment,
                                            ),
                                            "Downloading and verifying the pinned GameEngine llama.cpp runtime...",
                                        );
                                    }
                                }
                                ManagedSetupStatus::WslDistributionMissing => {
                                    if ui
                                        .add_enabled(
                                            !setup_busy,
                                            egui::Button::new("Set up WSL2 Local AI"),
                                        )
                                        .clicked()
                                    {
                                        self.start_managed_setup(
                                            ManagedSetupOperation::ProvisionWsl,
                                            "Provisioning the dedicated GameEngine-LocalAI WSL environment. Windows may require explicit elevation or restart; GameEngine never bypasses either boundary.",
                                        );
                                    }
                                }
                                ManagedSetupStatus::RestartRequired => {
                                    ui.strong("Restart required");
                                }
                                ManagedSetupStatus::OperatingSystemPrerequisiteUnavailable(
                                    message,
                                ) => {
                                    ui.strong("Unavailable");
                                    ui.small(message);
                                }
                            }
                            if setup_busy {
                                ui.spinner();
                            }
                        });
                        if matches!(&setup_status, ManagedSetupStatus::RestartRequired) {
                            ui.small(
                                "Windows reported that setup requires a restart. GameEngine persists only a machine-local continuation marker and does not reboot automatically. Reopen the Editor after the restart to continue.",
                            );
                        }
                        if !matches!(
                            &setup_status,
                            ManagedSetupStatus::RuntimeNotInstalled
                                | ManagedSetupStatus::OperatingSystemPrerequisiteUnavailable(_)
                        ) {
                            ui.horizontal_wrapped(|ui| {
                                ui.small(
                                    "Removal deletes GameEngine-owned runtime/cache state and unregisters only the dedicated GameEngine-LocalAI WSL distribution. User-owned GGUF source files are preserved.",
                                );
                                if ui
                                    .add_enabled(
                                        !setup_busy,
                                        egui::Button::new("Remove managed environment"),
                                    )
                                    .clicked()
                                {
                                    self.start_managed_setup(
                                        ManagedSetupOperation::RemoveEnvironment(
                                            self.managed_execution_environment,
                                        ),
                                        "Removing the selected GameEngine-managed Local AI environment...",
                                    );
                                }
                            });
                        }
                        ui.horizontal_wrapped(|ui| {
                            ui.label("Managed GGUF");
                            let models = self
                                .managed_local_runtime
                                .registered_models()
                                .unwrap_or_default();
                            egui::ComboBox::from_id_salt("ai_studio_managed_model")
                                .selected_text(
                                    models
                                        .iter()
                                        .find(|model| model.model_id == self.managed_model_id)
                                        .map(|model| model.display_name.as_str())
                                        .unwrap_or("Select registered GGUF"),
                                )
                                .width(260.0)
                                .show_ui(ui, |ui| {
                                    for model in &models {
                                        if ui
                                            .selectable_label(
                                                self.managed_model_id == model.model_id,
                                                &model.display_name,
                                            )
                                            .clicked()
                                        {
                                            self.managed_model_id = model.model_id.clone();
                                            self.last_model_resource_telemetry =
                                                ModelResourceTelemetry::default();
                                            self.save_preferences();
                                        }
                                    }
                                });
                            if ui
                                .add_enabled(
                                    !setup_busy,
                                    egui::Button::new("Register existing GGUF..."),
                                )
                                .clicked()
                                && let Some(path) = rfd::FileDialog::new()
                                    .add_filter("GGUF model", &["gguf"])
                                    .pick_file()
                            {
                                self.start_managed_setup(
                                    ManagedSetupOperation::RegisterModel(path),
                                    "Hashing and registering the exact GGUF bytes without modifying the source file...",
                                );
                            }
                        });
                        let selected_model = self
                            .managed_local_runtime
                            .registered_models()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|model| model.model_id == self.managed_model_id);
                        if let Some(model) = selected_model {
                            ui.small(format!(
                                "Representation: sha256={} · size={} · quantization={}",
                                model.content_sha256,
                                format_model_bytes(model.size_bytes),
                                optional_text(model.quantization.as_deref()),
                            ));
                            if self.managed_execution_environment
                                == ManagedExecutionEnvironment::Wsl2Linux
                            {
                                match self.managed_local_runtime.additional_storage_for_environment(
                                    &model.model_id,
                                    self.managed_execution_environment,
                                ) {
                                    Ok(0) => {
                                        ui.small("Linux-native WSL model copy: verified/present.");
                                    }
                                    Ok(bytes) => {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.small(format!(
                                                "WSL2 needs an additional {} Linux-native copy of these same model bytes.",
                                                format_model_bytes(bytes)
                                            ));
                                            if ui
                                                .add_enabled(
                                                    !setup_busy,
                                                    egui::Button::new("Approve copy"),
                                                )
                                                .clicked()
                                            {
                                                self.start_managed_setup(
                                                    ManagedSetupOperation::PrepareModel {
                                                        model_id: model.model_id.clone(),
                                                        environment: self.managed_execution_environment,
                                                        duplicate_storage_approved: true,
                                                    },
                                                    "Copying the exact verified GGUF bytes into the dedicated Linux-native WSL model store...",
                                                );
                                            }
                                        });
                                    }
                                    Err(error) => ui.small(format!(
                                        "WSL model preparation unavailable: {error}"
                                    )),
                                }
                            }
                        } else {
                            ui.small(
                                "No managed model selected. Model weights are never downloaded merely because a model is recommended; register an existing GGUF or use an explicit future catalog acquisition action.",
                            );
                        }
                    }
                    ModelBackendPreference::Local => {
                        ui.horizontal(|ui| {
                            ui.label("Endpoint");
                            if ui
                                .add(egui::TextEdit::singleline(&mut self.local_model_endpoint).desired_width(300.0))
                                .changed()
                            {
                                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                                self.save_preferences();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Installed models");
                            if ui
                                .add_enabled(
                                    self.model_discovery.is_none(),
                                    egui::Button::new("Discover"),
                                )
                                .clicked()
                            {
                                self.start_model_discovery();
                            }
                            if self.model_discovery.is_some() {
                                ui.spinner();
                            }
                        });
                        let inventory = self.current_installed_inventory().cloned();
                        if let Some(inventory) = inventory.as_ref() {
                            ui.horizontal(|ui| {
                                ui.label("Detected model");
                                egui::ComboBox::from_id_salt("ai_studio_installed_model")
                                    .selected_text(if self.local_model_name.trim().is_empty() {
                                        "Select discovered model"
                                    } else {
                                        self.local_model_name.trim()
                                    })
                                    .width(260.0)
                                    .show_ui(ui, |ui| {
                                        for model in &inventory.models {
                                            if ui
                                                .selectable_label(
                                                    self.local_model_name == model.name,
                                                    &model.name,
                                                )
                                                .clicked()
                                            {
                                                self.local_model_name = model.name.clone();
                                                self.last_model_resource_telemetry =
                                                    ModelResourceTelemetry::default();
                                                self.save_preferences();
                                            }
                                        }
                                    });
                                ui.small(format!("{} found", inventory.models.len()));
                            });
                        } else {
                            ui.small(
                                "No installed-model inventory is loaded. Discovery is explicit and loopback-only.",
                            );
                        }
                        ui.horizontal(|ui| {
                            ui.label("Custom / exact ID");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.local_model_name)
                                        .desired_width(260.0)
                                        .hint_text("model:tag"),
                                )
                                .changed()
                            {
                                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                                self.save_preferences();
                            }
                        });
                        if let Some(inventory) = inventory.as_ref()
                            && let Some(model) = inventory
                                .models
                                .iter()
                                .find(|model| model.name == self.local_model_name)
                        {
                            ui.small(format!(
                                "Installed evidence: digest={} · size={} · parameters={} · quantization={} · family={} · backend={}",
                                optional_text(model.digest.as_deref()),
                                model
                                    .size_bytes
                                    .map(format_model_bytes)
                                    .unwrap_or_else(|| "n/a".to_owned()),
                                optional_text(model.parameter_size.as_deref()),
                                optional_text(model.quantization_level.as_deref()),
                                optional_text(model.family.as_deref()),
                                optional_text(inventory.backend_version.as_deref()),
                            ));
                        }
                    }
                    ModelBackendPreference::HostedApi | ModelBackendPreference::Enterprise => {
                        ui.horizontal(|ui| {
                            ui.label("HTTPS chat endpoint");
                            if ui
                                .add(
                                    egui::TextEdit::singleline(&mut self.hosted_model_endpoint)
                                        .desired_width(320.0)
                                        .hint_text("https://…/v1/chat/completions"),
                                )
                                .changed()
                            {
                                self.save_preferences();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Model");
                            if ui
                                .add(egui::TextEdit::singleline(&mut self.hosted_model_name).desired_width(260.0))
                                .changed()
                            {
                                self.save_preferences();
                            }
                        });
                        if self.model_backend == ModelBackendPreference::HostedApi {
                            ui.horizontal(|ui| {
                                ui.label("API credential");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.hosted_secret_draft)
                                        .password(true)
                                        .desired_width(220.0)
                                        .hint_text("stored with Windows DPAPI"),
                                );
                                if ui.button("Store securely").clicked() {
                                    match hosted_model_backend::store_api_key(
                                        &self.hosted_secret_path,
                                        &self.hosted_secret_draft,
                                    ) {
                                        Ok(()) => {
                                            self.hosted_secret_draft.clear();
                                            self.status = Some(
                                                "Hosted API credential stored in the machine-local OS-protected secret store.".to_owned(),
                                            );
                                        }
                                        Err(error) => self.status = Some(error.to_string()),
                                    }
                                }
                                if ui.button("Remove").clicked() {
                                    match hosted_model_backend::remove_api_key(&self.hosted_secret_path) {
                                        Ok(()) => {
                                            self.status = Some("Hosted API credential removed.".to_owned())
                                        }
                                        Err(error) => self.status = Some(error.to_string()),
                                    }
                                }
                            });
                            ui.small(if hosted_model_backend::credential_is_configured(
                                &self.hosted_secret_path,
                            ) {
                                "Credential status: configured. Secret value is never serialized or exposed to Remote AI Studio."
                            } else {
                                "Credential status: not configured."
                            });
                        } else {
                            ui.small(
                                "Enterprise authentication uses the organization-managed Windows identity/session; GameEngine stores no API key.",
                            );
                        }
                    }
                }
                let mut profile = match self.model_backend {
                    ModelBackendPreference::Local => NativeModelConfig::Local(LocalModelConfig {
                        endpoint: self.local_model_endpoint.clone(),
                        model: self.local_model_name.clone(),
                    })
                    .capability_profile(),
                    ModelBackendPreference::ManagedLocal => self
                        .managed_model_config()
                        .map(NativeModelConfig::Managed)
                        .map(|config| config.capability_profile())
                        .unwrap_or_else(|_| {
                            NativeModelConfig::Managed(ManagedLocalModelConfig {
                                state_root: self.managed_local_runtime.root().to_path_buf(),
                                environment: self.managed_execution_environment,
                                model_id: self.managed_model_id.clone(),
                                model_content_sha256: String::new(),
                                model_path: PathBuf::new(),
                                model_size_bytes: 0,
                                quantization: None,
                                runtime_tag: PINNED_LLAMA_CPP_TAG.to_owned(),
                                runtime_revision: PINNED_LLAMA_CPP_REVISION.to_owned(),
                                runtime_artifact_sha256: String::new(),
                                runtime_compatibility_version: "llama-server-openai-v1".to_owned(),
                            })
                            .capability_profile()
                        }),
                    ModelBackendPreference::HostedApi | ModelBackendPreference::Enterprise => {
                        NativeModelConfig::Hosted(HostedModelConfig {
                            endpoint: self.hosted_model_endpoint.clone(),
                            model: self.hosted_model_name.clone(),
                            auth_mode: if self.model_backend == ModelBackendPreference::HostedApi {
                                HostedAuthMode::ApiKey
                            } else {
                                HostedAuthMode::EnterpriseManaged
                            },
                            encrypted_secret_path: self.hosted_secret_path.clone(),
                        })
                        .capability_profile()
                    }
                };
                let recommendation_profiles = self
                    .model_catalog
                    .profiles_for_model(profile.backend_id, &profile.model_id);
                profile.benchmark_verified = !recommendation_profiles.is_empty();
                ui.small(model_capability_summary(&profile));
                if profile.model_id.trim().is_empty() {
                    ui.small("GameEngine status: no model selected.");
                } else if recommendation_profiles.is_empty() {
                    ui.small(
                        "GameEngine status: Compatible / unverified — this exact backend/model representation has no complete benchmark-qualified recommendation.",
                    );
                } else {
                    let labels = recommendation_profiles
                        .iter()
                        .map(|profile| profile.label())
                        .collect::<Vec<_>>()
                        .join(", " );
                    ui.small(format!(
                        "GameEngine status: Recommended · {labels} · corpus {BENCHMARK_CORPUS_VERSION}"
                    ));
                }
                if matches!(
                    self.model_backend,
                    ModelBackendPreference::Local | ModelBackendPreference::ManagedLocal
                ) {
                    ui.small(format!(
                        "Resource controls: unload/reload {} · CPU offload {} · GPU residency telemetry {} · memory telemetry {}",
                        capability_label(profile.resource_capabilities.unload_reload),
                        capability_label(profile.resource_capabilities.cpu_gpu_offload),
                        capability_label(profile.resource_capabilities.gpu_residency),
                        capability_label(profile.resource_capabilities.backend_memory_telemetry),
                    ));
                    ui.small(format!(
                        "Observed model resources: resident {} · model size {} · GPU residency {} · context {}",
                        telemetry_bool_label(&self.last_model_resource_telemetry.resident),
                        telemetry_bytes_label(
                            &self.last_model_resource_telemetry.representation_size_bytes
                        ),
                        telemetry_bytes_label(&self.last_model_resource_telemetry.gpu_residency_bytes),
                        telemetry_count_label(
                            &self.last_model_resource_telemetry.context_length_tokens,
                            "tokens"
                        ),
                    ));
                    ui.small(
                        "Provider-reported local model residency is shown with provenance; device-wide free VRAM and TTFT are never fabricated.",
                    );
                } else {
                    ui.small(
                        "Local GPU residency controls are unavailable for Hosted API and Enterprise backends; remote GPU state is not projected into local resource controls.",
                    );
                }
                ui.small(format!(
                    "Resource posture: {:?} · workload {:?} · reclaim {:?}",
                    self.resource_plan.presentation,
                    self.resolved_workload,
                    self.resource_plan.reclaim
                ));
                ui.small(self.model_routing_status());
                self.show_agent_benchmark(ui);
            });
    }

    fn show_agent_benchmark(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new(format!(
            "GameEngine Agent Benchmark · {} record(s) · {}",
            self.benchmark_records.len(),
            self.model_catalog.catalog_version
        ))
        .default_open(cfg!(feature = "visual-validation"))
        .show(ui, |ui| {
            ui.small(format!(
                "Versioned corpus: {BENCHMARK_CORPUS_VERSION}. Recommendations require complete, comparable GameEngine task evidence; third-party scores alone never qualify a model."
            ));
            for catalog_profile in CatalogProfile::ALL {
                if let Some(recommendation) = self.model_catalog.recommendation(catalog_profile) {
                    ui.group(|ui| {
                        ui.strong(format!(
                            "{} · {}",
                            catalog_profile.label(),
                            recommendation.candidate.model_id
                        ));
                        ui.small(format!(
                            "evidence={} runs · aggregate={} ms · benchmark={}",
                            recommendation.evidence_runs,
                            recommendation.aggregate_elapsed_ms,
                            recommendation.benchmark_version
                        ));
                        ui.small(format!(
                            "source={} · license={} · transfer={} · storage={}",
                            recommendation.candidate.source,
                            recommendation.candidate.license,
                            format_model_bytes(recommendation.candidate.transfer_size_bytes),
                            format_model_bytes(recommendation.candidate.storage_size_bytes),
                        ));
                        ui.small(format!(
                            "memory={} · context={} · modalities={} · tools={}",
                            recommendation.candidate.memory_guidance,
                            recommendation
                                .candidate
                                .context_limit
                                .map(|limit| limit.to_string())
                                .unwrap_or_else(|| "n/a".to_owned()),
                            list_or_none(&recommendation.candidate.modalities),
                            list_or_none(&recommendation.candidate.tool_capabilities),
                        ));
                    });
                } else {
                    ui.small(format!(
                        "{}: No benchmark-qualified recommendation yet.",
                        catalog_profile.label()
                    ));
                }
            }
            ui.small(
                "Model weights are never bundled or downloaded automatically. A future catalog acquisition flow must show source plus transfer/storage size before an explicit user action.",
            );
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label("Evidence task");
                let selected_label = benchmark_task(&self.benchmark_task_id)
                    .map(|task| task.label)
                    .unwrap_or("Unknown task");
                egui::ComboBox::from_id_salt("ai_studio_benchmark_task")
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        for task in BENCHMARK_TASKS {
                            ui.selectable_value(
                                &mut self.benchmark_task_id,
                                task.id.to_owned(),
                                task.label,
                            );
                        }
                    });
                if ui
                    .add_enabled(
                        self.benchmark_task_record_available(),
                        egui::Button::new("Record current evidence"),
                    )
                    .clicked()
                {
                    self.record_selected_benchmark();
                }
            });
            ui.small(
                "Choose the Evidence task before starting inference or a native run; its versioned identity is frozen at execution start. Record only when that result intentionally executes the frozen corpus task. Records are machine-local and omit prompts, conversation history, retrieved source text, project paths, and credentials; this feature never uploads private projects.",
            );
        });
    }

    fn model_routing_status(&self) -> String {
        let Ok(primary) = self.selected_native_model_config() else {
            return format!(
                "Measured routing: policy {MODEL_ROUTER_POLICY_VERSION} · unavailable until a primary model is selected."
            );
        };
        let policy = ModelRoutingPolicy::derive(
            primary.clone(),
            self.native_routing_candidates(&primary),
            &self.benchmark_records,
        );
        format!(
            "Measured routing: policy {MODEL_ROUTER_POLICY_VERSION} · {} benchmark-qualified specialist workload(s) · one AgentRun with provider-independent context handoff.",
            policy.adopted_specialist_count()
        )
    }

    fn current_installed_inventory(&self) -> Option<&InstalledModelInventory> {
        let endpoint = self.local_model_endpoint.trim().trim_end_matches('/');
        self.installed_model_inventory.as_ref().filter(|inventory| {
            inventory.endpoint.trim().trim_end_matches('/') == endpoint
        })
    }

    fn start_model_discovery(&mut self) {
        match InstalledModelDiscoveryTask::spawn(self.local_model_endpoint.clone()) {
            Ok(task) => {
                self.model_discovery = Some(task);
                self.status = Some(
                    "Discovering compatible models from the configured loopback backend..."
                        .to_owned(),
                );
            }
            Err(error) => {
                self.status = Some(format!("Installed-model discovery failed: {error}"));
            }
        }
    }

    fn poll_model_discovery(&mut self, context: &egui::Context) {
        let Some(task) = self.model_discovery.as_ref() else {
            return;
        };
        let Some(result) = task.poll() else {
            context.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        self.model_discovery = None;
        match result {
            Ok(inventory) => {
                let model_count = inventory.models.len();
                let backend = inventory
                    .backend_version
                    .as_deref()
                    .unwrap_or("version unavailable")
                    .to_owned();
                self.installed_model_inventory = Some(inventory);
                self.status = Some(format!(
                    "Discovered {model_count} installed model(s); backend {backend}."
                ));
            }
            Err(error) => {
                self.status = Some(format!("Installed-model discovery failed: {error}"));
            }
        }
    }

    fn start_managed_setup(&mut self, operation: ManagedSetupOperation, message: &str) {
        if self.managed_setup_task.is_some() {
            self.status = Some("A managed Local AI setup operation is already running.".to_owned());
            return;
        }
        match ManagedSetupTask::spawn(self.managed_local_runtime.clone(), operation) {
            Ok(task) => {
                self.managed_setup_task = Some(task);
                self.status = Some(message.to_owned());
            }
            Err(error) => self.status = Some(format!(
                "Managed Local AI setup failed to start ({}): {error}",
                error.layer().label()
            )),
        }
    }

    fn poll_managed_setup(&mut self, context: &egui::Context) {
        let Some(task) = self.managed_setup_task.as_ref() else {
            return;
        };
        let Some(result) = task.poll() else {
            context.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        self.managed_setup_task = None;
        match result {
            Ok(ManagedSetupResult::RuntimeInstalled(installation)) => {
                self.status = Some(format!(
                    "Managed Local AI runtime ready: llama.cpp {} @ {} · {}.",
                    installation.runtime_tag,
                    installation.runtime_revision,
                    installation.environment.label()
                ));
            }
            Ok(ManagedSetupResult::WslProvisioned) => {
                self.status = Some(
                    "Dedicated GameEngine-LocalAI WSL environment is provisioned. Install the pinned runtime next."
                        .to_owned(),
                );
            }
            Ok(ManagedSetupResult::ModelRegistered(model)) => {
                self.managed_model_id = model.model_id.clone();
                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                self.save_preferences();
                self.status = Some(format!(
                    "Registered existing GGUF without modifying its bytes: {} · sha256={}.",
                    model.display_name, model.content_sha256
                ));
            }
            Ok(ManagedSetupResult::ModelPrepared(path)) => {
                self.status = Some(format!(
                    "Verified managed model copy for {}: {}.",
                    self.managed_execution_environment.label(),
                    path.display()
                ));
            }
            Ok(ManagedSetupResult::EnvironmentRemoved(environment)) => {
                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                self.status = Some(format!(
                    "Removed the GameEngine-managed {} runtime environment. Registered user-owned GGUF source files were not deleted.",
                    environment.label()
                ));
            }
            Err(error) => {
                self.status = Some(format!(
                    "Managed Local AI setup failed ({}): {error}",
                    error.layer().label()
                ));
            }
        }
    }

    fn managed_model_config(&self) -> Result<ManagedLocalModelConfig, String> {
        if self.managed_model_id.trim().is_empty() {
            return Err("Register or select a managed GGUF model before starting inference.".to_owned());
        }
        self.managed_local_runtime
            .configuration_for(
                &self.managed_model_id,
                self.managed_execution_environment,
            )
            .map_err(|error| error.to_string())
    }

    fn managed_benchmark_inventory(
        &self,
        config: &ManagedLocalModelConfig,
    ) -> InstalledModelInventory {
        InstalledModelInventory {
            endpoint: format!(
                "managed://{}",
                config.environment.benchmark_id()
            ),
            backend_version: Some(config.benchmark_runtime_identity()),
            models: vec![InstalledLocalModel {
                name: config.model_id.clone(),
                digest: Some(config.model_content_sha256.clone()),
                size_bytes: Some(config.model_size_bytes),
                parameter_size: None,
                quantization_level: config.quantization.clone(),
                family: Some("managed llama.cpp GGUF".to_owned()),
            }],
        }
    }

    fn benchmark_task_record_available(&self) -> bool {
        let Some(task) = benchmark_task(&self.benchmark_task_id) else {
            return false;
        };
        if task.kind == BenchmarkTaskKind::ReadQuestion {
            return self
                .last_native_question_benchmark
                .as_ref()
                .is_some_and(|snapshot| snapshot.policy.task_id == task.id);
        }
        self.native_run_benchmark_context
            .as_ref()
            .is_some_and(|benchmark| {
                benchmark.task_id == task.id
                    && !benchmark.routed
                    && self.host.run(&benchmark.run_id).is_ok_and(|run| {
                        matches!(
                            run.state,
                            AgentRunState::Completed
                                | AgentRunState::Failed
                                | AgentRunState::Cancelled
                        )
                    })
            })
    }

    fn record_selected_benchmark(&mut self) {
        let Some(task) = benchmark_task(&self.benchmark_task_id).copied() else {
            self.status = Some("The selected benchmark task is unavailable.".to_owned());
            return;
        };
        let record = if task.kind == BenchmarkTaskKind::ReadQuestion {
            let Some(snapshot) = self.last_native_question_benchmark.clone() else {
                self.status = Some(
                    "Run the selected read-question corpus task before recording evidence."
                        .to_owned(),
                );
                return;
            };
            read_question_record(
                &snapshot.policy.task_id,
                &snapshot.metrics,
                snapshot.policy.inventory.as_ref(),
                snapshot.policy.quality,
                snapshot.policy.workload,
                &snapshot.policy.hardware,
            )
        } else {
            let Some(context) = self.native_run_benchmark_context.clone() else {
                self.status = Some(
                    "Run the selected native write-capable corpus task before recording evidence."
                        .to_owned(),
                );
                return;
            };
            if context.routed {
                self.status = Some(
                    "Routed native runs are not ADR 0142 single-model evidence. Run this corpus task without a specialist handoff before recording a model baseline."
                        .to_owned(),
                );
                return;
            }
            let run = match self.host.run(&context.run_id) {
                Ok(run) => run.clone(),
                Err(error) => {
                    self.status = Some(error.to_string());
                    return;
                }
            };
            agent_run_record(
                &context.task_id,
                &run,
                AgentRunBenchmarkIdentity {
                    backend_id: &context.backend_id,
                    model_id: &context.model_id,
                    inventory: context.inventory.as_ref(),
                    quality: context.quality,
                    workload: context.workload,
                    hardware: &context.hardware,
                },
            )
        };
        let record = match record {
            Ok(record) => record,
            Err(error) => {
                self.status = Some(format!("Benchmark evidence rejected: {error}"));
                return;
            }
        };
        let path = match self.benchmark_store.record(&record) {
            Ok(path) => path,
            Err(error) => {
                self.status = Some(format!("Could not store benchmark evidence: {error}"));
                return;
            }
        };
        match self.refresh_benchmark_catalog() {
            Ok(()) => {
                let file_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("benchmark record");
                self.status = Some(format!(
                    "Recorded machine-local benchmark evidence as {file_name}."
                ));
            }
            Err(error) => {
                self.status = Some(format!(
                    "Benchmark evidence was stored, but catalog refresh failed: {error}"
                ));
            }
        }
    }

    fn refresh_benchmark_catalog(&mut self) -> Result<(), String> {
        let records = self.benchmark_store.load()?;
        let catalog = CuratedModelCatalog::from_bundled_manifest(&records)?;
        self.benchmark_records = records;
        self.model_catalog = catalog;
        Ok(())
    }

    fn native_routing_candidates(&self, primary: &NativeModelConfig) -> Vec<NativeModelConfig> {
        let NativeModelConfig::Local(primary) = primary else {
            return Vec::new();
        };
        self.current_installed_inventory()
            .map(|inventory| {
                inventory
                    .models
                    .iter()
                    .filter(|model| model.name.trim() != primary.model.trim())
                    .map(|model| {
                        NativeModelConfig::Local(LocalModelConfig {
                            endpoint: primary.endpoint.clone(),
                            model: model.name.clone(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn selected_native_model_config(&self) -> Result<NativeModelConfig, String> {
        match self.model_backend {
            ModelBackendPreference::Local => {
                if self.local_model_name.trim().is_empty() {
                    return Err("Set an installed external local model before starting inference.".to_owned());
                }
                Ok(NativeModelConfig::Local(LocalModelConfig {
                    endpoint: self.local_model_endpoint.clone(),
                    model: self.local_model_name.clone(),
                }))
            }
            ModelBackendPreference::ManagedLocal => {
                self.managed_model_config().map(NativeModelConfig::Managed)
            }
            ModelBackendPreference::HostedApi | ModelBackendPreference::Enterprise => {
                if !self.hosted_model_endpoint.trim().starts_with("https://") {
                    return Err("Hosted and enterprise model endpoints must use HTTPS.".to_owned());
                }
                if self.hosted_model_name.trim().is_empty() {
                    return Err("Set a hosted model before starting inference.".to_owned());
                }
                if self.model_backend == ModelBackendPreference::HostedApi
                    && !hosted_model_backend::credential_is_configured(&self.hosted_secret_path)
                {
                    return Err("Store a hosted API credential before starting hosted inference.".to_owned());
                }
                Ok(NativeModelConfig::Hosted(HostedModelConfig {
                    endpoint: self.hosted_model_endpoint.clone(),
                    model: self.hosted_model_name.clone(),
                    auth_mode: if self.model_backend == ModelBackendPreference::HostedApi {
                        HostedAuthMode::ApiKey
                    } else {
                        HostedAuthMode::EnterpriseManaged
                    },
                    encrypted_secret_path: self.hosted_secret_path.clone(),
                }))
            }
        }
    }

    fn start_native_question(&mut self) {
        let config = match self.selected_native_model_config() {
            Ok(config) => config,
            Err(error) => {
                self.status = Some(error);
                return;
            }
        };
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
        if config.requires_network() {
            match self
                .host
                .check_session_permission(&session_id, AgentCapability::NetworkAccess)
            {
                Ok(PermissionCheck::Granted) => {
                    self.start_native_question_authorized(config, conversation, session_id)
                }
                Ok(PermissionCheck::RequiresApproval) => {
                    self.pending_question_permission = Some(PendingQuestionPermission {
                        session_id,
                        config,
                        conversation,
                    });
                    self.status = Some("Hosted inference requires Network access approval.".to_owned());
                }
                Ok(PermissionCheck::Denied) => {
                    self.status = Some("Permission denied: Network access.".to_owned());
                }
                Err(error) => self.status = Some(error.to_string()),
            }
        } else {
            self.start_native_question_authorized(config, conversation, session_id);
        }
    }

    fn resolve_pending_question_permission(&mut self, scope: ApprovalScope) {
        let Some(pending) = self.pending_question_permission.take() else {
            return;
        };
        if let Err(error) = self.host.resolve_session_permission(
            &pending.session_id,
            AgentCapability::NetworkAccess,
            scope,
        ) {
            self.status = Some(error.to_string());
            return;
        }
        if scope == ApprovalScope::Deny {
            self.status = Some("Denied Network access for hosted inference.".to_owned());
            return;
        }
        match self
            .host
            .check_session_permission(&pending.session_id, AgentCapability::NetworkAccess)
        {
            Ok(PermissionCheck::Granted) => self.start_native_question_authorized(
                pending.config,
                pending.conversation,
                pending.session_id,
            ),
            Ok(_) => self.status = Some("Network access was not granted.".to_owned()),
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    fn start_native_question_authorized(
        &mut self,
        config: NativeModelConfig,
        conversation: Vec<QuestionMessage>,
        session_id: String,
    ) {
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
        let inventory = match &config {
            NativeModelConfig::Local(_) => self.current_installed_inventory().cloned(),
            NativeModelConfig::Managed(config) => Some(self.managed_benchmark_inventory(config)),
            NativeModelConfig::Hosted(_) => None,
        };
        self.native_question_benchmark_policy = Some(NativeQuestionBenchmarkPolicy {
            task_id: self.benchmark_task_id.clone(),
            quality: self.quality_preference,
            workload: self.resolved_workload,
            hardware: self.benchmark_hardware.clone(),
            inventory,
        });
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
        config: NativeModelConfig,
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
            Err(error) => {
                self.native_question_benchmark_policy = None;
                self.status = Some(error.to_string());
            }
        }
    }

    fn selected_local_resource_config(&self) -> Option<LocalModelResourceConfig> {
        match self.model_backend {
            ModelBackendPreference::Local if !self.local_model_name.trim().is_empty() => {
                Some(LocalModelResourceConfig::Ollama(LocalModelConfig {
                    endpoint: self.local_model_endpoint.clone(),
                    model: self.local_model_name.clone(),
                }))
            }
            ModelBackendPreference::ManagedLocal => self
                .managed_model_config()
                .ok()
                .map(LocalModelResourceConfig::Managed),
            _ => None,
        }
    }

    fn begin_model_reload_after_authoritative_inspection(
        &mut self,
        state: AiStudioAuthoritativeState,
    ) {
        let continuation = ModelResourceContinuation::ResumeAfterEditing {
            run_id: self.active_run_id.clone(),
            state,
        };
        if self.model_resource_task.is_some() {
            self.status = Some(
                "Authoritative state was re-inspected, but model reload is waiting for the active resource transition to finish."
                    .to_owned(),
            );
            return;
        }
        let Some(config) = self.selected_local_resource_config() else {
            self.finish_model_resource_continuation(continuation);
            return;
        };
        let Some(operation) = resume_model_resource_operation_after_authoritative_inspection(
            config.capability_profile().resource_capabilities,
        ) else {
            self.finish_model_resource_continuation(continuation);
            return;
        };
        match ModelResourceTask::spawn(config, operation) {
            Ok(task) => {
                self.model_resource_task = Some(task);
                self.model_resource_continuation = Some(continuation);
                self.status = Some(
                    "Authoritative state re-inspected; reacquiring local model residency before Resume completes..."
                        .to_owned(),
                );
            }
            Err(error) => {
                self.status = Some(format!(
                    "Model reload could not start after authoritative-state re-inspection: {error}; continuing without claiming model residency was reacquired."
                ));
                self.finish_model_resource_continuation(continuation);
            }
        }
    }

    fn begin_model_residency_request(
        &mut self,
        request: ModelResidencyRequest,
        continuation: ModelResourceContinuation,
    ) {
        if self.model_resource_task.is_some() {
            self.status = Some(
                "A model resource transition is already active; the new transition was not started."
                    .to_owned(),
            );
            return;
        }
        let Some(config) = self.selected_local_resource_config() else {
            self.finish_model_resource_continuation(continuation);
            return;
        };
        let capabilities = config.capability_profile().resource_capabilities;
        let Some(operation) =
            resource_operation_for_residency_request(request, capabilities)
        else {
            self.finish_model_resource_continuation(continuation);
            return;
        };
        match ModelResourceTask::spawn(config, operation) {
            Ok(task) => {
                self.model_resource_task = Some(task);
                self.model_resource_continuation = Some(continuation);
                self.status = Some(format!(
                    "Applying verified local model resource transition {operation:?}..."
                ));
            }
            Err(error) => {
                self.status = Some(format!(
                    "Model resource transition could not start: {error}; continuing without claiming residency changed."
                ));
                self.finish_model_resource_continuation(continuation);
            }
        }
    }

    fn poll_model_resource_task(&mut self, context: &egui::Context) {
        let Some(task) = self.model_resource_task.as_ref() else {
            return;
        };
        let Some(result) = task.poll() else {
            context.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        self.model_resource_task = None;
        let continuation = self.model_resource_continuation.take();
        match result {
            Ok(transition) => {
                self.last_model_resource_telemetry = transition.after.clone();
                self.status = Some(format!(
                    "Verified local model resource transition {:?} in {} ms.",
                    transition.operation,
                    telemetry_u64_value(&transition.operation_latency_ms)
                ));
            }
            Err(error) => {
                self.status = Some(format!(
                    "Model resource transition failed: {error}; continuing without claiming residency changed."
                ));
            }
        }
        if let Some(continuation) = continuation {
            self.finish_model_resource_continuation(continuation);
        }
    }

    fn finish_model_resource_continuation(&mut self, continuation: ModelResourceContinuation) {
        if let Some(action) = model_resource_continuation_runtime_action(&continuation) {
            self.pending_runtime_action = Some(action);
            return;
        }
        match continuation {
            ModelResourceContinuation::RestoreForEditing => {
                unreachable!("restore continuation is handled before non-renderer continuations")
            }
            ModelResourceContinuation::LaunchManagedPlay { run_id } => {
                if self.active_run_id.as_deref() == Some(run_id.as_str())
                    && self
                        .host
                        .run(&run_id)
                        .is_ok_and(|run| run.state == AgentRunState::Playtesting)
                {
                    self.request_permission(
                        run_id,
                        AgentCapability::RuntimeLaunch,
                        PendingPermissionAction::LaunchPlaytest,
                    );
                }
            }
            ModelResourceContinuation::ResumeAfterEditing { run_id, state } => {
                let resource_status = self.status.take();
                let resume_status = if let Some(run_id) = run_id.as_deref() {
                    match self.host.resume_after_editing(run_id, state.into()) {
                        Ok(ResumeDisposition::ResumedUnchanged) => {
                            "Authoritative state is unchanged; the run may resume.".to_owned()
                        }
                        Ok(ResumeDisposition::ReinspectRequired) => {
                            "User edits changed authoritative state; the run returned to inspection."
                                .to_owned()
                        }
                        Ok(ResumeDisposition::RepairRequired) => {
                            "User edits changed acceptance-relevant state; the run returned to repair."
                                .to_owned()
                        }
                        Err(error) => {
                            self.status = Some(error.to_string());
                            return;
                        }
                    }
                } else {
                    "Authoritative Editor state re-inspected.".to_owned()
                };
                self.editing_interrupted = false;
                self.status = Some(match resource_status {
                    Some(resource_status)
                        if resource_status.starts_with("Verified local model resource transition Reload")
                            || resource_status.starts_with("Model resource transition failed:")
                            || resource_status.starts_with("Model reload could not start after authoritative-state re-inspection:") =>
                    {
                        format!("{resume_status} {resource_status}")
                    }
                    _ => resume_status,
                });
                if self.active_runtime_mode == Some(AgentRuntimeMode::Native)
                    && let Some(run_id) = run_id
                    && let Err(error) = self.start_native_agent_turn(
                        &run_id,
                        Some("Manual editing ended. Authoritative Editor state was re-inspected; stale assumptions were rejected by AgentHost before this turn.".to_owned()),
                        Vec::new(),
                    )
                {
                    self.fail_run(&run_id, error);
                }
            }
        }
    }

    fn save_preferences(&mut self) {
        let preferences = AiStudioPreferences {
            schema_version: AI_STUDIO_PREFERENCES_SCHEMA_VERSION,
            quality_preference: self.quality_preference,
            confinement_requirement: self.confinement_requirement,
            external_agent_provider: self.external_provider_kind,
            model_backend: self.model_backend,
            managed_execution_environment: self.managed_execution_environment,
            managed_model_id: self.managed_model_id.clone(),
            local_model_endpoint: self.local_model_endpoint.clone(),
            local_model_name: self.local_model_name.clone(),
            hosted_model_endpoint: self.hosted_model_endpoint.clone(),
            hosted_model_name: self.hosted_model_name.clone(),
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
            && !self.restore_for_editing
            && self.pending_runtime_action.is_none();
        let session_id = self
            .native_question_session
            .take()
            .unwrap_or_else(|| self.selected_session.clone());
        let benchmark_policy = self.native_question_benchmark_policy.take();
        match result {
            Ok(answer) => {
                if let Some(policy) = benchmark_policy {
                    self.last_native_question_benchmark = Some(NativeQuestionBenchmarkSnapshot {
                        metrics: answer.metrics.clone(),
                        policy,
                    });
                }
                if matches!(
                    self.model_backend,
                    ModelBackendPreference::Local | ModelBackendPreference::ManagedLocal
                ) {
                    self.last_model_resource_telemetry = answer.resource_telemetry.clone();
                }
                let message = format_native_answer(&answer);
                match self.host.append_message(
                    &session_id,
                    ConversationRole::Assistant,
                    message,
                ) {
                    Ok(()) => {
                        self.status = Some(format!(
                            "Model backend {} answered with {} retrieved evidence source(s) in {} ms.",
                            answer.metrics.backend_id, answer.sources.len(), answer.metrics.elapsed_ms
                        ));
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            Err(error) => self.status = Some(error.to_string()),
        }
        if self.restore_for_editing {
            self.begin_model_residency_request(
                interrupt_model_residency_request(),
                ModelResourceContinuation::RestoreForEditing,
            );
        } else if restore_after_inference {
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

    fn native_runtime_busy(&self) -> bool {
        self.native_agent_runtime
            .as_ref()
            .is_some_and(NativeAgentRuntime::is_busy)
            || self.native_mcp_task.is_some()
    }

    fn prepare_native_workspace(&mut self, run_id: &str) -> Result<(), String> {
        if self.code_workspace.is_some() {
            return Ok(());
        }
        let (workspace_root, baseline_path) = self
            .host
            .workspace_paths(run_id)
            .map_err(|error| error.to_string())?;
        let workspace = CodeWorkspace::open_or_create(
            &self.project_root,
            workspace_root,
            baseline_path,
        )
        .map_err(|error| error.to_string())?;
        self.host
            .record_event(
                run_id,
                AgentEventKind::CodeWorkspacePrepared,
                "Prepared isolated managed code workspace for the native AgentRuntime.",
            )
            .map_err(|error| error.to_string())?;
        self.code_workspace = Some(workspace);
        Ok(())
    }

    fn start_native_agent_execution(&mut self, run_id: &str) -> Result<(), String> {
        self.prepare_native_workspace(run_id)?;
        if self.native_agent_runtime.is_none() {
            return Err("Native AgentRuntime has no frozen ModelBackend configuration.".to_owned());
        }
        if self
            .host
            .run(run_id)
            .is_ok_and(|run| run.state == AgentRunState::Inspecting)
        {
            self.host
                .transition_run(
                    run_id,
                    AgentRunState::Executing,
                    "Native AgentRuntime started from the immutable proposal snapshot.",
                )
                .map_err(|error| error.to_string())?;
        }
        self.start_native_agent_turn(run_id, None, Vec::new())
    }

    fn start_native_agent_turn(
        &mut self,
        run_id: &str,
        context: Option<String>,
        images: Vec<Vec<u8>>,
    ) -> Result<(), String> {
        let run = self.host.run(run_id).map_err(|error| error.to_string())?.clone();
        let (backend_label, routing_summary, routing_decisions) = {
            let runtime = self
                .native_agent_runtime
                .as_mut()
                .ok_or_else(|| "Native AgentRuntime is not initialized.".to_owned())?;
            runtime
                .start_turn(&run, context.as_deref(), images)
                .map_err(|error| error.to_string())?;
            (
                runtime.backend_label(),
                runtime.routing_policy_summary(),
                runtime.take_routing_decisions(),
            )
        };
        if routing_decisions.iter().any(|decision| decision.context_handoff)
            && let Some(benchmark) = self.native_run_benchmark_context.as_mut()
            && benchmark.run_id == run_id
        {
            benchmark.routed = true;
        }
        for decision in routing_decisions {
            self.host
                .record_semantic_progress(run_id, "model_routing", decision.audit_summary())
                .map_err(|error| error.to_string())?;
        }
        self.status = Some(format!(
            "{} is reasoning in {:?}; {}. Managed tools remain host-owned.",
            backend_label,
            run.state,
            routing_summary
        ));
        Ok(())
    }

    fn record_native_result_and_continue(
        &mut self,
        run_id: &str,
        tool: impl Into<String>,
        success: bool,
        result: impl Into<String>,
    ) {
        let result = result.into();
        let record = self
            .native_agent_runtime
            .as_mut()
            .ok_or_else(|| "Native AgentRuntime is not initialized.".to_owned())
            .and_then(|runtime| {
                runtime
                    .record_tool_result(tool.into(), success, result.clone())
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = record {
            self.fail_run(run_id, error);
            return;
        }
        if let Err(error) = self.start_native_agent_turn(run_id, None, Vec::new()) {
            self.fail_run(run_id, error);
        }
    }

    fn poll_native_agent_runtime(&mut self, context: &egui::Context) {
        let result = match self.native_agent_runtime.as_mut() {
            Some(runtime) => match runtime.poll() {
                Some(result) => result,
                None if runtime.is_busy() => {
                    context.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                None => return,
            },
            None => return,
        };
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let turn = match result {
            Ok(turn) => turn,
            Err(error) => {
                self.fail_run(&run_id, error.to_string());
                return;
            }
        };
        if !turn.summary.trim().is_empty()
            && let Err(error) = self.host.record_semantic_progress(
                &run_id,
                "native_reasoning",
                turn.summary.clone(),
            )
        {
            self.fail_run(&run_id, error.to_string());
            return;
        }
        if let Err(error) = self
            .host
            .record_working_state_update(&run_id, turn.working_state.into())
        {
            self.fail_run(&run_id, error.to_string());
            return;
        }
        let action_validation = self
            .host
            .run(&run_id)
            .map_err(|error| error.to_string())
            .and_then(|run| {
                self.native_agent_runtime
                    .as_ref()
                    .ok_or_else(|| "Native AgentRuntime is not initialized.".to_owned())?
                    .validate_action(run, &turn.action)
                    .map_err(|error| error.to_string())
            });
        if let Err(error) = action_validation {
            let _ = self.host.record_tool_action(
                &run_id,
                "native.policy",
                error.clone(),
                Some(false),
            );
            self.record_native_result_and_continue(&run_id, "native.policy", false, error);
            return;
        }
        self.handle_native_action(&run_id, turn.action);
    }

    fn handle_native_action(&mut self, run_id: &str, action: NativeAgentAction) {
        match action {
            NativeAgentAction::McpCall { tool, arguments } => {
                if mcp_write(&tool)
                    && let Err(error) = self.host.acquire_work_claims(
                        run_id,
                        [AgentWorkClaim::shared_resource("canonical_authoring")],
                    )
                {
                    let message = format!(
                        "MCP mutation is waiting for work ownership: {error}"
                    );
                    let _ = self.host.record_tool_action(
                        run_id,
                        tool.clone(),
                        "work ownership",
                        Some(false),
                    );
                    self.record_native_result_and_continue(run_id, tool, false, message);
                    return;
                }
                match NativeMcpTask::spawn(
                    self.connection.endpoint.clone(),
                    self.connection.authorization_token.clone(),
                    tool.clone(),
                    arguments,
                ) {
                    Ok(task) => {
                        self.pending_native_mcp_tool = Some(tool.clone());
                        self.native_mcp_task = Some(task);
                        self.status = Some(format!(
                            "Native AgentRuntime requested typed live Editor MCP tool `{tool}`."
                        ));
                    }
                    Err(error) => {
                        let _ = self.host.record_tool_action(
                            run_id,
                            tool.clone(),
                            "MCP dispatch preparation",
                            Some(false),
                        );
                        self.record_native_result_and_continue(run_id, tool, false, error);
                    }
                }
            }
            NativeAgentAction::CodeWrite { path, text } => {
                if let Err(error) = self
                    .host
                    .acquire_work_claims(run_id, [AgentWorkClaim::code_path(path.clone())])
                {
                    let message = format!(
                        "Managed code write is waiting for work ownership: {error}"
                    );
                    let _ = self.host.record_tool_action(
                        run_id,
                        "workspace.code_write",
                        path.clone(),
                        Some(false),
                    );
                    self.record_native_result_and_continue(
                        run_id,
                        format!("code_write:{path}"),
                        false,
                        message,
                    );
                    return;
                }
                let result = self
                    .code_workspace
                    .as_ref()
                    .ok_or_else(|| "Managed code workspace is unavailable.".to_owned())
                    .and_then(|workspace| {
                        workspace
                            .write_text(PathBuf::from(&path).as_path(), &text)
                            .map_err(|error| error.to_string())
                    });
                let success = result.is_ok();
                let message = result
                    .map(|()| format!("Wrote `{path}` inside the isolated code workspace."))
                    .unwrap_or_else(|error| error);
                let _ = self.host.record_tool_action(
                    run_id,
                    "workspace.code_write",
                    path.clone(),
                    Some(success),
                );
                self.record_native_result_and_continue(
                    run_id,
                    format!("code_write:{path}"),
                    success,
                    message,
                );
            }
            NativeAgentAction::RuntimeInput { input } => {
                let result = serde_json::from_value::<ProviderRuntimeInput>(input)
                    .map_err(|error| error.to_string())
                    .and_then(|input| input.command());
                match result {
                    Ok(command) => {
                        self.managed_candidate_input_recipe.push(command);
                        let _ = self.host.record_semantic_progress(
                            run_id,
                            "runtime_input_plan",
                            format!("Native runtime planned managed input {command:?}."),
                        );
                        self.record_native_result_and_continue(
                            run_id,
                            "runtime_input",
                            true,
                            "Input was added to the host-managed Play recipe.",
                        );
                    }
                    Err(error) => self.record_native_result_and_continue(
                        run_id,
                        "runtime_input",
                        false,
                        error,
                    ),
                }
            }
            NativeAgentAction::CompletionGate {
                gate,
                status,
                message,
            } => {
                let result = self
                    .host
                    .record_completion_gate(run_id, &gate, status, message)
                    .map_err(|error| error.to_string());
                if gate == "visual_evaluation" {
                    match result {
                        Ok(()) => {
                            let interactions_passed = match status {
                                CompletionStatus::Passed => Some(true),
                                CompletionStatus::Failed => Some(false),
                                CompletionStatus::Pending | CompletionStatus::NotApplicable => None,
                            };
                            if let Err(error) = self.host.record_playtest_result(run_id, true, interactions_passed, "Native visual evaluation completed against the host-captured managed Play frame.") {
                                self.record_runtime_evaluation_failure(run_id, error.to_string());
                            } else {
                                self.finish_runtime_evaluation(run_id, Some(0));
                            }
                        }
                        Err(error) => self.record_runtime_evaluation_failure(run_id, error),
                    }
                } else {
                    let success = result.is_ok();
                    let message = result
                        .map(|()| format!("Host accepted completion gate `{gate}` as {status:?}."))
                        .unwrap_or_else(|error| error);
                    self.record_native_result_and_continue(
                        run_id,
                        format!("completion_gate:{gate}"),
                        success,
                        message,
                    );
                }
            }
            NativeAgentAction::Progress { step, detail } => {
                let result = self
                    .host
                    .record_semantic_progress(run_id, step.clone(), detail)
                    .map_err(|error| error.to_string());
                let success = result.is_ok();
                self.record_native_result_and_continue(
                    run_id,
                    format!("progress:{step}"),
                    success,
                    result.map(|()| "Progress recorded.".to_owned()).unwrap_or_else(|e| e),
                );
            }
            NativeAgentAction::AwaitUser { question } => {
                if let Err(error) = self.host.append_message(
                    &self.selected_session,
                    ConversationRole::Assistant,
                    question.clone(),
                ) {
                    self.fail_run(run_id, error.to_string());
                    return;
                }
                if let Err(error) = self.host.transition_run(
                    run_id,
                    AgentRunState::AwaitingUser,
                    "Native AgentRuntime requires user input before continuing.",
                ) {
                    self.fail_run(run_id, error.to_string());
                    return;
                }
                self.status = Some(question);
            }
            NativeAgentAction::ReadyForValidation => {
                if let Some(runtime) = self.native_agent_runtime.as_mut() {
                    let _ = runtime.record_tool_result(
                        "ready_for_validation",
                        true,
                        "Control returned to AgentHost managed validation.",
                    );
                }
                self.finish_provider_execution(run_id, None);
            }
        }
    }

    fn poll_native_mcp(&mut self, context: &egui::Context) {
        let Some(task) = self.native_mcp_task.as_ref() else {
            return;
        };
        let Some(result) = task.poll() else {
            context.request_repaint_after(std::time::Duration::from_millis(50));
            return;
        };
        self.native_mcp_task = None;
        let Some(run_id) = self.active_run_id.clone() else {
            self.pending_native_mcp_tool = None;
            return;
        };
        let tool = self
            .pending_native_mcp_tool
            .take()
            .unwrap_or_else(|| "mcp.unknown".to_owned());
        match result {
            Ok(value) => {
                let _ = self.host.record_tool_action(
                    &run_id,
                    tool.clone(),
                    "typed live Editor MCP call",
                    Some(true),
                );
                self.record_native_result_and_continue(
                    &run_id,
                    tool,
                    true,
                    value.to_string(),
                );
            }
            Err(error) => {
                let _ = self.host.record_tool_action(
                    &run_id,
                    tool.clone(),
                    "typed live Editor MCP call",
                    Some(false),
                );
                self.record_native_result_and_continue(&run_id, tool, false, error);
            }
        }
    }

    fn current_external_provider_status(&self) -> ExternalAgentProviderStatus {
        if self.external_provider_kind == ExternalAgentProviderKind::Generic {
            ExternalAgentProviderStatus::generic(!self.provider_program.trim().is_empty())
        } else if self.external_provider_status.kind == self.external_provider_kind {
            self.external_provider_status.clone()
        } else {
            ExternalAgentProviderStatus::unchecked(self.external_provider_kind)
        }
    }

    fn refresh_external_provider_status(&mut self) {
        self.external_provider_status =
            probe_provider(self.external_provider_kind, &self.provider_program);
    }

    fn external_provider_is_requested(&self) -> bool {
        self.external_provider_kind != ExternalAgentProviderKind::Generic
            || !self.provider_program.trim().is_empty()
    }

    fn external_provider_is_ready(&self) -> bool {
        self.external_provider_is_requested() && self.current_external_provider_status().ready()
    }

    fn selected_external_provider(&self) -> Result<Option<ExternalAgentProviderKind>, String> {
        if !self.external_provider_is_requested() {
            return Ok(None);
        }
        let status = self.current_external_provider_status();
        if !status.ready() {
            return Err(format!(
                "{} is not ready (discovery: {}, authentication: {}). Refresh provider status or choose another runtime.",
                self.external_provider_kind.label(),
                status.discovery.label(),
                status.auth.label(),
            ));
        }
        Ok(Some(self.external_provider_kind))
    }

    fn show_provider(&mut self, ui: &mut egui::Ui) {
        ui.heading("Run");
        let previous_provider = self.external_provider_kind;
        let mut refresh_provider = false;
        ui.horizontal(|ui| {
            ui.label("External agent provider");
            egui::ComboBox::from_id_salt("ai_studio_external_agent_provider")
                .selected_text(self.external_provider_kind.label())
                .show_ui(ui, |ui| {
                    for provider in ExternalAgentProviderKind::ALL {
                        ui.selectable_value(
                            &mut self.external_provider_kind,
                            provider,
                            provider.label(),
                        );
                    }
                });
            if ui.button("Refresh status").clicked() {
                refresh_provider = true;
            }
        });
        if previous_provider != self.external_provider_kind {
            self.external_provider_status =
                ExternalAgentProviderStatus::unchecked(self.external_provider_kind);
            self.save_preferences();
        }
        if refresh_provider {
            self.refresh_external_provider_status();
        }
        let provider_status = self.current_external_provider_status();
        let capabilities = self.external_provider_kind.capabilities();
        ui.group(|ui| {
            ui.strong(format!("{} status", self.external_provider_kind.label()));
            ui.label(format!(
                "Discovery: {} · Authentication: {}",
                provider_status.discovery.label(),
                provider_status.auth.label(),
            ));
            ui.small(format!(
                "Capabilities: provider auth {} · MCP injection {} · structured events {} · host cancellation {}",
                capabilities.provider_managed_auth,
                capabilities.mcp_injection,
                capabilities.structured_events,
                capabilities.host_cancellation,
            ));
            ui.small(
                "Provider-managed login remains provider-owned. GameEngine stores no provider credential and reports only sanitized adapter status remotely.",
            );
        });
        if self.external_provider_kind == ExternalAgentProviderKind::Generic {
            ui.horizontal(|ui| {
                ui.label("Compatible agent program");
                ui.text_edit_singleline(&mut self.provider_program);
            });
            ui.horizontal(|ui| {
                ui.label("Arguments");
                ui.text_edit_singleline(&mut self.provider_args);
            });
        }
        let previous_confinement_requirement = self.confinement_requirement;
        ui.horizontal(|ui| {
            ui.label("External process confinement");
            egui::ComboBox::from_id_salt("ai_studio_process_confinement")
                .selected_text(self.confinement_requirement.label())
                .show_ui(ui, |ui| {
                    for requirement in [
                        AgentConfinementRequirement::AllowApplicationPolicyOnly,
                        AgentConfinementRequirement::RequireProviderOrOsConfinement,
                    ] {
                        ui.selectable_value(
                            &mut self.confinement_requirement,
                            requirement,
                            requirement.label(),
                        );
                    }
                });
        });
        if previous_confinement_requirement != self.confinement_requirement {
            self.save_preferences();
        }
        let confinement_status = self
            .active_run_id
            .as_deref()
            .and_then(|run_id| self.host.run(run_id).ok())
            .and_then(|run| run.confinement_profile.as_ref())
            .map(|profile| profile.summary())
            .unwrap_or_else(|| {
                "No external process confinement profile has been recorded. Generic external launches are application-policy-only; the native AgentRuntime is not an external child-process sandbox."
                    .to_owned()
            });
        ui.group(|ui| {
            ui.strong("Confinement status");
            ui.label(confinement_status);
            ui.small(
                "GameEngine application permissions remain authoritative. External providers are not treated as sandboxed unless their launch path reports enforceable provider/OS confinement.",
            );
            if self.confinement_requirement.requires_enforced_confinement() {
                ui.small(
                    "Fail-closed policy: an external agent will not start through the generic process runtime unless a provider/OS confinement adapter can satisfy this requirement.",
                );
            }
        });
        ui.small(
            "Go uses the selected first-class external provider when it is ready, the Generic command when configured, or otherwise the selected Managed Local, external local, Hosted API, or Enterprise ModelBackend. External and managed adapters remain clients of the same immutable proposal, Agent Host permissions and work claims, code workspace, validation, Play/frame evidence, and completion contract.",
        );
        let mut stop_requested = false;
        let mut interrupt_requested = false;
        let mut resume_requested = false;
        ui.horizontal_wrapped(|ui| {
            let can_go = self.process.is_none()
                && !self.native_runtime_busy()
                && self.pending_permission.is_none()
                && self.pending_question_permission.is_none()
                && (self.external_provider_is_ready()
                    || (!self.external_provider_is_requested()
                        && self.selected_native_model_config().is_ok()));
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
                && self.model_resource_task.is_none()
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
            self.model_resource_continuation = None;
            self.restore_for_editing = false;
            if let Some(runtime) = self.native_agent_runtime.as_mut() { runtime.interrupt(); }
            if let Some(task) = self.native_mcp_task.as_ref() { task.interrupt(); }
            self.native_mcp_task = None;
            self.pending_native_mcp_tool = None;
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
            self.native_agent_runtime = None;
            self.active_runtime_mode = None;
            self.active_external_provider = None;
            self.active_external_program = None;
            self.active_external_args = None;
            self.external_provider_diagnostics = ExternalAgentDiagnostics::default();
            self.pending_external_work_owner = None;
        }
        if interrupt_requested {
            if let Some(task) = self.native_question.as_ref() { task.interrupt(); }
            if let Some(runtime) = self.native_agent_runtime.as_mut() { runtime.interrupt(); }
            if let Some(task) = self.native_mcp_task.as_ref() { task.interrupt(); }
            self.native_mcp_task = None;
            self.pending_native_mcp_tool = None;
            self.pending_native_question_start = None;
            self.restore_for_editing = true;
            if self.native_question.is_some() {
                self.status = Some(
                    "Stopping inference at a safe backend boundary before releasing local model residency..."
                        .to_owned(),
                );
            } else {
                self.begin_model_residency_request(
                    interrupt_model_residency_request(),
                    ModelResourceContinuation::RestoreForEditing,
                );
            }
        }
        if resume_requested {
            self.pending_runtime_action = Some(AiStudioRuntimeAction::InspectAuthoritativeState);
            self.status = Some(
                "Re-inspecting authoritative Editor state before Resume...".to_owned(),
            );
        }
    }

    fn show_permission_prompt(&mut self, ui: &mut egui::Ui) {
        if self.pending_question_permission.is_some() {
            ui.separator();
            ui.group(|ui| {
                ui.strong("Approval required");
                ui.label(AgentCapability::NetworkAccess.label());
                ui.small("Hosted question context leaves this machine only after approval. Credentials never enter the permission record.");
                ui.horizontal(|ui| {
                    for (label, scope) in [
                        ("Allow once", ApprovalScope::Once),
                        ("This session", ApprovalScope::Run),
                        ("This project", ApprovalScope::Project),
                        ("Deny", ApprovalScope::Deny),
                    ] {
                        if ui.button(label).clicked() {
                            self.resolve_pending_question_permission(scope);
                        }
                    }
                });
            });
            return;
        }
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
        let external_provider = self.selected_external_provider()?;
        let (mode, provider_label, native_config) = if let Some(provider) = external_provider {
            (
                AgentRuntimeMode::External,
                provider.run_label().to_owned(),
                None,
            )
        } else {
            let config = self.selected_native_model_config()?;
            let label = config.label();
            (AgentRuntimeMode::Native, label, Some(config))
        };
        let native_requires_network = native_config
            .as_ref()
            .is_some_and(NativeModelConfig::requires_network);
        let has_non_trivial_work = !self.proposal_draft.planned_code_changes.is_empty()
            || !self.proposal_draft.planned_project_changes.is_empty()
            || !self.proposal_draft.planned_assets.is_empty();
        let benchmark_workload = classify_workload(WorkloadSignals {
            strong_reasoning_required: has_non_trivial_work
                && matches!(
                    self.quality_preference,
                    QualityPreference::Balanced | QualityPreference::Deep
                ),
            model_judgement_required: true,
            ..WorkloadSignals::default()
        });
        let native_benchmark_identity = native_config.as_ref().map(|config| {
            let inventory = match config {
                NativeModelConfig::Local(_) => self.current_installed_inventory().cloned(),
                NativeModelConfig::Managed(config) => {
                    Some(self.managed_benchmark_inventory(config))
                }
                NativeModelConfig::Hosted(_) => None,
            };
            (config.backend_id().to_owned(), config.model_id(), inventory)
        });
        let run_id = self.host.start_run_authorized(&self.selected_session, authorized_proposal_version, provider_label).map_err(|error| error.to_string())?;
        self.active_run_id = Some(run_id.clone());
        self.active_runtime_mode = Some(mode);
        self.active_external_provider = external_provider;
        self.active_external_program = external_provider.map(|_| self.provider_program.clone());
        self.active_external_args = external_provider.map(|_| self.provider_args.clone());
        self.external_provider_diagnostics = ExternalAgentDiagnostics::default();
        let routing_candidates = native_config
            .as_ref()
            .map(|config| self.native_routing_candidates(config))
            .unwrap_or_default();
        self.native_agent_runtime = native_config.map(|config| {
            NativeAgentRuntime::configured_routed(
                config,
                routing_candidates,
                &self.benchmark_records,
            )
        });
        self.native_run_benchmark_context = native_benchmark_identity.map(
            |(backend_id, model_id, inventory)| NativeRunBenchmarkContext {
                run_id: run_id.clone(),
                task_id: self.benchmark_task_id.clone(),
                backend_id,
                model_id,
                quality: self.quality_preference,
                workload: benchmark_workload,
                hardware: self.benchmark_hardware.clone(),
                inventory,
                routed: false,
            },
        );
        self.native_mcp_task = None;
        self.pending_native_mcp_tool = None;
        self.code_workspace = None;
        self.pending_code_changes.clear();
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
        match mode {
            AgentRuntimeMode::External => self.request_permission(
                run_id.clone(),
                AgentCapability::ExternalAgentProcess,
                PendingPermissionAction::LaunchExternalAgent,
            ),
            AgentRuntimeMode::Native if native_requires_network => self.request_permission(
                run_id.clone(),
                AgentCapability::NetworkAccess,
                PendingPermissionAction::StartNativeAgent,
            ),
            AgentRuntimeMode::Native => self.start_native_agent_execution(&run_id)?,
        }
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
            PendingPermissionAction::StartNativeAgent => {
                if let Err(error) = self.start_native_agent_execution(run_id) {
                    self.fail_run(run_id, error);
                }
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
        let Some(provider_kind) = self.active_external_provider else {
            self.fail_run(
                run_id,
                "External AgentRuntime has no provider snapshot for this run.".to_owned(),
            );
            return;
        };
        let generic_program = self.active_external_program.clone().unwrap_or_default();
        let generic_args_text = self.active_external_args.clone().unwrap_or_default();
        self.external_provider_diagnostics = ExternalAgentDiagnostics::default();
        if purpose == ExternalAgentPurpose::BuildOrRepair {
            match self.host.acquire_work_claims(
                run_id,
                [AgentWorkClaim::shared_resource("canonical_authoring")],
            ) {
                Ok(()) => self.pending_external_work_owner = None,
                Err(AgentHostError::WorkClaimConflict { owner_run_id, .. }) => {
                    self.pending_external_work_owner = Some((purpose, owner_run_id.clone()));
                    self.status = Some(format!(
                        "External agent is waiting for canonical authoring ownership held by run {owner_run_id}."
                    ));
                    return;
                }
                Err(error) => {
                    self.fail_run(
                        run_id,
                        format!("Could not acquire external agent authoring ownership: {error}"),
                    );
                    return;
                }
            }
        }
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
        let network_policy = match self.host.run(run_id) {
            Ok(run)
                if run
                    .proposal_snapshot
                    .requested_capabilities
                    .contains(&AgentCapability::NetworkAccess) =>
            {
                AgentConfinementNetworkPolicy::ManagedNetworkAccess
            }
            Ok(_) => AgentConfinementNetworkPolicy::LoopbackOnly,
            Err(error) => {
                self.fail_run(
                    run_id,
                    format!("Could not resolve confinement policy from run capabilities: {error}"),
                );
                return;
            }
        };
        let confinement_request = AgentConfinementRequest::new(
            self.confinement_requirement,
            workspace.root().to_path_buf(),
            self.connection.endpoint.clone(),
            network_policy,
        );
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
                OsString::from(&proposal_json),
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
        if let Some(repair_context) = repair_context.as_deref() {
            environment.push((
                OsString::from("GAMEENGINE_AGENT_REPAIR_CONTEXT"),
                OsString::from(repair_context),
            ));
        }
        let runtime_evaluation_context = runtime_observation.as_ref().map(|observation| {
            format!(
                "Evaluate host-captured managed Play frame {} ({}x{}). Inspect the image at GAMEENGINE_AGENT_CAPTURE_PATH. Do not mutate project or workspace state during this evaluation. Emit completion_gate with gate=visual_evaluation and status=passed or failed before any failing playtest_result, then emit playtest_result for the exercised interaction scenario when evidence supports it. A pass without this host-owned frame is rejected.",
                observation.artifact_id, observation.width, observation.height
            )
        });
        if let Some(observation) = runtime_observation.as_ref() {
            environment.push((
                OsString::from("GAMEENGINE_AGENT_CAPTURE_PATH"),
                observation.path.as_os_str().to_os_string(),
            ));
        }
        if let Some(context) = runtime_evaluation_context.as_deref() {
            environment.push((
                OsString::from("GAMEENGINE_AGENT_RUNTIME_EVALUATION_CONTEXT"),
                OsString::from(context),
            ));
        }
        let provider_prompt = external_agent_provider_prompt(
            &proposal_json,
            repair_context.as_deref(),
            runtime_evaluation_context.as_deref(),
        );
        let generic_args = split_direct_args(&generic_args_text);
        let launch_plan = match build_launch_plan(
            provider_kind,
            &generic_program,
            &generic_args,
            &provider_prompt,
            &self.connection.endpoint,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                match purpose {
                    ExternalAgentPurpose::BuildOrRepair => self.fail_run(run_id, error),
                    ExternalAgentPurpose::RuntimeEvaluation => {
                        self.record_runtime_evaluation_failure(run_id, error);
                    }
                }
                return;
            }
        };
        match ExternalAgentProcess::spawn(
            launch_plan.program.as_os_str(),
            &launch_plan.args,
            workspace.root(),
            &environment,
            &confinement_request,
        ) {
            Ok(mut process) => {
                let confinement_profile = process.confinement_profile().clone();
                if let Err(error) = self
                    .host
                    .record_confinement_profile(run_id, confinement_profile.clone())
                {
                    let _ = process.cancel();
                    self.fail_run(
                        run_id,
                        format!("Could not persist external agent confinement profile: {error}"),
                    );
                    return;
                }
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
                            self.status = Some(format!(
                                "External agent runtime started. {}",
                                confinement_profile.summary()
                            ));
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
                            self.status = Some(format!(
                                "External agent runtime is evaluating the captured managed Play frame. {}",
                                confinement_profile.summary()
                            ));
                        }
                    }
                }
            }
            Err(error) => {
                if let Some(profile) = error.confinement_profile().cloned()
                    && let Err(audit_error) =
                        self.host.record_confinement_profile(run_id, profile)
                {
                    self.status = Some(format!(
                        "Could not record rejected confinement profile: {audit_error}"
                    ));
                }
                match purpose {
                    ExternalAgentPurpose::BuildOrRepair => {
                        self.fail_run(run_id, format!("Could not launch external agent: {error}"));
                    }
                    ExternalAgentPurpose::RuntimeEvaluation => {
                        self.record_runtime_evaluation_failure(
                            run_id,
                            format!("Could not launch runtime evaluator: {error}"),
                        );
                    }
                }
            }
        }
    }

    fn release_external_authoring_claim(
        &mut self,
        run_id: &str,
        purpose: ExternalAgentPurpose,
    ) -> Result<(), String> {
        if purpose != ExternalAgentPurpose::BuildOrRepair {
            return Ok(());
        }
        self.host
            .release_work_claims(
                run_id,
                [AgentWorkClaim::shared_resource("canonical_authoring")],
            )
            .map_err(|error| format!("Could not release external agent authoring ownership: {error}"))
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
        let provider_kind = self
            .active_external_provider
            .unwrap_or(ExternalAgentProviderKind::Generic);
        for line in output {
            self.external_provider_diagnostics
                .observe(provider_kind, &line.text);
            if line.stream == ProcessStream::Stdout {
                if let Some(payload) = line.text.strip_prefix(PROVIDER_EVENT_PREFIX) {
                    match serde_json::from_str::<ProviderAgentEvent>(payload) {
                        Ok(event) => {
                            if let Err(error) = self.record_provider_semantic_event(&run_id, event) {
                                self.status = Some(error);
                            }
                            continue;
                        }
                        Err(error) => {
                            self.status = Some(format!(
                                "Provider emitted an invalid semantic AgentEvent: {error}"
                            ));
                        }
                    }
                }
                let translated = translate_provider_line(provider_kind, &line.text);
                if !translated.is_empty() {
                    for event in translated {
                        match event {
                            ExternalAgentSemanticEvent::Progress { step, detail } => {
                                if let Err(error) =
                                    self.host.record_semantic_progress(&run_id, step, detail)
                                {
                                    self.status = Some(error.to_string());
                                }
                            }
                            ExternalAgentSemanticEvent::ToolAction {
                                tool,
                                action,
                                success,
                            } => {
                                if let Err(error) = self
                                    .host
                                    .record_tool_action(&run_id, tool, action, success)
                                {
                                    self.status = Some(error.to_string());
                                }
                            }
                            ExternalAgentSemanticEvent::GameEngineProtocolPayload(payload) => {
                                match serde_json::from_str::<ProviderAgentEvent>(&payload) {
                                    Ok(event) => {
                                        if let Err(error) =
                                            self.record_provider_semantic_event(&run_id, event)
                                        {
                                            self.status = Some(error);
                                        }
                                    }
                                    Err(error) => {
                                        self.status = Some(format!(
                                            "Provider emitted an invalid semantic AgentEvent: {error}"
                                        ));
                                    }
                                }
                            }
                        }
                    }
                    continue;
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
                if let Err(error) = self.release_external_authoring_claim(&run_id, purpose) {
                    self.fail_run(&run_id, error);
                    return;
                }
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
                    let failure = self
                        .external_provider_diagnostics
                        .classify_exit(provider_kind, status.code());
                    let message = if failure.retryable {
                        format!("{} The provider classified this failure as retryable.", failure.message)
                    } else {
                        failure.message
                    };
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
                if let Err(release_error) =
                    self.release_external_authoring_claim(&run_id, purpose)
                {
                    self.fail_run(&run_id, release_error);
                    return;
                }
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
        if self.active_runtime_mode == Some(AgentRuntimeMode::Native) {
            let Some(observation) = self.managed_runtime_observation.clone() else {
                self.record_runtime_evaluation_failure(run_id, "Native runtime evaluation requires a host-captured frame artifact.".to_owned());
                return;
            };
            let image = match fs::read(&observation.path) {
                Ok(image) => image,
                Err(error) => { self.record_runtime_evaluation_failure(run_id, format!("Could not read host-captured frame for native evaluation: {error}")); return; }
            };
            let context = format!("Evaluate the attached host-captured managed Play frame {} ({}x{}). Resolve visual_evaluation with host-reportable evidence; interaction success must be consistent with the observed frame and the immutable playtest plan.", observation.artifact_id, observation.width, observation.height);
            if let Err(error) = self.start_native_agent_turn(run_id, Some(context), vec![image]) { self.record_runtime_evaluation_failure(run_id, error); }
        } else {
            self.request_permission(run_id.to_owned(), AgentCapability::ExternalAgentProcess, PendingPermissionAction::LaunchRuntimeEvaluation);
        }
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
                if self.active_runtime_mode == Some(AgentRuntimeMode::Native) {
                    let context = self.host.run(&run_id).map(managed_source_repair_context).unwrap_or_else(|error| format!("Re-inspect after validation failure: {error}"));
                    if let Err(error) = self.start_native_agent_turn(&run_id, Some(context), Vec::new()) { self.fail_run(&run_id, error); }
                } else {
                    self.request_permission(run_id, AgentCapability::ExternalAgentProcess, PendingPermissionAction::LaunchExternalAgent);
                }
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
                if self.active_runtime_mode == Some(AgentRuntimeMode::Native) {
                    let context = self.host.run(&run_id).map(|run| managed_repair_context(run, self.managed_runtime_observation.as_ref())).unwrap_or_else(|error| format!("Re-inspect after runtime failure: {error}"));
                    if let Err(error) = self.start_native_agent_turn(&run_id, Some(context), Vec::new()) { self.fail_run(&run_id, error); }
                } else {
                    self.request_permission(run_id, AgentCapability::ExternalAgentProcess, PendingPermissionAction::LaunchExternalAgent);
                }
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
        if self.managed_playtest_requested
            || self.pending_runtime_action.is_some()
            || self.model_resource_task.is_some()
        {
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
        let capabilities = self
            .selected_local_resource_config()
            .map(|config| config.capability_profile().resource_capabilities)
            .unwrap_or_default();
        self.resolved_workload = InferenceWorkload::RuntimeObservation;
        self.resource_plan = managed_play_resource_plan(self.quality_preference, capabilities);
        self.begin_model_residency_request(
            self.resource_plan.model_residency,
            ModelResourceContinuation::LaunchManagedPlay { run_id },
        );
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
        let claims = self
            .pending_code_changes
            .iter()
            .map(|change| {
                AgentWorkClaim::code_path(
                    change.relative_path.to_string_lossy().replace('\\', "/"),
                )
            })
            .collect::<Vec<_>>();
        if let Err(error) = self.host.acquire_work_claims(run_id, claims) {
            self.status = Some(format!(
                "Managed code apply is waiting for work ownership: {error}"
            ));
            return;
        }
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

fn external_agent_provider_prompt(
    proposal_json: &str,
    repair_context: Option<&str>,
    runtime_evaluation_context: Option<&str>,
) -> String {
    let mut prompt = format!(
        "Act as a GameEngine external AgentRuntime for the immutable proposal below.\n\n{proposal_json}\n\nPersisted project authoring changes must use the injected gameengine_editor MCP server. Project code changes must stay inside the current isolated Agent Code Workspace. Do not commit, push, or alter Git history. Agent Host permissions, work claims, managed validation, Play/frame evidence, and completion gates remain authoritative. When reporting GameEngine semantic progress, emit a standalone line beginning GAMEENGINE_AGENT_EVENT followed by the supported JSON event payload. Never emit credentials, bearer tokens, or MCP authorization material."
    );
    if let Some(context) = repair_context {
        prompt.push_str("\n\nRepair context:\n");
        prompt.push_str(context);
    }
    if let Some(context) = runtime_evaluation_context {
        prompt.push_str("\n\nRuntime evaluation context:\n");
        prompt.push_str(context);
    }
    prompt
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

fn telemetry_bool_label(value: &TelemetryValue<bool>) -> &'static str {
    match value {
        TelemetryValue::Measured(true) => "measured yes",
        TelemetryValue::Measured(false) => "measured no",
        TelemetryValue::ConservativeEstimate(true) => "estimated yes",
        TelemetryValue::ConservativeEstimate(false) => "estimated no",
        TelemetryValue::Unavailable => "unavailable",
    }
}

fn telemetry_bytes_label(value: &TelemetryValue<u64>) -> String {
    match value {
        TelemetryValue::Measured(bytes) => format!("measured {bytes} B"),
        TelemetryValue::ConservativeEstimate(bytes) => format!("estimated {bytes} B"),
        TelemetryValue::Unavailable => "unavailable".to_owned(),
    }
}

fn telemetry_count_label(value: &TelemetryValue<u64>, unit: &str) -> String {
    match value {
        TelemetryValue::Measured(count) => format!("measured {count} {unit}"),
        TelemetryValue::ConservativeEstimate(count) => format!("estimated {count} {unit}"),
        TelemetryValue::Unavailable => "unavailable".to_owned(),
    }
}

fn telemetry_u64_value(value: &TelemetryValue<u64>) -> String {
    match value {
        TelemetryValue::Measured(value) => value.to_string(),
        TelemetryValue::ConservativeEstimate(value) => format!("~{value}"),
        TelemetryValue::Unavailable => "unavailable".to_owned(),
    }
}

fn interrupt_model_residency_request() -> ModelResidencyRequest {
    ModelResidencyRequest::ReleaseIfSupported
}

fn managed_play_resource_plan(
    quality_preference: QualityPreference,
    capabilities: crate::resource_arbitration::ModelResourceCapabilities,
) -> ResourcePlan {
    resolve_resource_plan(
        InferenceWorkload::RuntimeObservation,
        quality_preference,
        MemoryPressure::Unknown,
        capabilities,
    )
}

fn resume_model_resource_operation_after_authoritative_inspection(
    capabilities: crate::resource_arbitration::ModelResourceCapabilities,
) -> Option<ModelResourceOperation> {
    (capabilities.unload_reload == CapabilityAvailability::Available)
        .then_some(ModelResourceOperation::Reload)
}

fn model_resource_continuation_runtime_action(
    continuation: &ModelResourceContinuation,
) -> Option<AiStudioRuntimeAction> {
    matches!(continuation, ModelResourceContinuation::RestoreForEditing)
        .then_some(AiStudioRuntimeAction::RestoreEditorPresentation)
}

fn optional_text(value: Option<&str>) -> &str {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("n/a")
}

fn format_model_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.2} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{} B", bytes as u64)
    }
}

fn list_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "n/a".to_owned()
    } else {
        values.join(", ")
    }
}

fn model_capability_summary(profile: &ModelCapabilityProfile) -> String {
    format!(
        "Backend: {} · Model: {} · structured: {} · tools: {} · images: {} · reasoning: {} · context: {} · streaming: {} · usage: {} · benchmark: {}",
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
        capability_flag(profile.streaming),
        capability_flag(profile.usage),
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

    #[test]
    fn hosted_preferences_exclude_sensitive_auth_state() {
        let preferences = AiStudioPreferences {
            schema_version: AI_STUDIO_PREFERENCES_SCHEMA_VERSION,
            quality_preference: QualityPreference::Balanced,
            confinement_requirement: AgentConfinementRequirement::default(),
            external_agent_provider: ExternalAgentProviderKind::ClaudeCode,
            model_backend: ModelBackendPreference::HostedApi,
            local_model_endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            local_model_name: String::new(),
            hosted_model_endpoint: "https://provider.example/v1/chat/completions".to_owned(),
            hosted_model_name: "example-model".to_owned(),
        };
        let json = serde_json::to_string(&preferences).expect("serialize preferences");
        assert!(!json.contains("authorization"));
        assert!(!json.contains("bearer"));
        assert!(!json.contains("protected_path"));
    }

    #[test]
    fn hosted_model_config_requires_network_and_reports_implemented_capabilities() {
        let config = NativeModelConfig::Hosted(HostedModelConfig {
            endpoint: "https://provider.example/v1/chat/completions".to_owned(),
            model: "example-model".to_owned(),
            auth_mode: HostedAuthMode::ApiKey,
            encrypted_secret_path: PathBuf::from("protected-state"),
        });
        assert!(config.requires_network());
        let profile = config.capability_profile();
        assert_eq!(profile.image_input, Some(false));
        assert_eq!(profile.streaming, Some(false));
        assert_eq!(profile.usage, Some(true));
        assert_eq!(profile.resource_capabilities, Default::default());
    }

    #[test]
    fn detached_presentation_close_and_reopen_preserves_placement() {
        let mut presentation = AiStudioPresentationState::default();
        presentation.detach();
        presentation.close();
        presentation.open();
        assert_eq!(presentation.mode, AiStudioPresentationMode::Detached);
        assert!(presentation.open);
        presentation.reattach();
        assert_eq!(presentation.mode, AiStudioPresentationMode::Embedded);
        assert!(presentation.open);
    }

    fn local_resource_capabilities() -> crate::resource_arbitration::ModelResourceCapabilities {
        LocalModelConfig {
            endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            model: "model:tag".to_owned(),
        }
        .capability_profile()
        .resource_capabilities
    }

    #[test]
    fn interrupt_resource_boundary_releases_supported_residency_before_editor_restore() {
        let capabilities = local_resource_capabilities();
        assert_eq!(
            resource_operation_for_residency_request(
                interrupt_model_residency_request(),
                capabilities,
            ),
            Some(ModelResourceOperation::Release)
        );
        assert!(matches!(
            model_resource_continuation_runtime_action(
                &ModelResourceContinuation::RestoreForEditing
            ),
            Some(AiStudioRuntimeAction::RestoreEditorPresentation)
        ));
    }

    #[test]
    fn managed_play_resource_boundary_prioritizes_renderer_and_verified_release() {
        let capabilities = local_resource_capabilities();
        let plan = managed_play_resource_plan(QualityPreference::Deep, capabilities);
        assert_eq!(
            plan.priority,
            crate::resource_arbitration::ResourcePriority::RuntimeRendering
        );
        assert_eq!(plan.presentation, PresentationPosture::Interactive);
        assert_eq!(plan.reclaim, ReclaimLevel::None);
        assert_eq!(
            plan.model_residency,
            ModelResidencyRequest::ReleaseIfSupported
        );
        assert_eq!(
            resource_operation_for_residency_request(plan.model_residency, capabilities),
            Some(ModelResourceOperation::Release)
        );
    }

    #[test]
    fn resume_reacquires_only_on_post_inspection_path_and_resource_planning_is_authoring_free() {
        let authoritative_state = AiStudioAuthoritativeState {
            document_revision: 17,
            game_code_generation: 23,
            document_path: Some(PathBuf::from("assets/scenes/test.scene.json")),
            document_dirty: true,
        };
        let unchanged = authoritative_state.clone();
        let capabilities = local_resource_capabilities();
        let interrupt_request = interrupt_model_residency_request();
        let play_plan = managed_play_resource_plan(QualityPreference::Balanced, capabilities);
        let reload =
            resume_model_resource_operation_after_authoritative_inspection(capabilities);
        assert_eq!(interrupt_request, ModelResidencyRequest::ReleaseIfSupported);
        assert_eq!(
            play_plan.priority,
            crate::resource_arbitration::ResourcePriority::RuntimeRendering
        );
        assert_eq!(reload, Some(ModelResourceOperation::Reload));
        assert_eq!(
            resume_model_resource_operation_after_authoritative_inspection(Default::default()),
            None
        );
        let continuation = ModelResourceContinuation::ResumeAfterEditing {
            run_id: Some("run-test".to_owned()),
            state: authoritative_state.clone(),
        };
        assert!(model_resource_continuation_runtime_action(&continuation).is_none());
        assert_eq!(authoritative_state, unchanged);
    }
}
