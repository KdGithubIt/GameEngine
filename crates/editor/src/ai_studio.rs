//! Conversation-first AI Studio frontend.
//!
//! This module owns only presentation and direct user interaction. Agent
//! lifecycle, permissions, persistence, provider process management, and code
//! workspace rules live in the GUI-free `agent_host` module.

mod benchmark_campaign_ui;
mod benchmark_child;
mod benchmark_experiment_ui;
mod settings_ui;

use crate::agent_benchmark::{
    AgentRunBenchmarkIdentity, BENCHMARK_CORPUS_VERSION, BENCHMARK_TASKS,
    BenchmarkHardwareIdentity, BenchmarkRecord, BenchmarkStore, BenchmarkTaskKind, CatalogProfile,
    CuratedModelCatalog, agent_run_record, benchmark_task, read_question_record,
};
use crate::agent_host::{
    AgentCapability, AgentConfinementNetworkPolicy, AgentConfinementRequest,
    AgentConfinementRequirement, AgentEventKind, AgentHost, AgentHostError, AgentProposal,
    AgentRunState, AgentWorkClaim, ApprovalScope, AuthoritativeStateSnapshot, CodeChange,
    CodeWorkspace, CompletionStatus, ConversationRole, ExternalAgentProcess,
    ManagedValidationAttemptStatus, ModelExchangeRecord, PermissionCheck, ProcessStream,
    ResumeDisposition, project_storage_key,
};
use crate::ai_studio_theme as theme;
use crate::external_agent_provider::{
    ExternalAgentDiagnostics, ExternalAgentExecutionEnvironment, ExternalAgentExecutionPlacement,
    ExternalAgentProviderKind, ExternalAgentProviderStatus, ExternalAgentSemanticEvent,
    build_launch_plan, probe_provider, probe_wsl_loopback_reachability, translate_provider_line,
    wsl_environment_forwarding,
};
use crate::hosted_model_backend;
use crate::hosted_model_backend::{HostedAuthMode, HostedModelConfig};
use crate::live_observation::{LiveObservationError, LiveObservationManager};
use crate::managed_local_runtime::{
    GgufModelCapability, MANAGED_BACKEND_ID, ManagedEnvironmentProbe, ManagedEnvironmentProbeTask,
    ManagedExecutionEnvironment, ManagedLocalModelConfig, ManagedLocalRuntime,
    ManagedSetupOperation, ManagedSetupResult, ManagedSetupStatus, ManagedSetupTask,
    PINNED_LLAMA_CPP_REVISION, PINNED_LLAMA_CPP_TAG,
};
use crate::model_router::{MODEL_ROUTER_POLICY_VERSION, ModelRoutingPolicy};
use crate::native_agent::{
    DEFAULT_LOCAL_MODEL_ENDPOINT, InstalledLocalModel, InstalledModelDiscoveryTask,
    InstalledModelInventory, LocalModelConfig, LocalModelResourceConfig, ModelCapabilityProfile,
    ModelResourceTask, NativeAnswer, NativeMetrics, NativeModelConfig, NativeQuestionTask,
    QuestionMessage, QuestionRole,
};
use crate::native_agent_runtime::{
    NativeAgentAction, NativeAgentRuntime, NativeMcpTask, mcp_write,
};
use crate::remote_ai_studio::{
    RemoteAiStudioRequest, RemoteAiStudioResponse, RemoteAiStudioServer, RemoteOperation,
    RemotePermissionScope, events_json, frame_bytes, sessions_json, snapshot_json,
};
use crate::resource_arbitration::{
    CapabilityAvailability, InferenceWorkload, MemoryPressure, ModelResidencyRequest,
    ModelResourceOperation, ModelResourceTelemetry, PresentationPosture, QualityPreference,
    ReclaimLevel, ResourcePlan, TelemetryValue, WorkloadSignals, classify_workload,
    resolve_resource_plan, resource_operation_for_residency_request,
};
use crate::runtime_debug::{
    RuntimeDebugExecutionReport, RuntimeDebugObservation, RuntimeDebugPlan, RuntimeDebugPredicate,
    RuntimeDebugScheduledInput, RuntimeDebugWaitResult,
};
use eframe::egui;
use engine::{GamepadAxis, GamepadButton, GamepadId, InputCommand, KeyCode, MouseButton};
use engine_authoring::ProjectRoot;
use serde::{Deserialize, Serialize};
use settings_ui::SettingsSection;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

const PROVIDER_EVENT_PREFIX: &str = "GAMEENGINE_AGENT_EVENT ";
const MAX_AUTONOMOUS_SOURCE_REPAIRS: usize = 2;
const MAX_AUTONOMOUS_RUNTIME_REPAIRS: usize = 2;
const AI_STUDIO_PREFERENCES_SCHEMA_VERSION: u32 = 1;
/// How long a managed-environment snapshot is reused before a worker re-probes it.
const MANAGED_PROBE_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum ModelBackendPreference {
    #[default]
    Local,
    ManagedLocal,
    HostedApi,
    Enterprise,
}

impl ModelBackendPreference {
    /// Every backend the studio can select between.
    const ALL: [Self; 4] = [
        Self::Local,
        Self::ManagedLocal,
        Self::HostedApi,
        Self::Enterprise,
    ];

    /// Short label used by selection controls.
    const fn label(self) -> &'static str {
        match self {
            Self::Local => "External local (Ollama-compatible)",
            Self::ManagedLocal => "Managed Local AI",
            Self::HostedApi => "Hosted API",
            Self::Enterprise => "Enterprise",
        }
    }
}

/// What submitting a message does.
///
/// ADR 0162 §1 replaces the separate Go control with a mode carried by the
/// composer: the mode is the explicit act, and submission performs it. Write
/// capability follows the displayed mode and nothing else, so a message can
/// never silently acquire it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum ConversationMode {
    /// Read-only. The agent may inspect the project and answer.
    ///
    /// This is the default so an installation that predates ADR 0162 never
    /// gains write-on-send without the user selecting it once.
    #[default]
    Ask,
    /// Write-capable. Submission commits the intent and starts a run.
    Build,
}

impl ConversationMode {
    /// Every mode, in the order the composer lists them.
    const ALL: [Self; 2] = [Self::Ask, Self::Build];

    /// Returns the label the composer shows.
    const fn label(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Build => "Build",
        }
    }

    /// Returns what submitting a message does in this mode.
    const fn description(self) -> &'static str {
        match self {
            Self::Ask => "Read-only. The agent inspects the project and answers; it never writes.",
            Self::Build => {
                "Write-capable. Sending commits your message as the proposal and starts a run."
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AiStudioPreferences {
    schema_version: u32,
    #[serde(default)]
    conversation_mode: ConversationMode,
    #[serde(default)]
    quality_preference: QualityPreference,
    #[serde(default)]
    confinement_requirement: AgentConfinementRequirement,
    #[serde(default)]
    external_agent_provider: ExternalAgentProviderKind,
    #[serde(default)]
    external_agent_execution_environment: ExternalAgentExecutionEnvironment,
    #[serde(default)]
    external_agent_wsl_distribution: String,
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
    #[serde(default)]
    presentation_mode: AiStudioPresentationMode,
}

impl Default for AiStudioPreferences {
    fn default() -> Self {
        Self {
            schema_version: AI_STUDIO_PREFERENCES_SCHEMA_VERSION,
            conversation_mode: ConversationMode::Ask,
            quality_preference: QualityPreference::Auto,
            confinement_requirement: AgentConfinementRequirement::default(),
            external_agent_provider: ExternalAgentProviderKind::default(),
            external_agent_execution_environment: ExternalAgentExecutionEnvironment::default(),
            external_agent_wsl_distribution: String::new(),
            model_backend: ModelBackendPreference::Local,
            managed_execution_environment: ManagedExecutionEnvironment::WindowsNative,
            managed_model_id: String::new(),
            local_model_endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            local_model_name: String::new(),
            hosted_model_endpoint: String::new(),
            hosted_model_name: String::new(),
            presentation_mode: AiStudioPresentationMode::default(),
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
    LaunchManagedPlay {
        run_id: String,
    },
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
    Progress {
        step: String,
        detail: String,
    },
    ToolAction {
        tool: String,
        action: String,
        success: Option<bool>,
    },
    CompletionGate {
        gate: String,
        status: CompletionStatus,
        message: String,
    },
    PlaytestResult {
        launched: bool,
        interactions_passed: Option<bool>,
        message: String,
    },
    RuntimeInput {
        input: ProviderRuntimeInput,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ProviderRuntimeInput {
    Key {
        key: String,
        pressed: bool,
        #[serde(default)]
        at_tick: Option<u64>,
    },
    HoldKey {
        key: String,
        ticks: u64,
        #[serde(default)]
        at_tick: Option<u64>,
    },
    MouseButton {
        button: String,
        pressed: bool,
        #[serde(default)]
        at_tick: Option<u64>,
    },
    HoldMouseButton {
        button: String,
        ticks: u64,
        #[serde(default)]
        at_tick: Option<u64>,
    },
    GamepadButton {
        gamepad: u32,
        button: String,
        pressed: bool,
        #[serde(default)]
        at_tick: Option<u64>,
    },
    GamepadAxis {
        gamepad: u32,
        axis: String,
        value: f32,
        #[serde(default)]
        at_tick: Option<u64>,
    },
    MouseMove {
        x: f32,
        y: f32,
        #[serde(default)]
        at_tick: Option<u64>,
    },
    MouseDelta {
        x: f64,
        y: f64,
        #[serde(default)]
        at_tick: Option<u64>,
    },
    MouseScroll {
        amount: f32,
        #[serde(default)]
        at_tick: Option<u64>,
    },
}

impl ProviderRuntimeInput {
    fn scheduled_commands(
        &self,
        default_tick: u64,
    ) -> Result<Vec<RuntimeDebugScheduledInput>, String> {
        let schedule = |tick: u64, command: InputCommand| {
            RuntimeDebugScheduledInput::at_tick(tick, command).map_err(|error| error.to_string())
        };
        match self {
            Self::Key {
                key,
                pressed,
                at_tick,
            } => Ok(vec![schedule(
                at_tick.unwrap_or(default_tick),
                InputCommand::Key {
                    key: provider_key_code(key)?,
                    pressed: *pressed,
                },
            )?]),
            Self::HoldKey {
                key,
                ticks,
                at_tick,
            } => {
                let start = at_tick.unwrap_or(default_tick);
                let end = runtime_hold_end_tick(start, *ticks)?;
                let key = provider_key_code(key)?;
                Ok(vec![
                    schedule(start, InputCommand::Key { key, pressed: true })?,
                    schedule(
                        end,
                        InputCommand::Key {
                            key,
                            pressed: false,
                        },
                    )?,
                ])
            }
            Self::MouseButton {
                button,
                pressed,
                at_tick,
            } => Ok(vec![schedule(
                at_tick.unwrap_or(default_tick),
                InputCommand::MouseButton {
                    button: provider_mouse_button(button)?,
                    pressed: *pressed,
                },
            )?]),
            Self::HoldMouseButton {
                button,
                ticks,
                at_tick,
            } => {
                let start = at_tick.unwrap_or(default_tick);
                let end = runtime_hold_end_tick(start, *ticks)?;
                let button = provider_mouse_button(button)?;
                Ok(vec![
                    schedule(
                        start,
                        InputCommand::MouseButton {
                            button,
                            pressed: true,
                        },
                    )?,
                    schedule(
                        end,
                        InputCommand::MouseButton {
                            button,
                            pressed: false,
                        },
                    )?,
                ])
            }
            Self::GamepadButton {
                gamepad,
                button,
                pressed,
                at_tick,
            } => Ok(vec![schedule(
                at_tick.unwrap_or(default_tick),
                InputCommand::GamepadButton {
                    gamepad: GamepadId(*gamepad),
                    button: provider_gamepad_button(button)?,
                    pressed: *pressed,
                },
            )?]),
            Self::GamepadAxis {
                gamepad,
                axis,
                value,
                at_tick,
            } if value.is_finite() => Ok(vec![schedule(
                at_tick.unwrap_or(default_tick),
                InputCommand::GamepadAxis {
                    gamepad: GamepadId(*gamepad),
                    axis: provider_gamepad_axis(axis)?,
                    value: *value,
                },
            )?]),
            Self::GamepadAxis { .. } => {
                Err("runtime gamepad axis values must be finite".to_owned())
            }
            Self::MouseMove { x, y, at_tick } if x.is_finite() && y.is_finite() => {
                Ok(vec![schedule(
                    at_tick.unwrap_or(default_tick),
                    InputCommand::MouseMove { position: (*x, *y) },
                )?])
            }
            Self::MouseDelta { x, y, at_tick } if x.is_finite() && y.is_finite() => {
                Ok(vec![schedule(
                    at_tick.unwrap_or(default_tick),
                    InputCommand::MouseDelta { delta: (*x, *y) },
                )?])
            }
            Self::MouseScroll { amount, at_tick } if amount.is_finite() => Ok(vec![schedule(
                at_tick.unwrap_or(default_tick),
                InputCommand::MouseScroll { amount: *amount },
            )?]),
            Self::MouseMove { .. } | Self::MouseDelta { .. } | Self::MouseScroll { .. } => {
                Err("runtime input numeric values must be finite".to_owned())
            }
        }
    }
}

fn runtime_hold_end_tick(start: u64, ticks: u64) -> Result<u64, String> {
    if ticks == 0 {
        return Err("runtime hold duration must contain at least one fixed tick".to_owned());
    }
    start
        .checked_add(ticks)
        .ok_or_else(|| "runtime hold tick overflowed u64".to_owned())
}

fn next_runtime_input_tick(inputs: &[RuntimeDebugScheduledInput]) -> u64 {
    inputs
        .iter()
        .map(RuntimeDebugScheduledInput::tick_offset)
        .max()
        .and_then(|tick| tick.checked_add(1))
        .unwrap_or(0)
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
#[derive(Debug, Clone, PartialEq)]
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
    /// Start the normal Editor Play path for the active project. Managed starts are paused immediately.
    StartPlaytest,
    /// Compatibility path for one already-authorized virtual input command.
    SendInput(InputCommand),
    /// Pause normal Play advancement without destroying the runtime world.
    PausePlaytest,
    /// Resume normal Play advancement.
    ResumePlaytest,
    /// Advance a bounded number of fixed simulation ticks while paused.
    StepPlaytest {
        /// Number of fixed ticks to execute.
        steps: u32,
    },
    /// Execute one frozen fixed-tick input plan through `InputSource::AiAgent`.
    RunDebugPlan(RuntimeDebugPlan),
    /// Capture bounded typed runtime state without mutating Play.
    ObserveRuntime,
    /// Advance paused Play until one allowlisted predicate matches or the budget expires.
    WaitRuntime {
        /// Host-owned typed predicate.
        predicate: RuntimeDebugPredicate,
        /// Maximum fixed ticks the host may advance.
        max_ticks: u32,
    },
    /// Evaluate one allowlisted predicate without advancing Play.
    AssertRuntime {
        /// Host-owned typed predicate.
        predicate: RuntimeDebugPredicate,
    },
    /// Re-run the most recent ADR 0064 input artifact through normal replay.
    ReplayLast,
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
    /// Managed Play is paused and exposes the captured structured observation.
    RuntimePaused(RuntimeDebugObservation),
    /// Managed Play resumed from Pause.
    RuntimeResumed(RuntimeDebugObservation),
    /// Bounded fixed-step execution completed.
    RuntimeStepped(RuntimeDebugObservation),
    /// Frozen deterministic input plan completed and was recorded as replay evidence.
    RuntimeDebugPlanCompleted(RuntimeDebugExecutionReport),
    /// Deterministic input execution aborted and the Editor discarded the Play world.
    RuntimeDebugPlanFailed(String),
    /// Bounded typed runtime observation was captured.
    RuntimeObserved(RuntimeDebugObservation),
    /// Bounded host wait completed.
    RuntimeWaited(RuntimeDebugWaitResult),
    /// Host assertion completed.
    RuntimeAsserted(RuntimeDebugWaitResult),
    /// ADR 0064 replay reproduction completed.
    RuntimeReplayCompleted(RuntimeDebugExecutionReport),
    /// The managed Play session stopped.
    PlayStopped,
    /// A Game View frame was captured by the runtime renderer.
    FrameCaptured(crate::FrameCapture),
    /// The requested operation failed without bypassing the normal runtime path.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
enum PendingPermissionAction {
    LaunchExternalAgent,
    StartNativeAgent,
    LaunchRuntimeEvaluation,
    ApplyCodeChanges,
    LaunchPlaytest,
    RunRuntimeDebugPlan(RuntimeDebugPlan),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum AiStudioPresentationMode {
    Embedded,
    /// AI Studio opens in its own OS window unless the user reattaches it,
    /// because the studio is a conversation surface used beside the Editor
    /// rather than one more dock competing with the scene for the same pixels.
    #[default]
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
            mode: AiStudioPresentationMode::default(),
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
    /// What submitting the draft will do (ADR 0162 §1).
    conversation_mode: ConversationMode,
    /// An instruction submitted while a run was already executing.
    ///
    /// ADR 0162 §3 keeps a running snapshot immutable, so the instruction waits
    /// for the next run instead of steering this one.
    deferred_intent: Option<String>,
    preferences_path: PathBuf,
    quality_preference: QualityPreference,
    confinement_requirement: AgentConfinementRequirement,
    external_provider_kind: ExternalAgentProviderKind,
    /// Whether the machine-local settings surface is open (ADR 0158 §5).
    settings_open: bool,
    /// Which section of that surface is being read (ADR 0162 §5).
    settings_section: SettingsSection,
    /// Whether the editable proposal surface is open (ADR 0162 §4).
    ///
    /// The proposal no longer sits between the transcript and the composer:
    /// the snapshot a run started from is read where that run is read, and the
    /// draft that will seed the next run is edited on demand from the header.
    proposal_open: bool,
    external_provider_environment: ExternalAgentExecutionEnvironment,
    external_provider_wsl_distribution: String,
    external_provider_status: ExternalAgentProviderStatus,
    model_backend: ModelBackendPreference,
    managed_local_runtime: ManagedLocalRuntime,
    managed_execution_environment: ManagedExecutionEnvironment,
    managed_model_id: String,
    managed_setup_task: Option<ManagedSetupTask>,
    managed_probe: Option<ManagedEnvironmentProbe>,
    managed_probe_task: Option<ManagedEnvironmentProbeTask>,
    managed_probe_completed_at: Option<std::time::Instant>,
    managed_probe_requested: bool,
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
    #[cfg(feature = "visual-validation")]
    visual_external_provider_evidence: bool,
    active_run_id: Option<String>,
    process: Option<ExternalAgentProcess>,
    process_purpose: Option<ExternalAgentPurpose>,
    external_provider_diagnostics: ExternalAgentDiagnostics,
    pending_external_work_owner: Option<(ExternalAgentPurpose, String)>,
    code_workspace: Option<CodeWorkspace>,
    pending_code_changes: Vec<CodeChange>,
    pending_permission: Option<PendingPermission>,
    pending_runtime_action: Option<AiStudioRuntimeAction>,
    managed_input_recipe: Vec<RuntimeDebugScheduledInput>,
    managed_candidate_input_recipe: Vec<RuntimeDebugScheduledInput>,
    managed_runtime_plan_completed: bool,
    managed_runtime_debug_observation: Option<String>,
    managed_playtest_requested: bool,
    managed_capture_requested: bool,
    managed_repair_requested: bool,
    managed_runtime_repairs: usize,
    managed_runtime_observation: Option<ManagedRuntimeObservation>,
    managed_evaluation_requested: bool,
    native_evaluation_had_image: bool,
    managed_playtest_started_at: Option<std::time::Instant>,
    last_captured_frame: Option<(egui::TextureHandle, String, u32, u32)>,
    benchmark_child: Option<benchmark_child::BenchmarkChildState>,
    benchmark_campaign: benchmark_campaign_ui::BenchmarkCampaignPanel,
    benchmark_experiment: benchmark_experiment_ui::BenchmarkExperimentPanel,
    benchmark_experiment_root: PathBuf,
    status: Option<String>,
}

impl AiStudioPanel {
    /// Opens the project-scoped AI Studio state for an Editor project.
    pub fn new(project: &ProjectRoot, connection: AiStudioConnection) -> Result<Self, String> {
        let ai_root = dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("GameEngine")
            .join("ai");
        let data_root = ai_root.join(project_storage_key(
            project.project_id().as_str(),
            project.path(),
        ));
        let preferences_path = data_root.join("preferences.json");
        let hosted_secret_path = data_root.join("secrets").join("hosted-api-key.dpapi");
        let preferences = load_ai_studio_preferences(&preferences_path);
        let managed_local_runtime = ManagedLocalRuntime::open(ai_root.join("managed-local"))
            .map_err(|error| error.to_string())?;
        let benchmark_store = BenchmarkStore::open(ai_root.join("benchmark"))?;
        let benchmark_experiment_root = ai_root.join("benchmark-experiments");
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
            conversation_mode: preferences.conversation_mode,
            deferred_intent: None,
            preferences_path,
            quality_preference: preferences.quality_preference,
            confinement_requirement: preferences.confinement_requirement,
            external_provider_kind: preferences.external_agent_provider,
            settings_open: false,
            settings_section: SettingsSection::Models,
            proposal_open: false,
            external_provider_environment: preferences.external_agent_execution_environment,
            external_provider_wsl_distribution: preferences.external_agent_wsl_distribution,
            external_provider_status: ExternalAgentProviderStatus::unchecked(
                preferences.external_agent_provider,
            ),
            model_backend: preferences.model_backend,
            managed_local_runtime,
            managed_execution_environment: preferences.managed_execution_environment,
            managed_model_id: preferences.managed_model_id,
            managed_setup_task: None,
            managed_probe: None,
            managed_probe_task: None,
            managed_probe_completed_at: None,
            managed_probe_requested: false,
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
            presentation: AiStudioPresentationState {
                mode: preferences.presentation_mode,
                open: true,
            },
            #[cfg(feature = "visual-validation")]
            detached_visual_frames: 0,
            #[cfg(feature = "visual-validation")]
            #[cfg(feature = "visual-validation")]
            visual_external_provider_evidence: false,
            active_run_id,
            process: None,
            process_purpose: None,
            external_provider_diagnostics: ExternalAgentDiagnostics::default(),
            pending_external_work_owner: None,
            code_workspace: None,
            pending_code_changes: Vec::new(),
            pending_permission: None,
            pending_runtime_action: None,
            managed_input_recipe: Vec::new(),
            managed_candidate_input_recipe: Vec::new(),
            managed_runtime_plan_completed: false,
            managed_runtime_debug_observation: None,
            managed_playtest_requested: false,
            managed_capture_requested: false,
            managed_repair_requested: false,
            managed_runtime_repairs: 0,
            managed_runtime_observation: None,
            managed_evaluation_requested: false,
            native_evaluation_had_image: false,
            managed_playtest_started_at: None,
            last_captured_frame: None,
            benchmark_child: None,
            benchmark_campaign: benchmark_campaign_ui::BenchmarkCampaignPanel::default(),
            benchmark_experiment: benchmark_experiment_ui::BenchmarkExperimentPanel::default(),
            benchmark_experiment_root,
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
        self.visual_external_provider_evidence = false;
    }

    #[cfg(feature = "visual-validation")]
    /// Seeds one session and run so the transcript can be reviewed (ADR 0158).
    ///
    /// An empty studio shows a composer and an empty-state line, which proves
    /// the layout and nothing about the entries. This drives the real Agent
    /// Host so the captured transcript is a projection of recorded host state
    /// rather than a drawn mock.
    pub fn prepare_transcript_visual_validation(&mut self) {
        self.prepare_hosted_backend_visual_validation();
        let Ok(session) = self.host.create_session("Intro cutscene pacing") else {
            self.status = Some("Transcript fixture could not create a session.".to_owned());
            return;
        };
        self.selected_session = session.clone();
        let mut proposal = AgentProposal {
            goal: "Slow the intro cutscene and cut to the balcony camera on the door beat."
                .to_owned(),
            ..AgentProposal::default()
        };
        proposal.planned_project_changes = vec!["assets/cutscenes/intro.timeline.json".to_owned()];
        proposal.acceptance_criteria =
            vec!["The balcony camera is active when the door marker fires.".to_owned()];
        let _ = self.host.update_proposal(&session, proposal.clone());
        self.proposal_draft = proposal;
        let _ = self.host.append_message(
            &session,
            ConversationRole::User,
            "The intro cutscene cuts to the balcony too early. Hold the wide shot until the door marker.",
        );
        let _ = self.host.append_message(
            &session,
            ConversationRole::Assistant,
            "The Camera Cut track changes at 2.0 s and the door marker is at 2.4 s. I can move the cut onto the marker and keep the wide shot until then.",
        );
        let version = self
            .host
            .session(&session)
            .map(|session| session.proposal.version)
            .unwrap_or_default();
        let Ok(run) = self
            .host
            .start_run_authorized(&session, version, "native:managed:local")
        else {
            self.status = Some("Transcript fixture could not start a run.".to_owned());
            return;
        };
        let _ = self.host.transition_run(
            &run,
            AgentRunState::Executing,
            "Build authorized proposal v1.",
        );
        let _ = self.host.record_semantic_progress(
            &run,
            "inspect_timeline",
            "Read the Camera Cut track and the marker lane of intro.timeline.json.",
        );
        let _ = self.host.record_model_exchange(
            &run,
            ModelExchangeRecord {
                turn: 1,
                prompt: "visual fixture prompt",
                response: "visual fixture response",
                prompt_tokens: Some(6_412),
                response_tokens: Some(188),
                finish_reason: "stop",
                response_digest: "fixture-digest",
                response_excerpt:
                    "{\"summary\":\"Move the camera cut onto the door marker\",\"action\":{\"type\":\"mcp_call\"}}",
            },
        );
        let _ = self.host.record_tool_action(
            &run,
            "timeline.apply",
            "rejected: clip overlaps an earlier clip on the same track",
            Some(false),
        );
        let _ = self.host.record_semantic_progress(
            &run,
            "repair",
            "Trim the preceding clip before moving the cut so the track stays non-overlapping.",
        );
        let _ = self
            .host
            .record_tool_action(&run, "timeline.apply", "applied", Some(true));
        self.active_run_id = Some(run);
        self.status =
            Some("Native run executing · proposal v1 · authoring mutation applied.".to_owned());
    }

    #[cfg(feature = "visual-validation")]
    /// Selects deterministic local-resource telemetry for ADR 0143 screenshot validation.
    pub fn prepare_local_model_resources_visual_validation(&mut self) {
        self.model_backend = ModelBackendPreference::Local;
        self.local_model_endpoint = DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned();
        self.local_model_name = "qwen2.5-coder:7b".to_owned();
        self.last_model_resource_telemetry = ModelResourceTelemetry {
            resident: TelemetryValue::Measured(true),
            representation_size_bytes: TelemetryValue::Measured(4_700_000_000),
            gpu_residency_bytes: TelemetryValue::Measured(3_200_000_000),
            context_length_tokens: TelemetryValue::Measured(8_192),
        };
        self.visual_external_provider_evidence = false;
    }

    #[cfg(feature = "visual-validation")]
    /// Selects deterministic external-provider state for ADR 0145 screenshot validation.
    pub fn prepare_external_agent_visual_validation(&mut self) {
        self.prepare_hosted_backend_visual_validation();
        self.external_provider_kind = ExternalAgentProviderKind::ClaudeCode;
        self.external_provider_status =
            ExternalAgentProviderStatus::visual_fixture(ExternalAgentProviderKind::ClaudeCode);
        self.visual_external_provider_evidence = true;
    }

    #[cfg(feature = "visual-validation")]
    /// Seeds deterministic host-owned state for Remote AI Studio browser screenshot validation.
    pub fn prepare_remote_companion_visual_validation(&mut self) -> Result<(), String> {
        let session_id = self.selected_session.clone();
        self.host
            .append_message(
                &session_id,
                ConversationRole::User,
                "Build a compact playable sample and verify the result from the managed Game View.",
            )
            .map_err(|error| error.to_string())?;
        self.host
            .append_message(
                &session_id,
                ConversationRole::Assistant,
                "I will keep the proposal version exact, report progress, and request permission before the next managed frame capture.",
            )
            .map_err(|error| error.to_string())?;

        let mut proposal = self
            .host
            .session(&session_id)
            .map_err(|error| error.to_string())?
            .proposal
            .clone();
        proposal.goal = "Complete the Remote AI Studio visual-validation sample.".to_owned();
        proposal.requirements = vec![
            "Keep the authoritative project inside the Editor Agent Host.".to_owned(),
            "Preserve reconnect-safe progress and exact proposal authorization.".to_owned(),
        ];
        proposal.acceptance_criteria = vec![
            "Build and Stop remain available without exposing raw MCP or process controls."
                .to_owned(),
            "Captured frame review stays readable at responsive browser widths.".to_owned(),
        ];
        proposal.validation_plan = vec![
            "Validate the managed source changes.".to_owned(),
            "Review the captured Game View frame.".to_owned(),
        ];
        proposal.playtest_plan =
            vec!["Launch managed Play and capture one Game View frame.".to_owned()];
        proposal.requested_capabilities = [
            AgentCapability::RuntimeLaunch,
            AgentCapability::FrameCapture,
        ]
        .into_iter()
        .collect();
        let proposal_version = self
            .host
            .update_proposal(&session_id, proposal)
            .map_err(|error| error.to_string())?;
        self.proposal_draft = self
            .host
            .session(&session_id)
            .map_err(|error| error.to_string())?
            .proposal
            .clone();

        let run_id = self
            .host
            .start_run_authorized(&session_id, proposal_version, "visual-validation")
            .map_err(|error| error.to_string())?;
        self.active_run_id = Some(run_id.clone());
        self.host
            .transition_run(
                &run_id,
                AgentRunState::Planning,
                "Authoritative snapshot loaded; planning the managed change.",
            )
            .map_err(|error| error.to_string())?;
        self.host
            .transition_run(
                &run_id,
                AgentRunState::Executing,
                "Executing through the normal Agent Host path.",
            )
            .map_err(|error| error.to_string())?;
        self.host
            .record_semantic_progress(
                &run_id,
                "Reconnect ready",
                "Authoritative snapshot and ordered event cursor are synchronized.",
            )
            .map_err(|error| error.to_string())?;
        self.host
            .transition_run(
                &run_id,
                AgentRunState::Validating,
                "Managed source validation is represented by deterministic visual evidence.",
            )
            .map_err(|error| error.to_string())?;
        self.host
            .transition_run(
                &run_id,
                AgentRunState::Playtesting,
                "Managed Play launched for deterministic frame review.",
            )
            .map_err(|error| error.to_string())?;
        self.host
            .record_playtest_result(
                &run_id,
                true,
                Some(true),
                "Managed Play launched and the scripted interaction completed.",
            )
            .map_err(|error| error.to_string())?;

        let width = 640_u32;
        let height = 360_u32;
        let mut rgba = vec![0_u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let offset = ((y * width + x) * 4) as usize;
                let checker = ((x / 80) + (y / 60)) % 2;
                rgba[offset] = if checker == 0 { 42 } else { 28 };
                rgba[offset + 1] = if checker == 0 { 112 } else { 76 };
                rgba[offset + 2] = if checker == 0 { 176 } else { 126 };
                rgba[offset + 3] = 255;
            }
        }
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
            writer
                .write_image_data(&rgba)
                .map_err(|error| error.to_string())?;
            writer.finish().map_err(|error| error.to_string())?;
        }
        self.host
            .store_captured_frame_artifact(&run_id, width, height, &png_bytes)
            .map_err(|error| error.to_string())?;
        self.host
            .record_completion_gate(
                &run_id,
                "acceptance_criteria",
                CompletionStatus::Passed,
                "Acceptance criteria are represented in the deterministic browser fixture.",
            )
            .map_err(|error| error.to_string())?;
        self.host
            .record_completion_gate(
                &run_id,
                "authoring_validation",
                CompletionStatus::Passed,
                "Authoring validation is represented in the deterministic browser fixture.",
            )
            .map_err(|error| error.to_string())?;
        self.host
            .record_completion_gate(
                &run_id,
                "visual_evaluation",
                CompletionStatus::Passed,
                "Captured Game View frame is ready for visual review.",
            )
            .map_err(|error| error.to_string())?;

        self.pending_permission = Some(PendingPermission {
            run_id,
            capability: AgentCapability::FrameCapture,
            action: PendingPermissionAction::CaptureFrame,
        });
        self.status =
            Some("Remote AI Studio browser visual-validation fixture is ready.".to_owned());
        Ok(())
    }

    #[cfg(feature = "visual-validation")]
    /// Selects a deterministic managed-local setup state for ADR 0155 screenshot validation.
    pub fn prepare_managed_local_visual_validation(&mut self) -> Result<(), String> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let fixture_root = std::env::temp_dir().join(format!(
            "gameengine-managed-local-visual-{}-{unique}",
            std::process::id()
        ));
        let model_path = fixture_root.join("qwen3.8-27b-abliterated-3.69bpw.gguf");
        self.managed_local_runtime =
            ManagedLocalRuntime::open(fixture_root).map_err(|error| error.to_string())?;
        self.model_backend = ModelBackendPreference::ManagedLocal;
        self.managed_execution_environment = ManagedExecutionEnvironment::WindowsNative;
        let model_id = self
            .managed_local_runtime
            .register_visual_validation_model(&model_path)
            .map_err(|error| error.to_string())?
            .model_id;
        self.prepare_managed_campaign_visual_validation(&model_id);
        self.managed_model_id = model_id;
        self.managed_setup_task = None;
        self.managed_probe_task = None;
        self.managed_probe_requested = false;
        self.managed_probe = Some(self.managed_local_runtime.probe_environment(
            self.managed_execution_environment,
            self.managed_model_id.clone(),
        ));
        self.managed_probe_completed_at = Some(std::time::Instant::now());
        self.last_model_resource_telemetry = ModelResourceTelemetry::default();
        self.external_provider_kind = ExternalAgentProviderKind::Generic;
        self.external_provider_status =
            ExternalAgentProviderStatus::unchecked(ExternalAgentProviderKind::Generic);
        self.visual_external_provider_evidence = false;
        Ok(())
    }

    /// Prepares AI Studio presentation state for one named ADR visual scenario.
    ///
    /// Unknown scenario names fall back to the hosted-backend fixture so a
    /// capture never renders an unprepared panel.
    #[cfg(feature = "visual-validation")]
    pub fn prepare_adr_visual_validation(&mut self, scenario: &str) {
        match scenario {
            "adr0144-hosted-backend" => {
                self.prepare_hosted_backend_visual_validation();
                self.status = Some(
                    "Hosted API selected · remote processing · credential stored outside project data · secret value hidden."
                        .to_owned(),
                );
            }
            "adr0144-enterprise-backend" => {
                self.model_backend = ModelBackendPreference::Enterprise;
                self.hosted_model_endpoint =
                    "https://enterprise.example/v1/chat/completions".to_owned();
                self.hosted_model_name = "enterprise-governed-model".to_owned();
                self.hosted_secret_draft.clear();
                self.external_provider_kind = ExternalAgentProviderKind::ClaudeCode;
                self.external_provider_status = ExternalAgentProviderStatus::visual_fixture(
                    ExternalAgentProviderKind::ClaudeCode,
                );
                self.status = Some(
                    "Enterprise backend selected · organization-managed remote processing · credential value hidden."
                        .to_owned(),
                );
            }
            "adr0158-transcript" => {
                self.prepare_transcript_visual_validation();
            }
            "adr0149-live-observation" => {
                self.model_backend = ModelBackendPreference::Local;
                self.status = Some(
                    "Live Game View observation · engine-native readback · PNG transport · 6 FPS cap · latest-frame-only retention · metrics enabled."
                        .to_owned(),
                );
            }
            "adr0153-confinement" => {
                self.confinement_requirement =
                    AgentConfinementRequirement::RequireProviderOrOsConfinement;
                self.external_provider_kind = ExternalAgentProviderKind::ClaudeCode;
                self.external_provider_status = ExternalAgentProviderStatus::visual_fixture(
                    ExternalAgentProviderKind::ClaudeCode,
                );
                self.status = Some(
                    "Confinement required · provider/OS enforcement must be proven before launch · fail closed when unavailable · no OS-sandbox claim is active."
                        .to_owned(),
                );
            }
            _ => self.prepare_hosted_backend_visual_validation(),
        }
    }

    #[cfg(feature = "visual-validation")]
    /// Returns whether the detached native viewport is ready to be captured.
    ///
    /// The transcript scrolls to its newest entry, and egui resolves a scroll
    /// area's content height from the previous frame. Capturing after two
    /// frames photographed the transcript before that scroll settled, so the
    /// newest entries — including the ones ADR 0158 refuses to collapse — were
    /// outside the captured image.
    pub fn detached_visual_validation_capture_ready(&self) -> bool {
        self.detached_visual_frames >= 4
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
                self.status = Some(format!(
                    "Live Game View observation is unavailable: {error}"
                ));
            }
        }
    }

    /// Records the result of an Editor-owned managed runtime action.
    pub fn report_runtime_result(
        &mut self,
        context: &egui::Context,
        result: AiStudioRuntimeResult,
    ) {
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
                    self.status =
                        Some("Restoring Editor presentation before manual editing...".to_owned());
                } else {
                    self.status =
                        Some("Restoring Editor presentation after native inference...".to_owned());
                }
                return;
            }
            AiStudioRuntimeResult::EditorRestored => {
                if self.restore_for_editing {
                    if let (Some(run_id), Some(snapshot)) =
                        (self.active_run_id.clone(), self.interrupt_snapshot.take())
                        && let Err(error) =
                            self.host.interrupt_for_editing(&run_id, snapshot.into())
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
                    self.status =
                        Some("Editor presentation restored after native inference.".to_owned());
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
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
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
                    } else if self.managed_input_recipe.is_empty() {
                        self.managed_runtime_plan_completed = true;
                        self.request_managed_frame_capture_if_ready(&run_id);
                    }
                }
            }
            AiStudioRuntimeResult::PlayStartPending => {
                self.status = Some(
                    "Managed Play is waiting for the engine-managed game-code build.".to_owned(),
                );
            }
            AiStudioRuntimeResult::RuntimeInputQueued(command) => {
                if let Err(error) = self
                    .host
                    .record_managed_runtime_input(&run_id, format!("{command:?}"))
                {
                    self.status = Some(error.to_string());
                } else {
                    self.status =
                        Some("Queued one compatibility AI Agent runtime input.".to_owned());
                }
            }
            AiStudioRuntimeResult::RuntimePaused(observation) => {
                self.managed_runtime_debug_observation = Some(observation.summary());
                self.status = Some(format!(
                    "Managed Play paused at fixed tick {}.",
                    observation.fixed_tick()
                ));
            }
            AiStudioRuntimeResult::RuntimeResumed(observation) => {
                self.managed_runtime_debug_observation = Some(observation.summary());
                self.status = Some(format!(
                    "Managed Play resumed from fixed tick {}.",
                    observation.fixed_tick()
                ));
            }
            AiStudioRuntimeResult::RuntimeStepped(observation)
            | AiStudioRuntimeResult::RuntimeObserved(observation) => {
                let summary = observation.summary();
                self.managed_runtime_debug_observation = Some(summary.clone());
                let _ = self.host.record_semantic_progress(
                    &run_id,
                    "runtime_observation",
                    summary.clone(),
                );
                self.status = Some(summary);
            }
            AiStudioRuntimeResult::RuntimeDebugPlanCompleted(report) => {
                let summary = report.summary();
                self.managed_runtime_plan_completed = true;
                self.managed_runtime_debug_observation = Some(summary.clone());
                if let Err(error) = self
                    .host
                    .record_managed_runtime_input(&run_id, summary.clone())
                    .and_then(|()| {
                        self.host.record_playtest_result(
                            &run_id,
                            true,
                            Some(true),
                            "Host executed the provider-planned interaction recipe on a frozen fixed-tick schedule.",
                        )
                    })
                {
                    self.status = Some(error.to_string());
                } else {
                    self.status = Some(summary);
                    self.request_managed_frame_capture_if_ready(&run_id);
                }
            }
            AiStudioRuntimeResult::RuntimeDebugPlanFailed(message) => {
                self.managed_runtime_plan_completed = true;
                self.managed_playtest_started_at = None;
                if let Err(error) =
                    self.host
                        .record_playtest_result(&run_id, true, Some(false), message.clone())
                {
                    self.status = Some(error.to_string());
                } else {
                    self.status = Some(message);
                }
            }
            AiStudioRuntimeResult::RuntimeWaited(wait) => {
                let summary = wait.summary();
                self.managed_runtime_debug_observation = Some(summary.clone());
                let _ =
                    self.host
                        .record_semantic_progress(&run_id, "runtime_wait", summary.clone());
                self.status = Some(summary);
            }
            AiStudioRuntimeResult::RuntimeAsserted(assertion) => {
                let summary = assertion.summary();
                let passed = assertion.matched() && assertion.unavailable().is_none();
                self.managed_runtime_debug_observation = Some(summary.clone());
                let _ = self.host.record_semantic_progress(
                    &run_id,
                    "runtime_assertion",
                    summary.clone(),
                );
                if let Err(error) = self.host.record_playtest_result(
                    &run_id,
                    true,
                    Some(passed),
                    format!("Host-owned runtime assertion passed={passed}: {summary}"),
                ) {
                    self.status = Some(error.to_string());
                } else {
                    if !passed && self.pending_runtime_action.is_none() {
                        self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
                    }
                    self.status = Some(summary);
                }
            }
            AiStudioRuntimeResult::RuntimeReplayCompleted(report) => {
                let summary = format!(
                    "ADR 0064 replay reproduction completed; {}",
                    report.summary()
                );
                self.managed_runtime_debug_observation = Some(summary.clone());
                let _ =
                    self.host
                        .record_semantic_progress(&run_id, "runtime_replay", summary.clone());
                self.status = Some(summary);
            }
            AiStudioRuntimeResult::PlayStopped => {
                self.managed_playtest_started_at = None;
                self.status = Some("Managed Play stopped.".to_owned());
            }
            AiStudioRuntimeResult::FrameCaptured(capture) => {
                match encode_agent_frame_png(&capture)
                    .map_err(|error| error.to_string())
                    .and_then(|png| {
                        self.host
                            .store_captured_frame_artifact(
                                &run_id,
                                capture.width,
                                capture.height,
                                &png,
                            )
                            .map_err(|error| error.to_string())
                    }) {
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
                        self.last_captured_frame =
                            Some((texture, artifact_id.clone(), capture.width, capture.height));
                        self.managed_runtime_observation = Some(ManagedRuntimeObservation {
                            artifact_id: artifact_id.clone(),
                            path,
                            width: capture.width,
                            height: capture.height,
                        });
                        self.managed_capture_requested = false;
                        self.status = Some(format!(
                            "Captured managed Play frame {artifact_id}; scheduling provider evaluation."
                        ));
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
        self.poll_managed_probe(context);
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
        self.request_managed_runtime_debug_plan_if_ready();
        self.poll_managed_playtest_timeout();
        self.poll_benchmark_child();
        self.poll_benchmark_experiment();
        self.poll_benchmark_campaign();

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
        embedded_window(context)
            .open(&mut open)
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
            self.save_preferences();
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
                // A detached OS window has no Editor chrome behind it, so it
                // paints the studio ground itself.
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0_f32, theme::BACKGROUND);
                ui.horizontal(|ui| {
                    if ui.button("Reattach").clicked() {
                        reattach_requested = true;
                    }
                    ui.small("Same project Agent Host · detached presentation");
                });
                ui.separator();
                self.show_contents(ui);
            },
        );

        #[cfg(feature = "visual-validation")]
        {
            self.detached_visual_frames = self.detached_visual_frames.saturating_add(1);
        }

        if reattach_requested {
            self.presentation.reattach();
            self.save_preferences();
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
                #[cfg(feature = "visual-validation")]
                if let Some(path) = std::env::var_os("GAMEENGINE_REMOTE_AI_STUDIO_VISUAL_URL_TO") {
                    let path = PathBuf::from(path);
                    match self.prepare_remote_companion_visual_validation() {
                        Ok(()) => {
                            if let Err(error) = fs::write(&path, server.companion_url()) {
                                let message = format!(
                                    "Remote AI Studio visual-validation URL could not be published: {error}"
                                );
                                eprintln!(
                                    "[editor.remote_ai_studio_visual_validation_failed] {message}"
                                );
                                self.status = Some(message);
                            }
                        }
                        Err(error) => {
                            let message = format!(
                                "Remote AI Studio visual-validation fixture failed: {error}"
                            );
                            eprintln!(
                                "[editor.remote_ai_studio_visual_validation_failed] {message}"
                            );
                            self.status = Some(message.clone());
                            let _ = fs::write(&path, format!("ERROR: {message}"));
                        }
                    }
                }
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
                let pending = self
                    .pending_permission
                    .as_ref()
                    .map(|permission| (permission.run_id.as_str(), permission.capability));
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
                    Err(error) => {
                        RemoteAiStudioResponse::error(404, "session_not_found", error, false)
                    }
                }
            }
            RemoteOperation::Message {
                session_id, text, ..
            } => {
                match self
                    .host
                    .append_message(&session_id, ConversationRole::User, text)
                {
                    Ok(()) => RemoteAiStudioResponse::json(serde_json::json!({"accepted": true})),
                    Err(error) => RemoteAiStudioResponse::error(
                        404,
                        "message_rejected",
                        error.to_string(),
                        false,
                    ),
                }
            }
            RemoteOperation::Go {
                session_id,
                proposal_version,
                ..
            } => {
                let proposal = match self.host.session(&session_id) {
                    Ok(session) if session.proposal.version == proposal_version => {
                        session.proposal.clone()
                    }
                    Ok(session) => {
                        return RemoteAiStudioResponse::error(
                            409,
                            "stale_proposal",
                            format!(
                                "Proposal version {proposal_version} is stale; current version is {}.",
                                session.proposal.version
                            ),
                            false,
                        );
                    }
                    Err(error) => {
                        return RemoteAiStudioResponse::error(
                            404,
                            "session_not_found",
                            error.to_string(),
                            false,
                        );
                    }
                };
                self.selected_session = session_id;
                self.proposal_draft = proposal;
                match self.begin_run_authorized(proposal_version) {
                    Ok(run_id) => {
                        RemoteAiStudioResponse::json(serde_json::json!({"run_id": run_id}))
                    }
                    Err(error) => RemoteAiStudioResponse::error(409, "go_rejected", error, false),
                }
            }
            RemoteOperation::CommitIntent {
                session_id,
                text,
                proposal_version,
                ..
            } => {
                let proposal = match self.host.session(&session_id) {
                    Ok(session) if session.proposal.version == proposal_version => {
                        session.proposal.clone()
                    }
                    Ok(session) => {
                        return RemoteAiStudioResponse::error(
                            409,
                            "stale_proposal",
                            format!(
                                "Proposal version {proposal_version} is stale; current version is {}.",
                                session.proposal.version
                            ),
                            false,
                        );
                    }
                    Err(error) => {
                        return RemoteAiStudioResponse::error(
                            404,
                            "session_not_found",
                            error.to_string(),
                            false,
                        );
                    }
                };
                if let Err(error) =
                    self.host
                        .append_message(&session_id, ConversationRole::User, text.clone())
                {
                    return RemoteAiStudioResponse::error(
                        404,
                        "message_rejected",
                        error.to_string(),
                        false,
                    );
                }
                self.selected_session = session_id;
                self.proposal_draft = proposal;
                self.derive_intent_proposal(&text);
                let committed = match self
                    .host
                    .update_proposal(&self.selected_session, self.proposal_draft.clone())
                {
                    Ok(version) => {
                        self.proposal_draft.version = version;
                        version
                    }
                    Err(error) => {
                        return RemoteAiStudioResponse::error(
                            409,
                            "intent_rejected",
                            error.to_string(),
                            false,
                        );
                    }
                };
                match self.begin_run_authorized(committed) {
                    Ok(run_id) => RemoteAiStudioResponse::json(
                        serde_json::json!({"run_id": run_id, "proposal_version": committed}),
                    ),
                    Err(error) => {
                        RemoteAiStudioResponse::error(409, "intent_rejected", error, false)
                    }
                }
            }
            RemoteOperation::Stop { run_id, .. } => match self.stop_run_exact(&run_id) {
                Ok(()) => RemoteAiStudioResponse::json(
                    serde_json::json!({"stopped": true, "run_id": run_id}),
                ),
                Err(error) => RemoteAiStudioResponse::error(409, "stop_rejected", error, false),
            },
            RemoteOperation::AwaitingUser { run_id, text, .. } => {
                let state = match self.host.run(&run_id) {
                    Ok(run) => run.state,
                    Err(error) => {
                        return RemoteAiStudioResponse::error(
                            404,
                            "run_not_found",
                            error.to_string(),
                            false,
                        );
                    }
                };
                if state != AgentRunState::AwaitingUser {
                    return RemoteAiStudioResponse::error(
                        409,
                        "not_awaiting_user",
                        "The run is no longer waiting for user input.",
                        false,
                    );
                }
                let session_id = self.host.session_ids().into_iter().find(|session_id| {
                    self.host
                        .session(session_id)
                        .is_ok_and(|session| session.runs.iter().any(|run| run.id == run_id))
                });
                let Some(session_id) = session_id else {
                    return RemoteAiStudioResponse::error(
                        404,
                        "run_not_found",
                        "The run session was not found.",
                        false,
                    );
                };
                if let Err(error) =
                    self.host
                        .append_message(&session_id, ConversationRole::User, text)
                {
                    return RemoteAiStudioResponse::error(
                        409,
                        "response_rejected",
                        error.to_string(),
                        false,
                    );
                }
                match self.host.transition_run(
                    &run_id,
                    AgentRunState::Executing,
                    "User response received; execution may continue.",
                ) {
                    Ok(()) => RemoteAiStudioResponse::json(
                        serde_json::json!({"accepted": true, "run_id": run_id}),
                    ),
                    Err(error) => RemoteAiStudioResponse::error(
                        409,
                        "response_rejected",
                        error.to_string(),
                        false,
                    ),
                }
            }
            RemoteOperation::Permission {
                run_id,
                capability,
                scope,
                ..
            } => {
                let Some(pending) = self.pending_permission.as_ref() else {
                    return RemoteAiStudioResponse::error(
                        409,
                        "permission_not_pending",
                        "No permission decision is pending.",
                        false,
                    );
                };
                if pending.run_id != run_id || pending.capability != capability {
                    return RemoteAiStudioResponse::error(
                        409,
                        "permission_stale",
                        "The permission request no longer matches the active decision.",
                        false,
                    );
                }
                let action = pending.action.clone();
                let approval = match scope {
                    RemotePermissionScope::Once => ApprovalScope::Once,
                    RemotePermissionScope::Run => ApprovalScope::Run,
                    RemotePermissionScope::Project => ApprovalScope::Project,
                    RemotePermissionScope::Deny => ApprovalScope::Deny,
                };
                self.resolve_pending_permission(&run_id, capability, action, approval);
                RemoteAiStudioResponse::json(
                    serde_json::json!({"resolved": true, "run_id": run_id}),
                )
            }
            RemoteOperation::Events { run_id, after } => {
                match events_json(&self.host, &run_id, after) {
                    Ok(events) => RemoteAiStudioResponse::sse(events),
                    Err(error) => RemoteAiStudioResponse::error(404, "run_not_found", error, false),
                }
            }
            RemoteOperation::Frame {
                run_id,
                artifact_id,
            } => match frame_bytes(&self.host, &run_id, &artifact_id) {
                Ok(bytes) => RemoteAiStudioResponse::png(bytes),
                Err(error) => RemoteAiStudioResponse::error(404, "frame_not_found", error, false),
            },
            RemoteOperation::StartLiveObservation {
                run_id, max_fps, ..
            } => {
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
            return Err(
                "The requested run is not the active run; stale Stop was rejected.".to_owned(),
            );
        }
        if self.host.run(run_id).is_ok_and(|run| {
            matches!(
                run.state,
                AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
            )
        }) {
            return Ok(());
        }
        if let Some(process) = self.process.as_mut() {
            process
                .cancel()
                .map_err(|error| format!("Could not stop agent process: {error}"))?;
        }
        self.process = None;
        self.process_purpose = None;
        self.pending_external_work_owner = None;
        self.host
            .cancel_run(run_id)
            .map_err(|error| error.to_string())
    }

    fn show_contents(&mut self, ui: &mut egui::Ui) {
        // Scoped to this Ui and its children, so the surrounding Editor chrome
        // keeps the style installed by `crate::ui::chrome`.
        theme::apply_studio_style(ui);
        self.show_studio_header(ui);
        // ADR 0158 §1: one transcript is the primary surface, with the composer
        // pinned to its lower edge. ADR 0162 §4 narrows what may share that
        // dock to the decisions that block the user, one run status line, and
        // the composer, so the transcript keeps the height ADR 0158 intended.
        egui::Panel::bottom("ai_studio_composer_dock")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                self.show_pinned_affordances(ui);
                self.show_run_status_strip(ui);
                self.show_composer(ui);
                if let Some(status) = self.status.clone() {
                    ui.horizontal_top(|ui| {
                        theme::status_dot(ui, theme::ACCENT_TEXT);
                        theme::selectable_text(
                            ui,
                            egui::RichText::new(status).small().color(theme::TEXT_MUTED),
                        );
                    });
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| self.show_transcript(ui));
        self.show_settings_surface(ui.ctx());
        self.show_proposal_surface(ui.ctx());
    }

    /// Draws the session row and the entry point to the settings surface.
    fn show_studio_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            self.show_session_header(ui);
            // ADR 0162 §4: the proposal is edited on demand rather than being
            // stacked above the composer on every frame.
            if ui.button("Proposal").clicked() {
                self.proposal_open = !self.proposal_open;
            }
            if ui.button("Settings").clicked() {
                self.settings_open = !self.settings_open;
            }
        });
        ui.separator();
    }

    /// Draws the projected transcript for the selected session.
    fn show_transcript(&mut self, ui: &mut egui::Ui) {
        let transcript = self
            .host
            .session(&self.selected_session)
            .map(crate::agent_transcript::project_session)
            .unwrap_or_default();
        let mut requested_navigation = None;
        let transcript_scroll = egui::ScrollArea::vertical()
            .id_salt("ai_studio_transcript")
            .auto_shrink([false, false])
            .stick_to_bottom(true);
        transcript_scroll.show(ui, |ui| {
            if transcript.entries.is_empty() {
                ui.weak("Describe what you want to build, change, inspect, or validate.");
                return;
            }
            let mut current_run: Option<String> = None;
            let mut steps: Vec<&crate::agent_transcript::TranscriptEntry> = Vec::new();
            for entry in &transcript.entries {
                if entry.run_id != current_run {
                    flush_internal_steps(ui, &mut steps, &mut requested_navigation);
                    if let Some(previous) = current_run.as_deref() {
                        self.show_run_span_footer(ui, previous);
                    }
                    current_run.clone_from(&entry.run_id);
                    if let Some(run_id) = entry.run_id.as_deref()
                        && let Some(span) =
                            transcript.runs.iter().find(|span| span.run_id == run_id)
                    {
                        self.show_run_span_header(ui, span);
                    }
                }
                if is_internal_step(entry) {
                    steps.push(entry);
                    continue;
                }
                flush_internal_steps(ui, &mut steps, &mut requested_navigation);
                if let Some(navigation) = show_transcript_entry(ui, entry) {
                    requested_navigation = Some((entry.run_id.clone(), navigation));
                }
            }
            flush_internal_steps(ui, &mut steps, &mut requested_navigation);
            if let Some(last) = current_run.as_deref() {
                self.show_run_span_footer(ui, last);
            }
        });
        if let Some((run_id, navigation)) = requested_navigation {
            self.open_transcript_navigation(run_id.as_deref(), navigation);
        }
    }

    /// Draws the head of a run span: what the run is, and the immutable
    /// proposal snapshot it was started from.
    ///
    /// ADR 0162 §2 requires the snapshot to be readable where the run is read.
    /// It is presented read-only here because a run's input never changes after
    /// Go; the editable draft lives on the proposal surface.
    fn show_run_span_header(
        &mut self,
        ui: &mut egui::Ui,
        span: &crate::agent_transcript::TranscriptRunSpan,
    ) {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            theme::status_dot(ui, theme::ACCENT_TEXT);
            ui.strong(format!("Run · {}", span.proposal_summary));
            ui.small(format!("{:?}", span.state));
        });
        let Ok(snapshot) = self
            .host
            .run(&span.run_id)
            .map(|run| run.proposal_snapshot.clone())
        else {
            return;
        };
        egui::CollapsingHeader::new(format!("Proposal snapshot · v{}", snapshot.version))
            .id_salt(("ai_studio_run_proposal", span.run_id.clone()))
            .default_open(false)
            .show(ui, |ui| show_proposal_snapshot(ui, &snapshot));
    }

    /// Draws the foot of a run span: what the run did and did not perform.
    ///
    /// ADR 0131 §13 requires completion to report unperformed checks, so the
    /// contract is expanded whenever a criterion is unperformed or failed even
    /// though it now scrolls with the run it belongs to (ADR 0162 §4).
    fn show_run_span_footer(&mut self, ui: &mut egui::Ui, run_id: &str) {
        let Ok(completion) = self.host.run(run_id).map(|run| run.completion.clone()) else {
            return;
        };
        // ADR 0162 §5: a code change set is run output, not configuration, so
        // it is reviewed and applied inside the run that produced it.
        if self.active_run_id.as_deref() == Some(run_id) {
            self.show_code_changes(ui);
        }
        let unresolved = completion_statuses(&completion)
            .iter()
            .any(|status| matches!(status, CompletionStatus::Pending | CompletionStatus::Failed));
        let run_id = run_id.to_owned();
        egui::CollapsingHeader::new("Completion contract")
            .id_salt(("ai_studio_run_completion", run_id.clone()))
            .default_open(unresolved)
            .show(ui, |ui| {
                self.show_completion_contract(ui, &run_id, completion);
            });
    }

    /// Opens the Editor context one transcript entry refers to.
    ///
    /// Navigation only ever reveals host-owned artifacts; it never re-derives
    /// state or mutates the run it came from.
    fn open_transcript_navigation(
        &mut self,
        run_id: Option<&str>,
        navigation: crate::agent_transcript::TranscriptNavigation,
    ) {
        use crate::agent_transcript::TranscriptNavigation;
        let Some(run_id) = run_id else {
            return;
        };
        let target = match navigation {
            TranscriptNavigation::CapturedFrame { artifact_id } => {
                self.host.captured_frame_artifact_path(run_id, &artifact_id)
            }
            TranscriptNavigation::CodeWorkspace => match self.host.workspace_paths(run_id) {
                Ok((workspace, _)) => workspace,
                Err(error) => {
                    self.status = Some(error.to_string());
                    return;
                }
            },
        };
        if let Err(error) = open::that(&target) {
            self.status = Some(format!(
                "Could not open {} for review: {error}",
                target.display()
            ));
        }
    }

    /// Draws the decisions that block the user, and nothing else.
    ///
    /// ADR 0162 §4 limits this dock to a pending permission request and a
    /// pending question. The proposal, the completion contract, and the rest of
    /// a run's content are read inside the run span in the transcript, where
    /// ADR 0158 §3 already places run content.
    fn show_pinned_affordances(&mut self, ui: &mut egui::Ui) {
        self.show_permission_prompt(ui);
        if let Some(question) = self.pending_agent_question() {
            theme::attention_card(ui, theme::ACCENT, |ui| {
                ui.strong("The agent is waiting on you");
                theme::selectable_text(ui, question);
                ui.small("Answer in the composer below.");
            });
        }
        self.show_deferred_intent(ui);
    }

    /// Draws the instruction that arrived while a run was already executing.
    ///
    /// ADR 0162 §3: the user is told which run their message applies to and is
    /// offered the choice to stop that run and commit the new instruction
    /// instead. Deferral is what happens if they do nothing.
    fn show_deferred_intent(&mut self, ui: &mut egui::Ui) {
        let Some(intent) = self.deferred_intent.clone() else {
            return;
        };
        let run_active = self.run_is_active();
        let mut replace_run = false;
        let mut build_now = false;
        let mut discard = false;
        theme::attention_card(ui, theme::WARNING, |ui| {
            ui.strong("Recorded for the next run");
            theme::selectable_text(ui, intent.clone());
            ui.horizontal_wrapped(|ui| {
                if run_active {
                    if ui.button("Stop the run and build this").clicked() {
                        replace_run = true;
                    }
                } else if ui.button("Build this now").clicked() {
                    build_now = true;
                }
                if ui.button("Keep it as conversation only").clicked() {
                    discard = true;
                }
            });
        });
        if replace_run {
            self.stop_active_run();
        }
        if replace_run || build_now {
            self.commit_intent(&intent);
        } else if discard {
            self.deferred_intent = None;
        }
    }

    /// Question the active run is waiting on, when it is waiting on one.
    fn pending_agent_question(&self) -> Option<String> {
        let run_id = self.active_run_id.as_deref()?;
        let run = self.host.run(run_id).ok()?;
        if run.state != AgentRunState::AwaitingUser {
            return None;
        }
        run.events
            .iter()
            .rev()
            .find(|event| event.kind == AgentEventKind::StateChanged)
            .map(|event| event.message.clone())
    }

    /// Draws the message composer and its compact backend indicator.
    fn show_composer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            self.show_mode_selection(ui);
            self.show_model_selection(ui);
            self.show_effort_selection(ui);
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.message_draft)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Ask a question, add a constraint, or continue the same conversation…"),
        );
        self.show_send_controls(ui);
    }

    /// Draws the mode entry of the composer's selection tier.
    ///
    /// ADR 0162 §1: the mode is displayed on the control that submits it, and
    /// write capability follows the displayed mode, so what Send will do is
    /// visible before it is pressed and cannot differ from what is shown.
    fn show_mode_selection(&mut self, ui: &mut egui::Ui) {
        let previous = self.conversation_mode;
        egui::ComboBox::from_id_salt("ai_studio_composer_mode")
            .selected_text(self.conversation_mode.label())
            .width(92.0)
            .show_ui(ui, |ui| {
                for mode in ConversationMode::ALL {
                    ui.selectable_value(&mut self.conversation_mode, mode, mode.label())
                        .on_hover_text(mode.description());
                }
            });
        if self.conversation_mode != previous {
            self.save_preferences();
        }
    }

    /// Draws the model entry of the composer's selection tier.
    ///
    /// ADR 0131 §1 requires the provider and its connection state to stay
    /// visible, and ADR 0162 §5 limits this control to choosing among entries
    /// that are already registered: nothing here installs, registers,
    /// authenticates, or removes anything. The one action that leaves the tier
    /// opens the configuration surface at the section that does.
    fn show_model_selection(&mut self, ui: &mut egui::Ui) {
        let selected_label = match self.described_native_model_config() {
            Ok(config) => config.label(),
            Err(_) => format!("{} · not ready", self.model_backend.label()),
        };
        let managed_models = self
            .managed_local_runtime
            .registered_models()
            .unwrap_or_default();
        let mut open_models_configuration = false;
        egui::ComboBox::from_id_salt("ai_studio_composer_model")
            .selected_text(selected_label)
            .width(300.0)
            .show_ui(ui, |ui| {
                theme::caption(ui, "Managed Local AI");
                if managed_models.is_empty() {
                    theme::hint(ui, "No GGUF is registered on this machine yet.");
                }
                for model in &managed_models {
                    let selected = self.model_backend == ModelBackendPreference::ManagedLocal
                        && self.managed_model_id == model.model_id;
                    if ui.selectable_label(selected, &model.display_name).clicked() && !selected {
                        self.model_backend = ModelBackendPreference::ManagedLocal;
                        self.managed_model_id = model.model_id.clone();
                        self.last_model_resource_telemetry = ModelResourceTelemetry::default();
                        self.save_preferences();
                    }
                }
                ui.separator();
                theme::caption(ui, "Other backends");
                // Managed Local AI is listed above as the models it has
                // registered, so it is not repeated as a backend here.
                for backend in ModelBackendPreference::ALL
                    .into_iter()
                    .filter(|backend| *backend != ModelBackendPreference::ManagedLocal)
                {
                    let selected = self.model_backend == backend;
                    let response = ui.selectable_label(
                        selected,
                        format!("{} · {}", backend.label(), self.backend_readiness(backend)),
                    );
                    if response.clicked() && !selected {
                        self.model_backend = backend;
                        self.save_preferences();
                    }
                }
                ui.separator();
                if ui.button("Configure models…").clicked() {
                    open_models_configuration = true;
                }
            });
        if open_models_configuration {
            self.settings_section = SettingsSection::Models;
            self.settings_open = true;
        }
    }

    /// Describes whether a backend can be used as it currently stands.
    ///
    /// This reads configured values only. It never contacts a backend and never
    /// writes, so listing an entry costs nothing and cannot change what the
    /// entry is.
    fn backend_readiness(&self, backend: ModelBackendPreference) -> &'static str {
        match backend {
            ModelBackendPreference::ManagedLocal => {
                if self.managed_model_id.trim().is_empty() {
                    "no GGUF selected"
                } else {
                    "registered"
                }
            }
            ModelBackendPreference::Local => {
                if self.local_model_name.trim().is_empty() {
                    "no model set"
                } else {
                    "configured"
                }
            }
            ModelBackendPreference::HostedApi => {
                if !self.hosted_model_endpoint.trim().starts_with("https://")
                    || self.hosted_model_name.trim().is_empty()
                {
                    "not configured"
                } else if hosted_model_backend::credential_is_configured(&self.hosted_secret_path) {
                    "signed in"
                } else {
                    "no credential"
                }
            }
            ModelBackendPreference::Enterprise => {
                if !self.hosted_model_endpoint.trim().starts_with("https://")
                    || self.hosted_model_name.trim().is_empty()
                {
                    "not configured"
                } else {
                    "configured"
                }
            }
        }
    }

    /// Draws the effort entry of the composer's selection tier.
    ///
    /// ADR 0150 makes this a machine-local latency/reasoning preference, and
    /// ADR 0162 §5 puts it beside the model because it is chosen as often.
    fn show_effort_selection(&mut self, ui: &mut egui::Ui) {
        let previous = self.quality_preference;
        egui::ComboBox::from_id_salt("ai_studio_composer_effort")
            .selected_text(format!("Effort · {}", self.quality_preference.label()))
            .show_ui(ui, |ui| {
                for quality in QualityPreference::ALL {
                    ui.selectable_value(&mut self.quality_preference, quality, quality.label());
                }
            });
        if self.quality_preference != previous {
            self.save_preferences();
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

    /// Draws the Send control for the composer.
    ///
    /// ADR 0162 §1: Send performs what the displayed mode says it performs.
    /// In Ask it answers from read-only evidence, in Build it commits the
    /// instruction and starts a run, and in either mode it answers a run that
    /// is waiting on the user.
    fn show_send_controls(&mut self, ui: &mut egui::Ui) {
        let awaiting_native = self.native_run_awaits_user();
        let run_active = self.run_is_active();
        let commit_blocked = match self.conversation_mode {
            ConversationMode::Ask => None,
            ConversationMode::Build if awaiting_native || run_active => None,
            ConversationMode::Build => self.intent_commit_blocked(),
        };
        let can_send = !self.message_draft.trim().is_empty()
            && self.native_question.is_none()
            && self.pending_question_permission.is_none()
            && commit_blocked.is_none();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_send, egui::Button::new("Send"))
                .clicked()
            {
                self.submit_message();
            }
            if self.native_question.is_some() {
                ui.spinner();
                ui.small("Reading current GameEngine/project evidence…");
                return;
            }
            if awaiting_native {
                ui.small("Your answer continues the run that is waiting on you.");
                return;
            }
            if let Some(reason) = commit_blocked {
                ui.small(reason);
                return;
            }
            match self.conversation_mode {
                ConversationMode::Ask => {
                    ui.small(ConversationMode::Ask.description());
                }
                ConversationMode::Build if run_active => {
                    ui.small(
                        "A run is executing; sending records the instruction for the next run.",
                    );
                }
                ConversationMode::Build => {
                    ui.small(ConversationMode::Build.description());
                }
            }
        });
    }

    /// Whether the active run is a native run waiting on the user.
    fn native_run_awaits_user(&self) -> bool {
        self.active_runtime_mode == Some(AgentRuntimeMode::Native)
            && self.active_run_id.as_ref().is_some_and(|run_id| {
                self.host
                    .run(run_id)
                    .is_ok_and(|run| run.state == AgentRunState::AwaitingUser)
            })
    }

    /// Whether a run is under way and has not reached a terminal state.
    fn run_is_active(&self) -> bool {
        self.active_run_id.as_ref().is_some_and(|run_id| {
            self.host.run(run_id).is_ok_and(|run| {
                !matches!(
                    run.state,
                    AgentRunState::Completed | AgentRunState::Failed | AgentRunState::Cancelled
                )
            })
        })
    }

    /// Returns why an intent cannot be committed right now, if it cannot be.
    ///
    /// This is the predicate the removed Go control carried (ADR 0162 §1): a
    /// run starts only when no other execution owns the studio and a usable
    /// runtime has been selected.
    fn intent_commit_blocked(&mut self) -> Option<&'static str> {
        if self.process.is_some() || self.native_runtime_busy() {
            return Some("An agent process is already running.");
        }
        if self.pending_permission.is_some() || self.pending_question_permission.is_some() {
            return Some("Resolve the pending approval first.");
        }
        if self.external_provider_is_ready() {
            return None;
        }
        if self.external_provider_is_requested() {
            return Some("The selected external agent provider is not ready.");
        }
        if self.described_native_model_config().is_err() {
            return Some("Select a configured model before building.");
        }
        None
    }

    /// Performs what the composer's mode says submitting a message does.
    ///
    /// ADR 0162 §1: submission is the affirmative action, and what it does
    /// follows the displayed mode. Answering a run that is waiting on the user
    /// is not a mode decision and continues that run in either mode.
    fn submit_message(&mut self) {
        let text = self.message_draft.trim().to_owned();
        if text.is_empty() {
            return;
        }
        if let Err(error) =
            self.host
                .append_message(&self.selected_session, ConversationRole::User, text.clone())
        {
            self.status = Some(error.to_string());
            return;
        }
        self.message_draft.clear();
        if self.native_run_awaits_user() {
            self.continue_awaiting_run();
            return;
        }
        match self.conversation_mode {
            ConversationMode::Ask => self.start_native_question(),
            ConversationMode::Build if self.run_is_active() => {
                // ADR 0162 §3: the running snapshot stays immutable, so the
                // instruction is carried to the next run and the user is told
                // so at the moment they submit it.
                self.deferred_intent = Some(text);
                self.status = Some(
                    "A run is already executing. This instruction was recorded for the next run."
                        .to_owned(),
                );
            }
            ConversationMode::Build => self.commit_intent(&text),
        }
    }

    /// Answers the question the active native run is waiting on.
    fn continue_awaiting_run(&mut self) {
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        match self.host.transition_run(
            &run_id,
            AgentRunState::Executing,
            "User response received; native execution may continue.",
        ) {
            Ok(()) => {
                if let Err(error) = self.start_native_agent_turn(
                    &run_id,
                    Some(
                        "User answered the pending product question. Re-read current conversation and continue without expanding the immutable proposal."
                            .to_owned(),
                    ),
                    Vec::new(),
                ) {
                    self.fail_run(&run_id, error);
                }
            }
            Err(error) => self.status = Some(error.to_string()),
        }
    }

    /// Commits a submitted instruction as the next proposal version and starts
    /// a run from exactly that version.
    ///
    /// ADR 0162 §2: the message becomes the proposal's goal and every other
    /// field of the current draft is carried forward, so the recorded artifacts
    /// are the ones Go produced — one new version in the session's proposal
    /// history, and one run whose immutable input is that version.
    fn commit_intent(&mut self, message: &str) {
        self.deferred_intent = None;
        self.derive_intent_proposal(message);
        self.begin_run();
    }

    /// Applies the derivation an intent commit performs on the draft proposal.
    ///
    /// The submitted instruction becomes the goal and every other field of the
    /// current draft is carried forward. Both the composer and the ADR 0133
    /// companion commit through this, so the two surfaces record the same
    /// proposal version for the same instruction (ADR 0162 §7).
    fn derive_intent_proposal(&mut self, message: &str) {
        self.proposal_draft.goal = message.to_owned();
    }

    fn model_routing_status(&mut self) -> String {
        let Ok(primary) = self.described_native_model_config() else {
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
        self.installed_model_inventory
            .as_ref()
            .filter(|inventory| inventory.endpoint.trim().trim_end_matches('/') == endpoint)
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
            Err(error) => {
                self.status = Some(format!(
                    "Managed Local AI setup failed to start ({}): {error}",
                    error.layer().label()
                ))
            }
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
        self.managed_probe_completed_at = None;
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

    /// Refreshes the managed-environment snapshot on a worker thread.
    ///
    /// The Local AI panel renders `managed_probe` and never probes inline. A
    /// WSL2 frame would otherwise block on three `wsl.exe` launches, and while a
    /// managed model transfer keeps the dedicated distribution busy each of
    /// those takes seconds, which stops the frame loop outright.
    fn poll_managed_probe(&mut self, context: &egui::Context) {
        let panel_visible = std::mem::take(&mut self.managed_probe_requested);
        if let Some(task) = self.managed_probe_task.as_ref() {
            let Some(outcome) = task.poll() else {
                context.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            };
            self.managed_probe_task = None;
            self.managed_probe_completed_at = Some(std::time::Instant::now());
            if let Some(probe) = outcome {
                self.managed_probe = Some(probe);
            }
            context.request_repaint();
            return;
        }
        if !panel_visible || self.managed_setup_task.is_some() || !self.managed_probe_is_stale() {
            return;
        }
        match ManagedEnvironmentProbeTask::spawn(
            self.managed_local_runtime.clone(),
            self.managed_execution_environment,
            self.managed_model_id.clone(),
        ) {
            Ok(task) => {
                self.managed_probe_task = Some(task);
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Err(error) => {
                self.managed_probe_completed_at = Some(std::time::Instant::now());
                self.status = Some(format!(
                    "Managed Local AI environment probe failed to start ({}): {error}",
                    error.layer().label()
                ));
            }
        }
    }

    fn managed_probe_is_stale(&self) -> bool {
        let Some(probe) = self.managed_probe.as_ref() else {
            return true;
        };
        if probe.environment != self.managed_execution_environment
            || probe.model_id != self.managed_model_id
        {
            return true;
        }
        self.managed_probe_completed_at
            .is_none_or(|completed_at| completed_at.elapsed() >= MANAGED_PROBE_REFRESH_INTERVAL)
    }

    /// Returns the snapshot the Local AI panel may render and asks the frame
    /// loop to keep it refreshed while the panel stays visible.
    fn managed_probe_for_panel(&mut self) -> Option<ManagedEnvironmentProbe> {
        self.managed_probe_requested = true;
        self.managed_probe
            .as_ref()
            .filter(|probe| {
                probe.environment == self.managed_execution_environment
                    && probe.model_id == self.managed_model_id
            })
            .cloned()
    }

    /// Resolves the frozen managed identity the UI may hand to an inference worker.
    ///
    /// Answers come from the cached environment probe, so neither drawing a frame nor
    /// pressing Send hashes the model on the UI thread. The inference worker performs
    /// ADR 0155 integrity verification against this frozen identity before endpoint
    /// startup.
    fn described_managed_model_config(&mut self) -> Result<ManagedLocalModelConfig, String> {
        if self.managed_model_id.trim().is_empty() {
            return Err(
                "Register or select a managed GGUF model before starting inference.".to_owned(),
            );
        }
        match self.managed_probe_for_panel() {
            Some(probe) => probe.described_config,
            None => Err("Checking the managed Local AI environment...".to_owned()),
        }
    }

    /// Presentation counterpart of [`Self::selected_native_model_config`].
    fn described_native_model_config(&mut self) -> Result<NativeModelConfig, String> {
        match self.model_backend {
            ModelBackendPreference::ManagedLocal => self
                .described_managed_model_config()
                .map(|config| NativeModelConfig::Managed(Box::new(config))),
            _ => self.selected_native_model_config(),
        }
    }

    fn managed_benchmark_inventory(
        &self,
        config: &ManagedLocalModelConfig,
    ) -> InstalledModelInventory {
        InstalledModelInventory {
            endpoint: format!("managed://{}", config.environment.benchmark_id()),
            backend_version: Some(config.benchmark_runtime_identity()),
            models: vec![InstalledLocalModel {
                name: config.model_id.clone(),
                digest: Some(config.model_content_sha256.clone()),
                size_bytes: Some(config.model_size_bytes),
                parameter_size: None,
                quantization_level: config
                    .model_representation
                    .clone()
                    .or_else(|| config.quantization.clone()),
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

    fn selected_native_model_config(&mut self) -> Result<NativeModelConfig, String> {
        match self.model_backend {
            ModelBackendPreference::Local => {
                if self.local_model_name.trim().is_empty() {
                    return Err(
                        "Set an installed external local model before starting inference."
                            .to_owned(),
                    );
                }
                Ok(NativeModelConfig::Local(LocalModelConfig {
                    endpoint: self.local_model_endpoint.clone(),
                    model: self.local_model_name.clone(),
                }))
            }
            ModelBackendPreference::ManagedLocal => self
                .described_managed_model_config()
                .map(|config| NativeModelConfig::Managed(Box::new(config))),
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
                    return Err(
                        "Store a hosted API credential before starting hosted inference."
                            .to_owned(),
                    );
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
                    self.status =
                        Some("Hosted inference requires Network access approval.".to_owned());
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
            self.status =
                Some("Preparing InferenceFocused presentation before native inference.".to_owned());
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

    fn selected_local_resource_config(&mut self) -> Option<LocalModelResourceConfig> {
        match self.model_backend {
            ModelBackendPreference::Local if !self.local_model_name.trim().is_empty() => {
                Some(LocalModelResourceConfig::Ollama(LocalModelConfig {
                    endpoint: self.local_model_endpoint.clone(),
                    model: self.local_model_name.clone(),
                }))
            }
            ModelBackendPreference::ManagedLocal => self
                .described_managed_model_config()
                .ok()
                .map(|config| LocalModelResourceConfig::Managed(Box::new(config))),
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
        let config = self.selected_local_resource_config();
        self.begin_model_residency_request_with_config(config, request, continuation);
    }

    fn begin_model_residency_request_with_config(
        &mut self,
        config: Option<LocalModelResourceConfig>,
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
        let Some(config) = config else {
            self.finish_model_resource_continuation(continuation);
            return;
        };
        let capabilities = config.capability_profile().resource_capabilities;
        let Some(operation) = resource_operation_for_residency_request(request, capabilities)
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
            conversation_mode: self.conversation_mode,
            quality_preference: self.quality_preference,
            confinement_requirement: self.confinement_requirement,
            external_agent_provider: self.external_provider_kind,
            external_agent_execution_environment: self.external_provider_environment,
            external_agent_wsl_distribution: self.external_provider_wsl_distribution.clone(),
            model_backend: self.model_backend,
            managed_execution_environment: self.managed_execution_environment,
            managed_model_id: self.managed_model_id.clone(),
            local_model_endpoint: self.local_model_endpoint.clone(),
            local_model_name: self.local_model_name.clone(),
            hosted_model_endpoint: self.hosted_model_endpoint.clone(),
            hosted_model_name: self.hosted_model_name.clone(),
            presentation_mode: self.presentation.mode,
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
                self.status = Some(format!(
                    "Could not serialize AI Studio preferences: {error}"
                ));
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
                match self
                    .host
                    .append_message(&session_id, ConversationRole::Assistant, message)
                {
                    Ok(()) => {
                        self.status = Some(format!(
                            "Model backend {} answered with {} retrieved evidence source(s) in {} ms.",
                            answer.metrics.backend_id,
                            answer.sources.len(),
                            answer.metrics.elapsed_ms
                        ));
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            Err(error) => {
                let diagnostic = format!("Inference failed: {error}");
                self.status = Some(diagnostic.clone());
                if let Err(append_error) =
                    self.host
                        .append_message(&session_id, ConversationRole::System, diagnostic)
                {
                    self.status = Some(format!(
                        "Inference failed: {error}; Conversation diagnostic could not be recorded: {append_error}"
                    ));
                }
            }
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

    /// Draws the editable proposal that will seed the next run.
    ///
    /// ADR 0162 §4 keeps this off the dock: the draft is inspected and edited
    /// on demand, while the snapshot a run already started from is read in that
    /// run's span. Editing here never affects a run that is already executing.
    fn show_proposal_surface(&mut self, context: &egui::Context) {
        if !self.proposal_open {
            return;
        }
        let current_version = self
            .host
            .session(&self.selected_session)
            .map(|session| session.proposal.version)
            .unwrap_or(self.proposal_draft.version);
        let mut open = self.proposal_open;
        egui::Window::new(format!("Structured proposal · v{current_version}"))
            .id(egui::Id::new("ai_studio_proposal"))
            .open(&mut open)
            .default_width(520.0)
            .default_height(560.0)
            .resizable(true)
            .show(context, |ui| {
                theme::apply_studio_style(ui);
                egui::ScrollArea::vertical()
                    .id_salt("ai_studio_proposal_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        theme::hint(
                            ui,
                            "Editing the draft never changes a run that has already started.",
                        );
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
                        edit_lines(
                            ui,
                            "Planned assets",
                            &mut self.proposal_draft.planned_assets,
                        );
                        edit_lines(
                            ui,
                            "Validation plan",
                            &mut self.proposal_draft.validation_plan,
                        );
                        edit_lines(ui, "Playtest plan", &mut self.proposal_draft.playtest_plan);
                        if ui.button("Save proposal version").clicked() {
                            match self.host.update_proposal(
                                &self.selected_session,
                                self.proposal_draft.clone(),
                            ) {
                                Ok(version) => {
                                    self.proposal_draft.version = version;
                                    self.status =
                                        Some(format!("Saved proposal version {version}."));
                                }
                                Err(error) => self.status = Some(error.to_string()),
                            }
                        }
                    });
            });
        self.proposal_open = open;
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
        let workspace =
            CodeWorkspace::open_or_create(&self.project_root, workspace_root, baseline_path)
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
        let run = self
            .host
            .run(run_id)
            .map_err(|error| error.to_string())?
            .clone();
        let benchmark_single_model = self.benchmark_child_active();
        let (backend_label, routing_summary, routing_decisions) = {
            let runtime = self
                .native_agent_runtime
                .as_mut()
                .ok_or_else(|| "Native AgentRuntime is not initialized.".to_owned())?;
            if benchmark_single_model {
                runtime
                    .start_turn_single_model(&run, context.as_deref(), images)
                    .map_err(|error| error.to_string())?;
            } else {
                runtime
                    .start_turn(&run, context.as_deref(), images)
                    .map_err(|error| error.to_string())?;
            }
            (
                runtime.backend_label(),
                runtime.routing_policy_summary(),
                runtime.take_routing_decisions(),
            )
        };
        if routing_decisions
            .iter()
            .any(|decision| decision.context_handoff)
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
            backend_label, run.state, routing_summary
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
        let outcome = match self.native_agent_runtime.as_mut() {
            Some(runtime) => match runtime.poll() {
                Some(outcome) => outcome,
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
        // ADR 0159: the exchange is recorded before the turn is interpreted, so
        // a run that fails on this turn still carries what the model returned.
        if let Some(exchange) = outcome.exchange.as_ref() {
            let excerpt = exchange.response_excerpt();
            let _ = self.host.record_model_exchange(
                &run_id,
                ModelExchangeRecord {
                    turn: exchange.turn,
                    prompt: &exchange.prompt,
                    response: &exchange.response,
                    prompt_tokens: exchange.prompt_tokens,
                    response_tokens: exchange.response_tokens,
                    finish_reason: &exchange.finish_reason,
                    response_digest: &exchange.response_digest,
                    response_excerpt: &excerpt,
                },
            );
        }
        let turn = match outcome.result {
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
            let _ =
                self.host
                    .record_tool_action(&run_id, "native.policy", error.clone(), Some(false));
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
                    let message = format!("MCP mutation is waiting for work ownership: {error}");
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
                    let message =
                        format!("Managed code write is waiting for work ownership: {error}");
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
                let default_tick = next_runtime_input_tick(&self.managed_candidate_input_recipe);
                let result = serde_json::from_value::<ProviderRuntimeInput>(input)
                    .map_err(|error| error.to_string())
                    .and_then(|input| input.scheduled_commands(default_tick));
                match result {
                    Ok(scheduled) => {
                        let summary = scheduled
                            .iter()
                            .map(|input| {
                                format!("tick {} {:?}", input.tick_offset(), input.command())
                            })
                            .collect::<Vec<_>>()
                            .join(", ");
                        self.managed_candidate_input_recipe
                            .extend(scheduled.iter().cloned());
                        let _ = self.host.record_semantic_progress(
                            run_id,
                            "runtime_input_plan",
                            format!("Native runtime planned fixed-tick input: {summary}."),
                        );
                        self.record_native_result_and_continue(
                            run_id,
                            "runtime_input",
                            true,
                            "Input was added to the frozen host-managed Play recipe.",
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
                    if status == CompletionStatus::Passed && !self.native_evaluation_had_image {
                        self.record_runtime_evaluation_failure(
                            run_id,
                            "Native model attempted to pass visual_evaluation without verified image input; false visual claims are rejected.".to_owned(),
                        );
                        return;
                    }
                    match result {
                        Ok(()) => {
                            let playtest_result = match status {
                                CompletionStatus::Passed => self.host.record_playtest_result(
                                    run_id,
                                    true,
                                    Some(true),
                                    "Native visual evaluation completed against the host-captured managed Play frame.",
                                ),
                                CompletionStatus::Failed => self.host.record_playtest_result(
                                    run_id,
                                    true,
                                    Some(false),
                                    "Native visual evaluation failed against the host-captured managed Play frame.",
                                ),
                                CompletionStatus::Pending | CompletionStatus::NotApplicable => Ok(()),
                            };
                            if let Err(error) = playtest_result {
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
                    result
                        .map(|()| "Progress recorded.".to_owned())
                        .unwrap_or_else(|e| e),
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
                self.record_native_result_and_continue(&run_id, tool, true, value.to_string());
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

    /// Where the selected external provider process is placed on this machine.
    fn external_agent_placement(&self) -> ExternalAgentExecutionPlacement {
        ExternalAgentExecutionPlacement {
            environment: self.external_provider_environment,
            distribution: self.external_provider_wsl_distribution.clone(),
        }
    }

    fn refresh_external_provider_status(&mut self) {
        self.external_provider_status = probe_provider(
            self.external_provider_kind,
            &self.provider_program,
            &self.external_agent_placement(),
        );
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

    /// Draws the one-line run status strip.
    ///
    /// ADR 0162 §4: an active run is represented outside the transcript by its
    /// state and the actions that apply to it, on one line, and the strip is
    /// absent when no run is active. Stop therefore stays reachable without
    /// scrolling for as long as a run can be stopped.
    fn show_run_status_strip(&mut self, ui: &mut egui::Ui) {
        let mut stop_requested = false;
        let mut interrupt_requested = false;
        let mut resume_requested = false;
        let active_run = self.active_run_id.clone().and_then(|run_id| {
            self.host.run(&run_id).ok().map(|run| {
                (
                    run.state,
                    run.proposal_snapshot.version,
                    run.provider_label.clone(),
                )
            })
        });
        ui.horizontal_wrapped(|ui| {
            if let Some((state, proposal_version, provider_label)) = active_run.as_ref() {
                theme::status_dot(ui, theme::ACCENT_TEXT);
                ui.strong(format!("{state:?}"));
                ui.label(
                    egui::RichText::new(format!("proposal v{proposal_version} · {provider_label}"))
                        .small()
                        .color(theme::TEXT_MUTED),
                );
            }
            let can_stop = self.run_is_active();
            if can_stop && ui.button("Stop").clicked() {
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
            if can_interrupt && ui.button("Interrupt for Editing").clicked() {
                interrupt_requested = true;
            }
            if self.editing_interrupted
                && self.pending_runtime_action.is_none()
                && ui.button("Resume").clicked()
            {
                resume_requested = true;
            }
        });
        if stop_requested {
            self.stop_active_run();
        }
        if interrupt_requested {
            if let Some(task) = self.native_question.as_ref() {
                task.interrupt();
            }
            if let Some(runtime) = self.native_agent_runtime.as_mut() {
                runtime.interrupt();
            }
            if let Some(task) = self.native_mcp_task.as_ref() {
                task.interrupt();
            }
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
            self.status =
                Some("Re-inspecting authoritative Editor state before Resume...".to_owned());
        }
    }

    /// Stops whatever the active run is doing and cancels it.
    ///
    /// Stop is reached from the run status strip and from the decision that
    /// offers to replace the active run with a newer instruction (ADR 0162
    /// §3), so the sequence lives here rather than inside either control.
    fn stop_active_run(&mut self) {
        let run_id = self.active_run_id.clone();
        if let Some(task) = self.native_question.as_ref() {
            task.interrupt();
        }
        self.pending_native_question_start = None;
        self.model_resource_continuation = None;
        self.restore_for_editing = false;
        if let Some(runtime) = self.native_agent_runtime.as_mut() {
            runtime.interrupt();
        }
        if let Some(task) = self.native_mcp_task.as_ref() {
            task.interrupt();
        }
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
        let action = pending.action.clone();
        ui.separator();
        ui.group(|ui| {
            ui.strong("Approval required");
            ui.label(capability.label());
            ui.small(
                "Project-level approval persists as user application state; credentials never do.",
            );
            ui.horizontal(|ui| {
                for (label, scope) in [
                    ("Allow once", ApprovalScope::Once),
                    ("This run", ApprovalScope::Run),
                    ("This project", ApprovalScope::Project),
                    ("Deny", ApprovalScope::Deny),
                ] {
                    if ui.button(label).clicked() {
                        self.resolve_pending_permission(&run_id, capability, action.clone(), scope);
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
                    theme::selectable_text(
                        ui,
                        egui::RichText::new(change.relative_path.display().to_string()).monospace(),
                    );
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

    fn show_completion_contract(
        &mut self,
        ui: &mut egui::Ui,
        run_id: &str,
        report: crate::agent_host::CompletionReport,
    ) {
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
            if ui
                .add_enabled(can_capture, egui::Button::new("Capture managed frame"))
                .clicked()
            {
                self.managed_capture_requested = true;
                self.request_permission(
                    run_id.to_owned(),
                    AgentCapability::FrameCapture,
                    PendingPermissionAction::CaptureFrame,
                );
            }
            if ui
                .add_enabled(
                    self.managed_playtest_started_at.is_some(),
                    egui::Button::new("Stop managed Play"),
                )
                .clicked()
            {
                self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
            }
        });
        if let Some((texture, artifact_id, width, height)) = &self.last_captured_frame {
            ui.group(|ui| {
                ui.strong(format!("Captured frame · {artifact_id} · {width}x{height}"));
                ui.add(
                    egui::Image::new(texture)
                        .fit_to_exact_size(egui::vec2(480.0, 270.0))
                        .maintain_aspect_ratio(true),
                );
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
        let run_id = self
            .host
            .start_run_authorized(
                &self.selected_session,
                authorized_proposal_version,
                provider_label,
            )
            .map_err(|error| error.to_string())?;
        self.active_run_id = Some(run_id.clone());
        self.active_runtime_mode = Some(mode);
        self.active_external_provider = external_provider;
        self.active_external_program = external_provider.map(|_| self.provider_program.clone());
        self.active_external_args = external_provider.map(|_| self.provider_args.clone());
        self.external_provider_diagnostics = ExternalAgentDiagnostics::default();
        let benchmark_single_model = self.benchmark_child_active();
        let routing_candidates = native_config
            .as_ref()
            .map(|config| self.native_routing_candidates(config))
            .unwrap_or_default();
        self.native_agent_runtime = native_config.map(|config| {
            if benchmark_single_model {
                NativeAgentRuntime::configured(config)
            } else {
                NativeAgentRuntime::configured_routed(
                    config,
                    routing_candidates,
                    &self.benchmark_records,
                )
            }
        });
        self.native_run_benchmark_context =
            native_benchmark_identity.map(|(backend_id, model_id, inventory)| {
                NativeRunBenchmarkContext {
                    run_id: run_id.clone(),
                    task_id: self.benchmark_task_id.clone(),
                    backend_id,
                    model_id,
                    quality: self.quality_preference,
                    workload: benchmark_workload,
                    hardware: self.benchmark_hardware.clone(),
                    inventory,
                    routed: false,
                }
            });
        self.native_mcp_task = None;
        self.pending_native_mcp_tool = None;
        self.code_workspace = None;
        self.pending_code_changes.clear();
        self.pending_runtime_action = None;
        self.managed_input_recipe.clear();
        self.managed_candidate_input_recipe.clear();
        self.managed_runtime_plan_completed = false;
        self.managed_runtime_debug_observation = None;
        self.managed_playtest_requested = false;
        self.managed_capture_requested = false;
        self.managed_repair_requested = false;
        self.managed_runtime_repairs = 0;
        self.managed_runtime_observation = None;
        self.managed_evaluation_requested = false;
        self.native_evaluation_had_image = false;
        self.managed_playtest_started_at = None;
        self.last_captured_frame = None;
        if mode == AgentRuntimeMode::Native
            && self.benchmark_child_requires_initial_validation_failure()
        {
            self.prepare_native_workspace(&run_id)?;
            self.host
                .transition_run(
                    &run_id,
                    AgentRunState::Executing,
                    "Benchmark validation-repair baseline prepared; running the mandatory initial failing validation before model repair.",
                )
                .map_err(|error| error.to_string())?;
            self.host
                .begin_managed_validation(&run_id, true)
                .map_err(|error| error.to_string())?;
            return Ok(run_id);
        }
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
        if self.benchmark_child_allows(capability) {
            match self
                .host
                .resolve_permission(&run_id, capability, ApprovalScope::Run)
            {
                Ok(()) => self.execute_permission_action(&run_id, action),
                Err(error) => self.status = Some(error.to_string()),
            }
            return;
        }
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
                if matches!(action, PendingPermissionAction::RunRuntimeDebugPlan(_)) {
                    self.managed_runtime_plan_completed = true;
                    let _ = self.host.record_playtest_result(
                        &run_id,
                        true,
                        Some(false),
                        "Deterministic managed runtime input was denied by Runtime Input Control permission.",
                    );
                    self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
                }
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
        if let Err(error) = self.host.resolve_permission(run_id, capability, scope) {
            self.status = Some(error.to_string());
            return;
        }
        if scope == ApprovalScope::Deny {
            if matches!(action, PendingPermissionAction::RunRuntimeDebugPlan(_)) {
                self.managed_runtime_plan_completed = true;
                let _ = self.host.record_playtest_result(
                    run_id,
                    true,
                    Some(false),
                    "Deterministic managed runtime input approval was denied.",
                );
                self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
            }
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
            PendingPermissionAction::RunRuntimeDebugPlan(plan) => {
                self.pending_runtime_action = Some(AiStudioRuntimeAction::RunDebugPlan(plan));
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
        let proposal_json = match self
            .host
            .run(run_id)
            .and_then(|run| serde_json::to_string(&run.proposal_snapshot).map_err(Into::into))
        {
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
                    self.status = Some(
                        "Runtime evaluation requires a host-captured frame artifact.".to_owned(),
                    );
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
        let placement = self.external_agent_placement();
        // A provider placed in WSL2 reaches the Editor only when the
        // distribution shares the host loopback. Proving that here keeps the
        // failure at launch, where it names the cause, instead of inside a turn.
        if let Err(error) = probe_wsl_loopback_reachability(&placement, &self.connection.endpoint) {
            match purpose {
                ExternalAgentPurpose::BuildOrRepair => self.fail_run(run_id, error),
                ExternalAgentPurpose::RuntimeEvaluation => {
                    self.record_runtime_evaluation_failure(run_id, error);
                }
            }
            return;
        }
        if placement.environment == ExternalAgentExecutionEnvironment::Wsl2Linux {
            // Windows variables reach a Linux process only when WSLENV names
            // them, and the captured-frame path must be translated so the
            // provider opens the file the Editor wrote.
            let forwarding =
                wsl_environment_forwarding(&environment, &["GAMEENGINE_AGENT_CAPTURE_PATH"]);
            environment.push(forwarding);
        }
        let launch_plan = match build_launch_plan(
            provider_kind,
            &placement,
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
                    && let Err(audit_error) = self.host.record_confinement_profile(run_id, profile)
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
            .map_err(|error| {
                format!("Could not release external agent authoring ownership: {error}")
            })
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
                            if let Err(error) = self.record_provider_semantic_event(&run_id, event)
                            {
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
                                if let Err(error) =
                                    self.host.record_tool_action(&run_id, tool, action, success)
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
                format!(
                    "{stream} output received; raw provider text omitted from persisted history."
                ),
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
                        format!(
                            "{} The provider classified this failure as retryable.",
                            failure.message
                        )
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
                if let Err(release_error) = self.release_external_authoring_claim(&run_id, purpose)
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
        self.native_evaluation_had_image = false;
        if self.active_runtime_mode == Some(AgentRuntimeMode::Native) {
            let structured = self
                .managed_runtime_debug_observation
                .clone()
                .unwrap_or_else(|| "Structured runtime observation unavailable.".to_owned());
            let visual_supported = self
                .native_agent_runtime
                .as_ref()
                .is_some_and(NativeAgentRuntime::supports_visual_evaluation);
            if visual_supported {
                let Some(observation) = self.managed_runtime_observation.clone() else {
                    self.record_runtime_evaluation_failure(
                        run_id,
                        "Native visual runtime evaluation requires a host-captured frame artifact."
                            .to_owned(),
                    );
                    return;
                };
                let image = match fs::read(&observation.path) {
                    Ok(image) => image,
                    Err(error) => {
                        self.record_runtime_evaluation_failure(
                            run_id,
                            format!(
                                "Could not read host-captured frame for native evaluation: {error}"
                            ),
                        );
                        return;
                    }
                };
                self.native_evaluation_had_image = true;
                let context = format!(
                    "Evaluate the attached host-captured managed Play frame {} ({}x{}). Resolve visual_evaluation with host-reportable evidence. Deterministic structured runtime evidence: {structured}",
                    observation.artifact_id, observation.width, observation.height
                );
                if let Err(error) = self.start_native_agent_turn(run_id, Some(context), vec![image])
                {
                    self.record_runtime_evaluation_failure(run_id, error);
                }
            } else {
                let context = format!(
                    "The selected Native ModelBackend has no verified image-input route. Do not claim visual inspection. Evaluate only deterministic structured host runtime evidence and report visual_evaluation as not_applicable unless host evidence proves a failure: {structured}"
                );
                if let Err(error) = self.start_native_agent_turn(run_id, Some(context), Vec::new())
                {
                    self.record_runtime_evaluation_failure(run_id, error);
                }
            }
        } else {
            self.request_permission(
                run_id.to_owned(),
                AgentCapability::ExternalAgentProcess,
                PendingPermissionAction::LaunchRuntimeEvaluation,
            );
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
                    let context = self
                        .host
                        .run(&run_id)
                        .map(managed_source_repair_context)
                        .unwrap_or_else(|error| {
                            format!("Re-inspect after validation failure: {error}")
                        });
                    if let Err(error) =
                        self.start_native_agent_turn(&run_id, Some(context), Vec::new())
                    {
                        self.fail_run(&run_id, error);
                    }
                } else {
                    self.request_permission(
                        run_id,
                        AgentCapability::ExternalAgentProcess,
                        PendingPermissionAction::LaunchExternalAgent,
                    );
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
                    let context = self
                        .host
                        .run(&run_id)
                        .map(|run| {
                            managed_repair_context(run, self.managed_runtime_observation.as_ref())
                        })
                        .unwrap_or_else(|error| {
                            format!("Re-inspect after runtime failure: {error}")
                        });
                    if let Err(error) =
                        self.start_native_agent_turn(&run_id, Some(context), Vec::new())
                    {
                        self.fail_run(&run_id, error);
                    }
                } else {
                    self.request_permission(
                        run_id,
                        AgentCapability::ExternalAgentProcess,
                        PendingPermissionAction::LaunchExternalAgent,
                    );
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
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        if !self
            .host
            .run(&run_id)
            .is_ok_and(|run| run.state == AgentRunState::Playtesting)
        {
            return;
        }
        self.managed_runtime_plan_completed = false;
        self.managed_runtime_debug_observation = None;
        self.managed_capture_requested = false;
        self.managed_evaluation_requested = false;
        self.managed_runtime_observation = None;
        self.managed_playtest_requested = true;
        let config = self.selected_local_resource_config();
        let capabilities = config
            .as_ref()
            .map(|config| config.capability_profile().resource_capabilities)
            .unwrap_or_default();
        self.resolved_workload = InferenceWorkload::RuntimeObservation;
        self.resource_plan = managed_play_resource_plan(self.quality_preference, capabilities);
        let residency_request = self.resource_plan.model_residency;
        self.begin_model_residency_request_with_config(
            config,
            residency_request,
            ModelResourceContinuation::LaunchManagedPlay { run_id },
        );
    }

    fn request_managed_runtime_debug_plan_if_ready(&mut self) {
        if self.managed_playtest_started_at.is_none()
            || self.managed_runtime_plan_completed
            || self.managed_input_recipe.is_empty()
            || self.pending_permission.is_some()
            || self.pending_runtime_action.is_some()
        {
            return;
        }
        let end_tick = self
            .managed_input_recipe
            .iter()
            .map(RuntimeDebugScheduledInput::tick_offset)
            .max()
            .unwrap_or(0);
        let plan = match RuntimeDebugPlan::new(self.managed_input_recipe.clone(), end_tick) {
            Ok(plan) => plan,
            Err(error) => {
                self.managed_runtime_plan_completed = true;
                if let Some(run_id) = self.active_run_id.clone() {
                    let _ = self.host.record_playtest_result(
                        &run_id,
                        true,
                        Some(false),
                        format!("Managed deterministic runtime plan was invalid: {error}"),
                    );
                    self.pending_runtime_action = Some(AiStudioRuntimeAction::StopPlaytest);
                }
                self.status = Some(error.to_string());
                return;
            }
        };
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        self.request_permission(
            run_id,
            AgentCapability::RuntimeInputControl,
            PendingPermissionAction::RunRuntimeDebugPlan(plan),
        );
    }

    fn poll_managed_playtest_timeout(&mut self) {
        let Some(started_at) = self.managed_playtest_started_at else {
            return;
        };
        if started_at.elapsed() < std::time::Duration::from_secs(120) {
            return;
        }
        let Some(run_id) = self.active_run_id.clone() else {
            return;
        };
        let state = self.host.run(&run_id).map(|run| run.state).ok();
        if matches!(
            state,
            Some(AgentRunState::Playtesting | AgentRunState::Evaluating)
        ) {
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
        self.native_evaluation_had_image = false;
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
            CompletionStatus::NotApplicable => {
                self.status = Some(format!(
                    "Runtime evaluator finished with {exit_code:?}; visual evaluation is not applicable because no verified image-input path was used. Structured runtime evidence remains authoritative."
                ));
            }
            CompletionStatus::Pending => {
                self.record_runtime_evaluation_failure(
                    run_id,
                    format!(
                        "Runtime evaluator finished with {exit_code:?} without resolving visual_evaluation; runtime repair is required."
                    ),
                );
            }
        }
        self.native_evaluation_had_image = false;
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
        self.managed_runtime_plan_completed = false;
        self.managed_runtime_debug_observation = None;
        self.managed_repair_requested = false;
        if let Err(error) = self.host.begin_managed_validation(run_id, has_code_changes) {
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
                AgentWorkClaim::code_path(change.relative_path.to_string_lossy().replace('\\', "/"))
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

    fn record_provider_semantic_event(
        &mut self,
        run_id: &str,
        event: ProviderAgentEvent,
    ) -> Result<(), String> {
        match event {
            ProviderAgentEvent::Progress { step, detail } => self
                .host
                .record_semantic_progress(run_id, step, detail)
                .map_err(|error| error.to_string()),
            ProviderAgentEvent::ToolAction {
                tool,
                action,
                success,
            } => self
                .host
                .record_tool_action(run_id, tool, action, success)
                .map_err(|error| error.to_string()),
            ProviderAgentEvent::CompletionGate {
                gate,
                status,
                message,
            } => self
                .host
                .record_completion_gate(run_id, &gate, status, message)
                .map_err(|error| error.to_string()),
            ProviderAgentEvent::PlaytestResult {
                launched,
                interactions_passed,
                message,
            } => {
                self.host
                    .record_playtest_result(run_id, launched, interactions_passed, message)
                    .map_err(|error| error.to_string())?;
                if launched
                    && interactions_passed == Some(true)
                    && self.managed_playtest_started_at.is_some()
                    && self
                        .host
                        .run(run_id)
                        .is_ok_and(|run| run.audit.managed_runtime_inputs > 0)
                    && !self.managed_capture_requested
                {
                    self.managed_capture_requested = true;
                    self.request_permission(
                        run_id.to_owned(),
                        AgentCapability::FrameCapture,
                        PendingPermissionAction::CaptureFrame,
                    );
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
                let default_tick = next_runtime_input_tick(&self.managed_candidate_input_recipe);
                let scheduled = input.scheduled_commands(default_tick)?;
                let summary = scheduled
                    .iter()
                    .map(|input| format!("tick {} {:?}", input.tick_offset(), input.command()))
                    .collect::<Vec<_>>()
                    .join(", ");
                self.managed_candidate_input_recipe
                    .extend(scheduled.iter().cloned());
                self.host
                    .record_semantic_progress(
                        run_id,
                        "runtime_input_plan",
                        format!("Queued provider-planned fixed-tick runtime input: {summary}."),
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
        LiveObservationError::TooManySessions => {
            RemoteAiStudioResponse::error(409, "live_observation_capacity", error.to_string(), true)
        }
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
            result
                .failure
                .as_ref()
                .map(|failure| format!("{:?}: {}", result.gate, failure.message))
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

/// Draws one projected transcript entry.
///
/// An entry that may be collapsed still shows what happened and how it turned
/// out; only its detail is hidden. Permission escalations, failures, and
/// cancellations are never collapsed.
/// Whether an entry is a step the runtime took rather than something the user
/// is being told.
///
/// ADR 0158 §3 lets a presentation collapse detail but never an escalation, an
/// escape hatch, a failed gate, or an unperformed criterion, so a failed
/// outcome and an entry the projection marked uncollapsible are never grouped.
fn is_internal_step(entry: &crate::agent_transcript::TranscriptEntry) -> bool {
    use crate::agent_transcript::{TranscriptEntryKind, TranscriptOutcome};

    entry.collapsible
        && entry.outcome != Some(TranscriptOutcome::Failed)
        && matches!(
            entry.kind,
            TranscriptEntryKind::RunState
                | TranscriptEntryKind::Progress
                | TranscriptEntryKind::ToolAction
                | TranscriptEntryKind::ModelExchange
                | TranscriptEntryKind::ResourcePolicy
                | TranscriptEntryKind::WorkCoordination
                | TranscriptEntryKind::EditingState
                | TranscriptEntryKind::Note
        )
}

/// Draws the machine steps accumulated so far as one closed disclosure.
///
/// A run reports dozens of these, and reading them one card at a time buries
/// the conversation. They stay in host order and nothing is dropped: the
/// disclosure states how many there are and opens onto the same entries.
fn flush_internal_steps(
    ui: &mut egui::Ui,
    steps: &mut Vec<&crate::agent_transcript::TranscriptEntry>,
    requested: &mut Option<(
        Option<String>,
        crate::agent_transcript::TranscriptNavigation,
    )>,
) {
    match steps.len() {
        0 => return,
        1 => {
            let entry = steps[0];
            if let Some(navigation) = show_transcript_entry(ui, entry) {
                *requested = Some((entry.run_id.clone(), navigation));
            }
        }
        count => {
            let first = steps[0];
            egui::CollapsingHeader::new(format!("{count} internal steps"))
                .id_salt((
                    "ai_studio_internal_steps",
                    first.run_id.clone(),
                    first.sequence,
                    first.created_unix_ms,
                ))
                .default_open(false)
                .show(ui, |ui| {
                    for entry in steps.iter() {
                        if let Some(navigation) = show_transcript_entry(ui, entry) {
                            *requested = Some((entry.run_id.clone(), navigation));
                        }
                    }
                });
        }
    }
    steps.clear();
}

/// Draws a conversation message as a message rather than as a record.
///
/// The studio is read as a conversation (ADR 0158 §1), so what a person and the
/// agent said is drawn as text, and only the machinery around it is drawn as
/// labelled records.
fn show_message_entry(ui: &mut egui::Ui, entry: &crate::agent_transcript::TranscriptEntry) {
    use crate::agent_transcript::TranscriptEntryKind;

    let body = if entry.detail.trim().is_empty() {
        entry.summary.clone()
    } else {
        entry.detail.clone()
    };
    ui.add_space(5.0);
    if entry.kind == TranscriptEntryKind::UserMessage {
        ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
            let bubble = (ui.available_width() * 0.82).max(180.0);
            theme::card_frame()
                .fill(theme::SURFACE_HOVERED)
                .show(ui, |ui| {
                    ui.set_max_width(bubble);
                    theme::selectable_text(ui, body);
                });
        });
        return;
    }
    theme::caption(ui, entry.kind.label());
    theme::selectable_text(ui, body);
}

fn show_transcript_entry(
    ui: &mut egui::Ui,
    entry: &crate::agent_transcript::TranscriptEntry,
) -> Option<crate::agent_transcript::TranscriptNavigation> {
    use crate::agent_transcript::{TranscriptEntryKind, TranscriptNavigation, TranscriptOutcome};

    if matches!(
        entry.kind,
        TranscriptEntryKind::UserMessage
            | TranscriptEntryKind::AgentMessage
            | TranscriptEntryKind::SystemMessage
    ) {
        show_message_entry(ui, entry);
        return None;
    }
    let outcome_label = entry.outcome.map(|outcome| outcome.label());
    let header = match outcome_label {
        Some(label) => format!("{} · {} [{label}]", entry.kind.label(), entry.summary),
        None => format!("{} · {}", entry.kind.label(), entry.summary),
    };
    let mut requested = None;
    let mut draw_detail = |ui: &mut egui::Ui| {
        if !entry.detail.trim().is_empty() && entry.detail != entry.summary {
            let machine_text = matches!(
                entry.kind,
                TranscriptEntryKind::ToolAction
                    | TranscriptEntryKind::ModelExchange
                    | TranscriptEntryKind::CodeChange
            );
            if machine_text {
                theme::selectable_text(ui, egui::RichText::new(entry.detail.clone()).monospace());
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(entry.detail.clone());
                }
            } else {
                theme::selectable_text(ui, entry.detail.clone());
            }
        }
        match entry.navigation.as_ref() {
            Some(TranscriptNavigation::CapturedFrame { artifact_id })
                if ui.small_button("Open captured frame").clicked() =>
            {
                requested = Some(TranscriptNavigation::CapturedFrame {
                    artifact_id: artifact_id.clone(),
                });
            }
            Some(TranscriptNavigation::CodeWorkspace)
                if ui.small_button("Reveal code workspace").clicked() =>
            {
                requested = Some(TranscriptNavigation::CodeWorkspace);
            }
            _ => {}
        }
    };
    let failed = entry.outcome == Some(TranscriptOutcome::Failed);
    ui.group(|ui| {
        if entry.collapsible {
            egui::CollapsingHeader::new(header)
                .id_salt((entry.run_id.clone(), entry.sequence, entry.created_unix_ms))
                .default_open(false)
                .show(ui, &mut draw_detail);
            return;
        }
        if failed {
            ui.colored_label(theme::ACCENT_TEXT, header);
        } else {
            ui.strong(header);
        }
        draw_detail(ui);
    });
    requested
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
        _ => Err(format!(
            "unsupported managed runtime mouse button `{button}`"
        )),
    }
}

fn provider_gamepad_button(button: &str) -> Result<GamepadButton, String> {
    match button.trim().to_ascii_lowercase().as_str() {
        "south" | "a" => Ok(GamepadButton::South),
        "east" | "b" => Ok(GamepadButton::East),
        "west" | "x" => Ok(GamepadButton::West),
        "north" | "y" => Ok(GamepadButton::North),
        "left_shoulder" | "lb" => Ok(GamepadButton::LeftShoulder),
        "right_shoulder" | "rb" => Ok(GamepadButton::RightShoulder),
        "select" | "back" => Ok(GamepadButton::Select),
        "start" | "menu" => Ok(GamepadButton::Start),
        _ => Err(format!(
            "unsupported managed runtime gamepad button `{button}`"
        )),
    }
}

fn provider_gamepad_axis(axis: &str) -> Result<GamepadAxis, String> {
    match axis.trim().to_ascii_lowercase().as_str() {
        "left_stick_x" => Ok(GamepadAxis::LeftStickX),
        "left_stick_y" => Ok(GamepadAxis::LeftStickY),
        "right_stick_x" => Ok(GamepadAxis::RightStickX),
        "right_stick_y" => Ok(GamepadAxis::RightStickY),
        "left_trigger" => Ok(GamepadAxis::LeftTrigger),
        "right_trigger" => Ok(GamepadAxis::RightTrigger),
        _ => Err(format!("unsupported managed runtime gamepad axis `{axis}`")),
    }
}

/// Smallest embedded studio that still shows a conversation beside its cards.
const EMBEDDED_MIN_SIZE: egui::Vec2 = egui::vec2(460.0_f32, 520.0_f32);
/// Size the embedded studio opens at before the user resizes it.
const EMBEDDED_DEFAULT_SIZE: egui::Vec2 = egui::vec2(600.0_f32, 760.0_f32);
/// Room the embedded studio leaves around itself inside the Editor.
const EMBEDDED_EDITOR_MARGIN: f32 = 80.0_f32;

/// Returns the embedded AI Studio window, kept inside the Editor around it.
///
/// egui moves a window that is larger than the screen but never shrinks one, so
/// an Editor shorter than the studio's opening size would leave the studio's
/// lower edge — and the scroll bar that reaches its lower cards — hanging past
/// the bottom of the display. The bound is generous enough that it never fights
/// a user dragging the studio wider.
fn embedded_window(context: &egui::Context) -> egui::Window<'static> {
    let editor = context.content_rect();
    let max_width = (editor.width() - EMBEDDED_EDITOR_MARGIN).max(EMBEDDED_MIN_SIZE.x);
    let max_height = (editor.height() - EMBEDDED_EDITOR_MARGIN).max(EMBEDDED_MIN_SIZE.y);
    egui::Window::new("AI Studio")
        .id(egui::Id::new("gameengine_ai_studio"))
        .frame(
            egui::Frame::window(&context.global_style())
                .fill(theme::BACKGROUND)
                .stroke(egui::Stroke::new(1.0_f32, theme::BORDER)),
        )
        .default_pos(egui::pos2(940.0_f32, 84.0_f32))
        .default_size(EMBEDDED_DEFAULT_SIZE)
        .min_width(EMBEDDED_MIN_SIZE.x)
        .min_height(EMBEDDED_MIN_SIZE.y)
        .max_width(max_width)
        .max_height(max_height)
        .resizable(true)
}

/// Returns the scroll area shape the embedded studio's contents must keep.
///
/// The layout itself now uses panels, so this stands in for those contents in
/// the window-size contract test.
#[cfg(test)]
///
/// This is what keeps the studio a panel. An `egui::Window` grows to whatever
/// its contents ask for, so without a scroll area the studio's own cards expand
/// it until it covers the Editor, and the cards past the screen edge cannot be
/// reached at all. Refusing to shrink spans the cards across the window the way
/// the detached presentation already draws them, rather than sizing them to the
/// widest card.
fn embedded_contents_scroll_area() -> egui::ScrollArea {
    egui::ScrollArea::vertical()
        .id_salt("ai_studio_embedded_contents")
        .auto_shrink([false, false])
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
            message.push_str(&format!("- {}:{}\n", source.kind.label(), source.path));
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

/// Returns every criterion of a completion report, for reporting rules that
/// care whether anything is still unperformed rather than which check it is.
fn completion_statuses(report: &crate::agent_host::CompletionReport) -> [CompletionStatus; 7] {
    [
        report.acceptance_criteria,
        report.authoring_validation,
        report.source_validation,
        report.play_launch,
        report.frame_capture,
        report.visual_evaluation,
        report.interaction_scenarios,
    ]
}

/// Draws an immutable proposal snapshot as text.
fn show_proposal_snapshot(ui: &mut egui::Ui, proposal: &AgentProposal) {
    theme::caption(ui, "Goal");
    theme::selectable_text(ui, proposal.goal.clone());
    snapshot_lines(ui, "Requirements", &proposal.requirements);
    snapshot_lines(ui, "Assumptions", &proposal.assumptions);
    snapshot_lines(ui, "Acceptance criteria", &proposal.acceptance_criteria);
    snapshot_lines(
        ui,
        "Planned project changes",
        &proposal.planned_project_changes,
    );
    snapshot_lines(ui, "Planned code changes", &proposal.planned_code_changes);
    snapshot_lines(ui, "Planned assets", &proposal.planned_assets);
    snapshot_lines(ui, "Validation plan", &proposal.validation_plan);
    snapshot_lines(ui, "Playtest plan", &proposal.playtest_plan);
}

/// Draws one read-only list of a proposal snapshot, omitting empty sections.
fn snapshot_lines(ui: &mut egui::Ui, label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    theme::caption(ui, label);
    for value in values {
        theme::selectable_text(ui, format!("· {value}"));
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
        let event: ProviderAgentEvent =
            serde_json::from_str(r#"{"type":"progress","step":"inspect","detail":"scene"}"#)
                .expect("semantic event");
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
        let scheduled = input.scheduled_commands(0).expect("scheduled command");
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].tick_offset(), 0);
        assert_eq!(
            scheduled[0].command(),
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
            at_tick: None,
        };
        assert!(input.scheduled_commands(0).is_err());
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
            source_repair_decision(AgentRunState::Repairing, CompletionStatus::Failed, 1,),
            SourceRepairDecision::Retry(1)
        );
        assert_eq!(
            source_repair_decision(AgentRunState::Repairing, CompletionStatus::Failed, 2,),
            SourceRepairDecision::Retry(2)
        );
        assert_eq!(
            source_repair_decision(AgentRunState::Repairing, CompletionStatus::Failed, 3,),
            SourceRepairDecision::Exhausted
        );
        assert_eq!(
            source_repair_decision(AgentRunState::Repairing, CompletionStatus::Pending, 1,),
            SourceRepairDecision::Wait
        );
        assert_eq!(
            source_repair_decision(AgentRunState::Evaluating, CompletionStatus::Failed, 1,),
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
            conversation_mode: ConversationMode::Build,
            quality_preference: QualityPreference::Balanced,
            confinement_requirement: AgentConfinementRequirement::default(),
            external_agent_provider: ExternalAgentProviderKind::ClaudeCode,
            external_agent_execution_environment: ExternalAgentExecutionEnvironment::Wsl2Linux,
            external_agent_wsl_distribution: "Ubuntu-24.04".to_owned(),
            model_backend: ModelBackendPreference::HostedApi,
            managed_execution_environment: ManagedExecutionEnvironment::WindowsNative,
            managed_model_id: String::new(),
            local_model_endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            local_model_name: String::new(),
            hosted_model_endpoint: "https://provider.example/v1/chat/completions".to_owned(),
            hosted_model_name: "example-model".to_owned(),
            presentation_mode: AiStudioPresentationMode::default(),
        };
        let json = serde_json::to_string(&preferences).expect("serialize preferences");
        assert!(!json.contains("authorization"));
        assert!(!json.contains("bearer"));
        assert!(!json.contains("protected_path"));
    }

    #[test]
    fn preferences_written_before_conversation_modes_default_to_ask() {
        // ADR 0162 compatibility: an installation that predates the mode must
        // not gain write-on-send from an upgrade alone.
        let preferences: AiStudioPreferences =
            serde_json::from_str(r#"{"schema_version":1,"model_backend":"local"}"#)
                .expect("deserialize preferences without a conversation mode");
        assert_eq!(preferences.conversation_mode, ConversationMode::Ask);
    }

    #[test]
    fn legacy_external_local_preferences_are_not_silently_migrated_to_managed() {
        let preferences: AiStudioPreferences = serde_json::from_str(
            r#"{"schema_version":1,"model_backend":"local","local_model_endpoint":"http://127.0.0.1:11434","local_model_name":"legacy-model"}"#,
        )
        .expect("deserialize legacy external-local preferences");
        assert_eq!(preferences.model_backend, ModelBackendPreference::Local);
        assert_eq!(
            preferences.managed_execution_environment,
            ManagedExecutionEnvironment::WindowsNative
        );
        assert!(preferences.managed_model_id.is_empty());
        assert_eq!(preferences.local_model_name, "legacy-model");
        assert_eq!(preferences.local_model_endpoint, "http://127.0.0.1:11434");
        assert_eq!(
            AiStudioPreferences::default().model_backend,
            ModelBackendPreference::Local
        );
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
    fn presentation_opens_detached_until_the_user_reattaches_it() {
        assert_eq!(
            AiStudioPresentationState::default().mode,
            AiStudioPresentationMode::Detached
        );
        // A studio that was reattached must stay reattached across Editor
        // restarts, so the chosen mode is machine-local preference state rather
        // than a per-session default.
        let reattached: AiStudioPreferences =
            serde_json::from_str(r#"{"schema_version":1,"presentation_mode":"embedded"}"#)
                .expect("deserialize reattached presentation preferences");
        assert_eq!(
            reattached.presentation_mode,
            AiStudioPresentationMode::Embedded
        );
        let legacy: AiStudioPreferences = serde_json::from_str(r#"{"schema_version":1}"#)
            .expect("deserialize preferences written before the mode was persisted");
        assert_eq!(legacy.presentation_mode, AiStudioPresentationMode::Detached);
    }

    /// Returns the cursor the studio asks for while one piece of text is hovered.
    fn studio_cursor_over_text(selectable: bool) -> egui::CursorIcon {
        const TEXT: &str = "Managed Local AI";
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0_f32, 400.0_f32));
        let context = egui::Context::default();
        let draw = |ui: &mut egui::Ui| {
            theme::apply_studio_style(ui);
            if selectable {
                theme::selectable_text(ui, TEXT).rect
            } else {
                ui.label(TEXT).rect
            }
        };

        // The first pass lays the text out so the second can aim the pointer at
        // it; a cursor is only claimed by whatever the pointer is over.
        let mut rect = egui::Rect::NOTHING;
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..egui::RawInput::default()
            },
            |ui| rect = draw(ui),
        );
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                events: vec![egui::Event::PointerMoved(rect.center())],
                ..egui::RawInput::default()
            },
            |ui| {
                draw(ui);
            },
        );
        output.platform_output.cursor_icon
    }

    /// Studio chrome must not turn the pointer into a text caret.
    ///
    /// Reported as buttons that become text selection where their labels are:
    /// a selectable label claims the I-beam for its glyphs, so a row of
    /// controls flickered between the pointer and the caret as it was crossed.
    #[test]
    fn only_copyable_studio_text_claims_the_text_cursor() {
        assert_ne!(
            studio_cursor_over_text(false),
            egui::CursorIcon::Text,
            "studio chrome claimed the text caret"
        );
        assert_eq!(
            studio_cursor_over_text(true),
            egui::CursorIcon::Text,
            "text offered for copying must still be selectable"
        );
    }

    /// The embedded studio must stay a panel and must reach its lower cards.
    ///
    /// Reported together: the studio grew until it covered the Editor, and once
    /// reattached its contents could not be scrolled, so everything below the
    /// window edge was unreachable.
    #[test]
    fn embedded_presentation_fits_the_editor_and_scrolls_to_its_lower_contents() {
        // A long unbroken status line is what the studio writes while managed
        // Local AI setup runs, and it is one of the contents that used to widen
        // the window.
        const LONG_STATUS: &str = "Provisioning the dedicated GameEngine-LocalAI WSL environment. Windows may require explicit elevation or restart; GameEngine never bypasses either boundary.";
        // The window frame draws its title bar and margins outside the content
        // area the size bounds apply to.
        const WINDOW_CHROME: f32 = 60.0_f32;

        for (editor, tallest) in [
            // A roomy Editor: the studio must stay the size it opens at instead
            // of growing with its contents.
            (
                egui::vec2(1_600.0_f32, 900.0_f32),
                EMBEDDED_DEFAULT_SIZE.y + WINDOW_CHROME,
            ),
            // An Editor shorter than the studio's opening size: the studio must
            // fit anyway, because egui moves an oversized window but never
            // shrinks one.
            (egui::vec2(1_000.0_f32, 640.0_f32), 640.0_f32),
        ] {
            let context = egui::Context::default();
            // A window resolves its size from the previous frame's contents, so
            // unbounded growth needs more than one frame to show up.
            for frame in 0..4 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, editor)),
                    ..egui::RawInput::default()
                };
                let mut window_rect = None;
                let mut overflow = None;
                let _ = context.run_ui(input, |ui| {
                    let response = embedded_window(ui.ctx()).show(ui.ctx(), |ui| {
                        theme::apply_studio_style(ui);
                        let scrolled = embedded_contents_scroll_area().show(ui, |ui| {
                            ui.horizontal(|ui| {
                                theme::status_dot(ui, theme::ACCENT_TEXT);
                                ui.label(LONG_STATUS);
                            });
                            // Stands in for the studio's stack of cards, which
                            // is taller than any window the Editor can host.
                            ui.allocate_exact_size(
                                egui::vec2(10.0_f32, 4_000.0_f32),
                                egui::Sense::hover(),
                            );
                        });
                        overflow = Some(scrolled.content_size.y - scrolled.inner_rect.height());
                    });
                    window_rect = response.map(|response| response.response.rect);
                });

                let window = window_rect.expect("embedded studio must be laid out");
                assert!(
                    window.height() <= tallest,
                    "frame {frame} grew the embedded studio to {} tall in a {}-tall Editor",
                    window.height(),
                    editor.y
                );
                assert!(
                    window.width() <= editor.x,
                    "frame {frame} widened the embedded studio to {} in a {}-wide Editor",
                    window.width(),
                    editor.x
                );
                let overflow = overflow.expect("embedded contents must be laid out");
                assert!(
                    overflow > 0.0_f32,
                    "frame {frame} left the lower contents unreachable instead of scrollable"
                );
            }
        }
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
        let reload = resume_model_resource_operation_after_authoritative_inspection(capabilities);
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
