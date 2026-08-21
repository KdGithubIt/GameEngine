//! Conversation-first AI Studio frontend.
//!
//! This module owns only presentation and direct user interaction. Agent
//! lifecycle, permissions, persistence, provider process management, and code
//! workspace rules live in the GUI-free `agent_host` module.

mod benchmark_campaign_ui;
mod benchmark_child;
mod benchmark_experiment_ui;
#[allow(dead_code)]
mod execution_routing;
mod settings_ui;

use crate::acp_agent_host_bridge::AcpBridgePoll;
use crate::acp_agent_runtime::{AcpNormalizedEvent, AcpProcessRuntime};
use crate::acp_integration::AcpIntegration;
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
use crate::benchmark_experiment::BenchmarkRunFailureKind;
use crate::claude_acp_adapter::{CLAUDE_ACP_AGENT_ID, ClaudeAcpConfig, discover_claude_acp};
use crate::codex_acp_adapter::{
    CODEX_ACP_DESCRIPTOR_ID, CodexAcpRuntime, CodexAcpSessionPreferences,
};
use crate::external_agent_provider::{
    ExternalAgentDiagnostics, ExternalAgentExecutionEnvironment, ExternalAgentExecutionPlacement,
    ExternalAgentProbeTask, ExternalAgentProviderKind, ExternalAgentProviderReport,
    ExternalAgentProviderStatus, ExternalAgentQuestionRole, ExternalAgentQuestionTask,
    ExternalAgentQuestionTurn, ExternalAgentSemanticEvent, ExternalAgentSetupAction,
    ExternalAgentSetupTask, build_launch_plan, build_question_launch_plan, build_question_prompt,
    probe_editor_mcp_endpoint, probe_provider, probe_wsl_loopback_reachability, setup_command_text,
    sign_in_url, translate_provider_line, wsl_environment_forwarding,
};
use crate::goose_local_acp::{
    GOOSE_ACP_AGENT_NAME, GOOSE_LOCAL_ACP_DESCRIPTOR_ID, GooseLocalAcpConfig, GooseLocalAcpRuntime,
};
use crate::hosted_model_backend;
use crate::hosted_model_backend::{HostedAuthMode, HostedModelConfig};
use crate::live_observation::{LiveObservationError, LiveObservationManager};
use crate::managed_local_runtime::{
    GgufModelCapability, MANAGED_BACKEND_ID, ManagedEnvironmentProbe, ManagedEnvironmentProbeTask,
    ManagedExecutionEnvironment, ManagedGooseSetupStatus, ManagedLocalModelConfig,
    ManagedLocalRuntime, ManagedSetupOperation, ManagedSetupResult, ManagedSetupStatus,
    ManagedSetupTask, PINNED_GOOSE_VERSION, PINNED_LLAMA_CPP_REVISION, PINNED_LLAMA_CPP_TAG,
};
use crate::model_router::{MODEL_ROUTER_POLICY_VERSION, ModelRoutingPolicy};
use crate::native_agent::{
    DEFAULT_LOCAL_MODEL_ENDPOINT, InstalledLocalModel, InstalledModelDiscoveryTask,
    InstalledModelInventory, LocalModelConfig, LocalModelResourceConfig, ModelCapabilityProfile,
    ModelResourceTask, NativeAnswer, NativeMetrics, NativeModelConfig, NativeQuestionTask,
    NativeSamplingOptions, QuestionMessage, QuestionRole,
};
use crate::native_agent_runtime::{
    NativeAgentAction, NativeAgentRuntime, NativeMcpTask, mcp_write,
};
use crate::remote_ai_studio::{
    PhoneUrlBaseError, RemoteAiStudioRequest, RemoteAiStudioResponse, RemoteAiStudioServer,
    RemoteOperation, RemotePermissionScope, events_json, frame_bytes, masked_phone_url,
    sessions_json, snapshot_json,
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
use execution_routing::{AiExecutionDriver, AiExecutionRouter};
use serde::{Deserialize, Serialize};
use settings_ui::{ProviderReadiness, SettingsSection};
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

const PROVIDER_EVENT_PREFIX: &str = "GAMEENGINE_AGENT_EVENT ";
const MAX_AUTONOMOUS_SOURCE_REPAIRS: usize = 2;
const MAX_AUTONOMOUS_RUNTIME_REPAIRS: usize = 2;
const AI_STUDIO_PREFERENCES_SCHEMA_VERSION: u32 = 1;
/// How long a managed-environment snapshot is reused before a worker re-probes it.
const MANAGED_PROBE_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
/// How many provider setup output lines the settings surface keeps.
const MAX_SETUP_LOG_LINES: usize = 40;

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

    /// Returns the identity the companion uses for this backend.
    const fn remote_id(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::ManagedLocal => "managed_local",
            Self::HostedApi => "hosted_api",
            Self::Enterprise => "enterprise",
        }
    }

    /// Returns which group of the composer's AI list this backend appears in.
    const fn ai_group(self) -> AiGroup {
        match self {
            Self::Local | Self::ManagedLocal => AiGroup::LocalModels,
            Self::HostedApi | Self::Enterprise => AiGroup::Cloud,
        }
    }
}

/// Which group of the composer's AI list an entry appears in.
///
/// ADR 0164 §1 groups by what the reader is choosing between rather than by
/// which internal path serves the entry, so a backend added later has to say
/// where it belongs instead of silently missing the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiGroup {
    /// Runs on this machine.
    LocalModels,
    /// Runs on someone else's machine.
    Cloud,
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

    /// Returns the identity the companion uses for this mode.
    const fn remote_id(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Build => "build",
        }
    }

    /// Returns the shortest accurate summary of the mode.
    ///
    /// Used where both modes are listed side by side, so the mode that is not
    /// selected can still be understood without repeating the sentence the
    /// composer already states about the mode that is.
    const fn summary(self) -> &'static str {
        match self {
            Self::Ask => "Answers from project evidence. Never writes.",
            Self::Build => "Commits your message as the proposal and starts a run.",
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
    /// Whether Ask is answered by the ready external provider (ADR 0163 §1).
    ///
    /// Retained for compatibility only; see [`default_ask_uses_external_provider`].
    #[serde(default = "default_ask_uses_external_provider")]
    ask_uses_external_provider: bool,
    /// Which family the selected AI belongs to (ADR 0164 §1).
    ///
    /// Absent in a file written before ADR 0164, which is why it is optional:
    /// the family is then derived from the three controls that record replaced.
    #[serde(default)]
    selected_ai_family: Option<SelectedAiFamily>,
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
    /// The external origin a private reverse proxy publishes for this PC.
    ///
    /// ADR 0164 §4: empty is the not-ready state, so an installation written
    /// before that record loads without gaining a claim about reachability.
    #[serde(default)]
    remote_phone_url_base: String,
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
            ask_uses_external_provider: default_ask_uses_external_provider(),
            selected_ai_family: None,
            model_backend: ModelBackendPreference::Local,
            managed_execution_environment: ManagedExecutionEnvironment::WindowsNative,
            managed_model_id: String::new(),
            local_model_endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            local_model_name: String::new(),
            hosted_model_endpoint: String::new(),
            hosted_model_name: String::new(),
            presentation_mode: AiStudioPresentationMode::default(),
            remote_phone_url_base: String::new(),
        }
    }
}

fn default_local_model_endpoint() -> String {
    DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned()
}

impl AiStudioPreferences {
    /// Returns which family the recorded preferences select.
    ///
    /// ADR 0164 Compatibility: a file written before that record has no
    /// selection of its own, and instead records a provider, a backend, and an
    /// Ask routing flag. Exactly one selection is recovered from them — the
    /// agent when Ask was routed to one that can answer questions, and the
    /// recorded `ModelBackend` in every other case — so an upgrade never
    /// changes who runs the next message.
    fn resolved_selected_ai_family(&self) -> SelectedAiFamily {
        if let Some(family) = self.selected_ai_family {
            return family;
        }
        if self.ask_uses_external_provider && self.external_agent_provider.can_answer_questions() {
            SelectedAiFamily::Agent
        } else {
            SelectedAiFamily::Model
        }
    }
}

/// Ask prefers a ready external provider so one signed-in provider serves both
/// modes without a second runtime to configure (ADR 0163 §1).
///
/// ADR 0164 §1 removed the control that set this. The field remains readable so
/// a preference file written before that record still loads, and it is consumed
/// once, by [`AiStudioPreferences::resolved_selected_ai_family`].
const fn default_ask_uses_external_provider() -> bool {
    true
}

/// Which family the selected AI belongs to.
///
/// ADR 0164 §1 makes "who runs the next message" one value. The entry inside
/// each family is carried by the field that family already used, so the split
/// ADR 0163 established between an agent program and a `ModelBackend` survives
/// unchanged behind a single selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum SelectedAiFamily {
    /// An external agent program with its own account and its own models.
    Agent,
    /// A model GameEngine itself runs or calls.
    #[default]
    Model,
}

/// Who runs the next message.
///
/// ADR 0164 §1: this is the one value the composer selects. Mode display,
/// effective write capability, and the executing path are all derived from it
/// together with the conversation mode, so the studio cannot report one
/// executor while another performs the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectedAi {
    /// An external agent program answers Ask and runs Build.
    Agent(ExternalAgentProviderKind),
    /// A model backend answers Ask and runs Build.
    Model(ModelBackendPreference),
}

/// Whether an Ask turn is answered by an external agent.
///
/// ADR 0164 §1 removes the routing preference: selecting an agent is the
/// statement that the agent serves the selected mode. ADR 0163 §1's conditions
/// are otherwise unchanged — the agent must be one whose read-only launch
/// GameEngine can construct, and the status being read must describe that same
/// agent and report it ready.
/// Returns the stable identity the companion uses for one AI entry.
///
/// The identity has to survive a restart and a reordering of the list, so it
/// names what the entry is rather than where it appears. `managed_model_id` is
/// read only for a Managed Local entry, where the registered model is what
/// distinguishes one entry from another.
fn ai_entry_id(selection: SelectedAi, managed_model_id: &str) -> String {
    match selection {
        SelectedAi::Agent(kind) => format!("agent:{}", kind.run_label()),
        SelectedAi::Model(ModelBackendPreference::ManagedLocal) => {
            format!("model:managed_local:{managed_model_id}")
        }
        SelectedAi::Model(backend) => format!("model:{}", backend.remote_id()),
    }
}

/// Returns the identity the companion uses for one effort level.
fn effort_id(quality: QualityPreference) -> String {
    quality.label().to_ascii_lowercase()
}

/// Returns which effort level an identity names.
fn effort_from_id(id: &str) -> Option<QualityPreference> {
    QualityPreference::ALL
        .into_iter()
        .find(|quality| quality.label().eq_ignore_ascii_case(id))
}

/// Returns why an agent cannot serve a mode, when it cannot.
///
/// ADR 0164 §2: the sentence names the entry, says which mode it cannot serve,
/// and points at the two places that change it. It never proposes running the
/// turn on something else, because the composer would then be naming an
/// executor that did not do the work.
fn agent_unavailable_for_mode(
    kind: ExternalAgentProviderKind,
    mode: ConversationMode,
    readiness: ProviderReadiness,
) -> Option<String> {
    if mode == ConversationMode::Ask && !kind.can_answer_questions() {
        return Some(format!(
            "{} can run Build, and cannot answer Ask. Choose a model for Ask, or switch to Build.",
            kind.label()
        ));
    }
    match readiness {
        ProviderReadiness::Ready => None,
        ProviderReadiness::Working => Some(format!(
            "{} is still being checked on this machine.",
            kind.label()
        )),
        _ => Some(format!(
            "{} cannot {} yet: {}. Set it up under Agents in settings, or choose another AI.",
            kind.label(),
            match mode {
                ConversationMode::Ask => "answer",
                ConversationMode::Build => "build",
            },
            readiness.label().to_lowercase()
        )),
    }
}

fn ask_is_agent_served(selection: SelectedAi, status: &ExternalAgentProviderStatus) -> bool {
    match selection {
        SelectedAi::Agent(kind) => {
            kind.can_answer_questions() && status.kind == kind && status.ready()
        }
        SelectedAi::Model(_) => false,
    }
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
    BenchmarkProfilePrepared,
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

/// Complete provider-facing schema for GameEngine semantic events. This text
/// is inserted into the actual prompt as well as the child environment because
/// provider models do not implicitly inspect environment variables.
const PROVIDER_EVENT_PROTOCOL_GUIDANCE: &str = r#"Each semantic event must be exactly one standalone line:
GAMEENGINE_AGENT_EVENT {JSON}
Supported JSON objects (no other type or field names are accepted):
{"type":"progress","step":"string","detail":"string"}
{"type":"tool_action","tool":"string","action":"string","success":true|false|null}
{"type":"completion_gate","gate":"acceptance_criteria"|"authoring_validation"|"visual_evaluation","status":"pending"|"passed"|"failed"|"not_applicable","message":"string"}
{"type":"playtest_result","launched":true|false,"interactions_passed":true|false|null,"message":"string"}
{"type":"runtime_input","input":{"kind":"key","key":"string","pressed":true|false,"at_tick":optional_u64}}
{"type":"runtime_input","input":{"kind":"hold_key","key":"string","ticks":u64,"at_tick":optional_u64}}
{"type":"runtime_input","input":{"kind":"mouse_button","button":"string","pressed":true|false,"at_tick":optional_u64}}
{"type":"runtime_input","input":{"kind":"hold_mouse_button","button":"string","ticks":u64,"at_tick":optional_u64}}
{"type":"runtime_input","input":{"kind":"gamepad_button","gamepad":u32,"button":"string","pressed":true|false,"at_tick":optional_u64}}
{"type":"runtime_input","input":{"kind":"gamepad_axis","gamepad":u32,"axis":"string","value":f32,"at_tick":optional_u64}}
{"type":"runtime_input","input":{"kind":"mouse_move","x":f32,"y":f32,"at_tick":optional_u64}}
{"type":"runtime_input","input":{"kind":"mouse_delta","x":f64,"y":f64,"at_tick":optional_u64}}
{"type":"runtime_input","input":{"kind":"mouse_scroll","amount":f32,"at_tick":optional_u64}}
The only valid completion gate names are acceptance_criteria, authoring_validation, and visual_evaluation. Do not substitute validation, source_validation, or similar names."#;

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
    read_only_authorization_token: String,
}

impl AiStudioConnection {
    /// Creates an in-memory connection descriptor for the active Editor MCP host.
    pub fn new(
        endpoint: impl Into<String>,
        authorization_token: impl Into<String>,
        read_only_authorization_token: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            authorization_token: authorization_token.into(),
            read_only_authorization_token: read_only_authorization_token.into(),
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

struct AcpQuestionState {
    acp_session_id: String,
    gameengine_session_id: String,
    answer: String,
    started_at: std::time::Instant,
}

struct AcpPendingPermission {
    acp_session_id: String,
    request_id: String,
    run_id: String,
    capability: AgentCapability,
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
    /// The external origin a private reverse proxy publishes for this PC.
    ///
    /// ADR 0164 §4 composes the phone URL from this value and the gateway's own
    /// access token. It is user-supplied because the hop that produces it is
    /// user-owned, and GameEngine must not learn it from an overlay vendor.
    remote_phone_url_base: String,
    live_observation: LiveObservationManager,
    selected_session: String,
    session_title_draft: Option<String>,
    session_delete_confirmation: Option<String>,
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
    /// Set while the settings surface asks to move the studio between its
    /// embedded and detached presentations (ADR 0147).
    ///
    /// The request is recorded rather than applied where it is made, because
    /// the control is drawn inside the very presentation it replaces.
    presentation_toggle_requested: bool,
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
    /// Which family the selected AI belongs to (ADR 0164 §1).
    ///
    /// Together with `external_provider_kind` or `model_backend` this is the
    /// single value the composer selects. It is written only by
    /// [`AiStudioPanel::select_ai`], so the family and the entry cannot drift
    /// apart into the two independent selections ADR 0164 §1 replaced.
    selected_ai_family: SelectedAiFamily,
    execution_router: AiExecutionRouter,
    acp: AcpIntegration,
    acp_question: Option<AcpQuestionState>,
    active_acp_agent_id: Option<String>,
    active_acp_run_session: Option<String>,
    pending_acp_permission: Option<AcpPendingPermission>,
    /// The provider process answering the current Ask turn, if one is running.
    external_question: Option<ExternalAgentQuestionTask>,
    /// Which session a provider-served answer belongs to.
    external_question_session: Option<String>,
    /// A provider install or sign-in the Editor started (ADR 0163 §2, §4).
    external_setup: Option<ExternalAgentSetupTask>,
    /// Provider output retained while a setup step is in progress.
    external_setup_log: Vec<String>,
    /// Confirmation text or device code entered for an interactive sign-in.
    external_setup_input: String,
    /// The sign-in URL the provider printed, when it printed one.
    external_sign_in_url: Option<String>,
    /// A background discovery/authentication probe of first-class providers.
    external_provider_probe: Option<ExternalAgentProbeTask>,
    /// Whether the once-per-session provider probe has already been requested.
    external_provider_probe_requested: bool,
    /// The most recent probe report for every first-class provider.
    external_provider_probe_results: Vec<ExternalAgentProviderReport>,
    /// Whether a ready provider has already been offered as the selection.
    ///
    /// ADR 0163 §3 adopts a detected provider only while nothing else has been
    /// configured, and only once per Editor session.
    external_provider_adoption_done: bool,
    model_backend: ModelBackendPreference,
    /// Which backend's settings the Models section is currently about.
    ///
    /// ADR 0164 §1 leaves the selection to the composer, so this scopes what is
    /// being configured and never decides who runs the next message. It is not
    /// persisted: it is a reading position, not a preference.
    settings_model_view: ModelBackendPreference,
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
    /// A provider-reported MCP failure prevents a successful process exit from
    /// being treated as a completed Build.
    external_provider_mcp_failure: Option<String>,
    /// Whether the provider stream claimed workspace file-change activity in
    /// the current process. Used to detect Windows sandbox false-success.
    external_provider_reported_workspace_write: bool,
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
        Self::new_with_campaign_recovery(project, connection, true)
    }

    /// Opens one isolated benchmark child without restoring interactive campaign state.
    ///
    /// The child belongs to an already-running campaign parent. Restoring a `Running`
    /// checkpoint here would mistake normal child startup for an Editor restart and
    /// create the parent's `pause.requested` marker.
    pub fn new_benchmark_child(
        project: &ProjectRoot,
        connection: AiStudioConnection,
        benchmark_run: &std::path::Path,
    ) -> Result<Self, String> {
        let mut panel = Self::new_with_campaign_recovery(project, connection, false)?;
        panel.configure_benchmark_child(benchmark_run)?;
        Ok(panel)
    }

    fn new_with_campaign_recovery(
        project: &ProjectRoot,
        connection: AiStudioConnection,
        restore_campaign_checkpoint: bool,
    ) -> Result<Self, String> {
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
        let selected_ai_family = preferences.resolved_selected_ai_family();
        let managed_local_runtime = ManagedLocalRuntime::open(ai_root.join("managed-local"))
            .map_err(|error| error.to_string())?;
        let benchmark_store = BenchmarkStore::open(ai_root.join("benchmark"))?;
        let benchmark_experiment_root = ai_root.join("benchmark-experiments");
        let benchmark_campaign = if restore_campaign_checkpoint {
            benchmark_campaign_ui::BenchmarkCampaignPanel::load_checkpoint(
                &benchmark_experiment_root,
            )
        } else {
            benchmark_campaign_ui::BenchmarkCampaignPanel::default()
        };
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
        let acp = AcpIntegration::new(
            project.path().to_path_buf(),
            connection.endpoint.clone(),
            connection.authorization_token.clone(),
            connection.read_only_authorization_token.clone(),
        )
        .map_err(|error| error.to_string())?;
        let mut execution_router = AiExecutionRouter::default();
        execution_router
            .set_acp_route("agent:codex", CODEX_ACP_DESCRIPTOR_ID)
            .map_err(|error| error.to_string())?;
        execution_router
            .set_acp_route("agent:claude-code", CLAUDE_ACP_AGENT_ID)
            .map_err(|error| error.to_string())?;
        execution_router
            .set_acp_route("model:managed_local", GOOSE_LOCAL_ACP_DESCRIPTOR_ID)
            .map_err(|error| error.to_string())?;
        execution_router
            .set_legacy_route("agent:generic-external")
            .map_err(|error| error.to_string())?;
        Ok(Self {
            project_root: project.path().to_path_buf(),
            project_id: project.project_id().as_str().to_owned(),
            connection,
            host,
            remote_server: None,
            remote_requests: None,
            remote_phone_url_base: preferences.remote_phone_url_base,
            live_observation: LiveObservationManager::default(),
            selected_session,
            session_title_draft: None,
            session_delete_confirmation: None,
            proposal_draft,
            message_draft: String::new(),
            conversation_mode: preferences.conversation_mode,
            deferred_intent: None,
            preferences_path,
            quality_preference: preferences.quality_preference,
            confinement_requirement: preferences.confinement_requirement,
            external_provider_kind: preferences.external_agent_provider,
            settings_open: false,
            presentation_toggle_requested: false,
            settings_section: SettingsSection::Models,
            proposal_open: false,
            external_provider_environment: preferences.external_agent_execution_environment,
            external_provider_wsl_distribution: preferences.external_agent_wsl_distribution,
            external_provider_status: ExternalAgentProviderStatus::unchecked(
                preferences.external_agent_provider,
            ),
            selected_ai_family,
            execution_router,
            acp,
            acp_question: None,
            active_acp_agent_id: None,
            active_acp_run_session: None,
            pending_acp_permission: None,
            external_question: None,
            external_question_session: None,
            external_setup: None,
            external_setup_log: Vec::new(),
            external_setup_input: String::new(),
            external_sign_in_url: None,
            external_provider_probe: None,
            external_provider_probe_requested: false,
            external_provider_probe_results: Vec::new(),
            external_provider_adoption_done: false,
            model_backend: preferences.model_backend,
            settings_model_view: preferences.model_backend,
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
            external_provider_mcp_failure: None,
            external_provider_reported_workspace_write: false,
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
            benchmark_campaign,
            benchmark_experiment: benchmark_experiment_ui::BenchmarkExperimentPanel::default(),
            benchmark_experiment_root,
            status: benchmark_status,
        })
    }

    /// Authorizes an MCP request that claims to belong to an external run.
    /// Mutating calls acquire canonical authoring ownership only for the
    /// duration of the actual tool invocation, so code-only runs never block
    /// unrelated authoring and an abandoned process cannot retain ownership.
    pub fn begin_external_mcp_call(
        &mut self,
        run_id: &str,
        tool: &str,
        mutating: bool,
    ) -> Result<(), String> {
        let legacy_external_active = self.process.is_some();
        let acp_external_active = self.active_acp_run_session.is_some();
        if self.active_run_id.as_deref() != Some(run_id)
            || (!legacy_external_active && !acp_external_active)
        {
            return Err(format!(
                "MCP request for run `{run_id}` was rejected because that external run is no longer active."
            ));
        }
        self.host.run(run_id).map_err(|error| error.to_string())?;
        if mutating {
            if legacy_external_active
                && self.process_purpose != Some(ExternalAgentPurpose::BuildOrRepair)
            {
                return Err(format!(
                    "Mutating MCP tool `{tool}` is not allowed during read-only runtime evaluation."
                ));
            }
            self.host
                .acquire_work_claims(
                    run_id,
                    [AgentWorkClaim::shared_resource("canonical_authoring")],
                )
                .map_err(|error| {
                    format!("Could not acquire authoring ownership for `{tool}`: {error}")
                })?;
        }
        Ok(())
    }

    /// Records the authoritative result returned by the Editor MCP host and
    /// releases any short-lived mutation claim.
    pub fn finish_external_mcp_call(
        &mut self,
        run_id: &str,
        tool: &str,
        mutating: bool,
        succeeded: bool,
    ) {
        if let Err(error) = self.host.record_tool_action(
            run_id,
            tool,
            "Editor MCP host executed the requested tool",
            Some(succeeded),
        ) {
            self.status = Some(error.to_string());
        }
        if mutating
            && succeeded
            && let Err(error) = self.host.record_completion_gate(
                run_id,
                "authoring_validation",
                CompletionStatus::Passed,
                format!("Editor MCP host successfully applied `{tool}`."),
            )
        {
            self.status = Some(error.to_string());
        }
        if mutating
            && let Err(error) = self.host.release_work_claims(
                run_id,
                [AgentWorkClaim::shared_resource("canonical_authoring")],
            )
        {
            self.status = Some(format!(
                "Could not release authoring ownership after `{tool}`: {error}"
            ));
        }
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
        self.settings_model_view = ModelBackendPreference::ManagedLocal;
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
        self.settings_section = SettingsSection::Models;
        self.settings_open = true;
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
            "adr0164-ai-selection" => {
                self.external_provider_status = ExternalAgentProviderStatus::visual_fixture(
                    ExternalAgentProviderKind::ClaudeCode,
                );
                self.select_ai(SelectedAi::Agent(ExternalAgentProviderKind::ClaudeCode));
                self.conversation_mode = ConversationMode::Build;
                self.status = Some(
                    "One AI selection · agents, local models, and cloud in one list · mode, AI, and effort are the only composer selections."
                        .to_owned(),
                );
            }
            "adr0164-agents-section" => {
                self.external_provider_status = ExternalAgentProviderStatus::visual_fixture(
                    ExternalAgentProviderKind::ClaudeCode,
                );
                self.settings_section = SettingsSection::Agents;
                self.settings_open = true;
                self.status = Some(
                    "Agents · one readiness state and one action per agent · discovery, capabilities, and resolved paths are collapsed diagnosis."
                        .to_owned(),
                );
            }
            "adr0164-remote-phone-url" => {
                self.remote_phone_url_base = "https://my-pc.example-tailnet.ts.net".to_owned();
                self.settings_section = SettingsSection::Remote;
                self.settings_open = true;
                self.status = Some(
                    "Remote · one reachable phone URL with a masked token · the loopback gateway is collapsed under Advanced."
                        .to_owned(),
                );
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
        self.request_external_provider_probe_once();
        self.poll_external_provider_probe(context);
        self.poll_acp_question(context);
        self.poll_external_question(context);
        self.poll_external_setup(context);
        self.poll_model_resource_task(context);
        self.poll_native_mcp(context);
        self.poll_native_agent_runtime(context);
        self.retry_external_work_wait_if_ready();
        self.poll_acp_run(context);
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
        self.apply_requested_presentation_toggle();
    }

    /// Moves the studio between its presentations once the frame has drawn.
    ///
    /// ADR 0147 keeps detach and reattach presentation-only operations, and the
    /// control that asks for them now lives in the settings surface the studio
    /// itself draws, so the mode may only change after that drawing is done.
    fn apply_requested_presentation_toggle(&mut self) {
        if !std::mem::take(&mut self.presentation_toggle_requested) {
            return;
        }
        match self.presentation.mode {
            AiStudioPresentationMode::Embedded => self.presentation.detach(),
            AiStudioPresentationMode::Detached => self.presentation.reattach(),
        }
        self.save_preferences();
    }

    fn show_embedded(&mut self, context: &egui::Context) {
        let mut open = self.presentation.open;
        embedded_window(context)
            .open(&mut open)
            .show(context, |ui| self.show_contents(ui));
        self.presentation.open = open;
    }

    fn show_detached(&mut self, context: &egui::Context) {
        let mut close_requested = false;
        context.show_viewport_immediate(
            egui::ViewportId::from_hash_of("gameengine_ai_studio_detached"),
            egui::ViewportBuilder::default()
                .with_title("AI Studio")
                .with_inner_size(DETACHED_DEFAULT_SIZE)
                .with_min_inner_size(DETACHED_MIN_SIZE)
                .with_resizable(true),
            |ui, _class| {
                close_requested = ui.input(|input| input.viewport().close_requested());
                #[cfg(feature = "visual-validation")]
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                // A detached OS window has no Editor chrome behind it, so it
                // paints the studio ground itself.
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0_f32, theme::BACKGROUND);
                self.show_contents(ui);
            },
        );

        #[cfg(feature = "visual-validation")]
        {
            self.detached_visual_frames = self.detached_visual_frames.saturating_add(1);
        }

        if close_requested {
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
            RemoteOperation::Selection => {
                let selection = self.selection_json();
                RemoteAiStudioResponse::json(selection)
            }
            RemoteOperation::SetSelection {
                mode, ai, effort, ..
            } => {
                match self.apply_remote_selection(mode.as_deref(), ai.as_deref(), effort.as_deref())
                {
                    Ok(()) => {
                        let selection = self.selection_json();
                        RemoteAiStudioResponse::json(selection)
                    }
                    Err(error) => {
                        RemoteAiStudioResponse::error(400, "invalid_selection", error, false)
                    }
                }
            }
            RemoteOperation::Snapshot { session_id } => {
                let pending = self
                    .pending_acp_permission
                    .as_ref()
                    .map(|permission| (permission.run_id.as_str(), permission.capability))
                    .or_else(|| {
                        self.pending_permission
                            .as_ref()
                            .map(|permission| (permission.run_id.as_str(), permission.capability))
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
                let approval = match scope {
                    RemotePermissionScope::Once => ApprovalScope::Once,
                    RemotePermissionScope::Run => ApprovalScope::Run,
                    RemotePermissionScope::Project => ApprovalScope::Project,
                    RemotePermissionScope::Deny => ApprovalScope::Deny,
                };
                if let Some(pending) = self.pending_acp_permission.as_ref() {
                    if pending.run_id != run_id || pending.capability != capability {
                        return RemoteAiStudioResponse::error(
                            409,
                            "permission_stale",
                            "The ACP permission request no longer matches the active decision.",
                            false,
                        );
                    }
                    self.resolve_pending_acp_permission(approval);
                    return RemoteAiStudioResponse::json(
                        serde_json::json!({"resolved": true, "run_id": run_id}),
                    );
                }
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
        if let Some(acp_session_id) = self.active_acp_run_session.take() {
            self.pending_acp_permission = None;
            self.active_acp_agent_id = None;
            return self
                .acp
                .cancel_run(&mut self.host, &acp_session_id)
                .map_err(|error| error.to_string());
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

    fn conversation_lane_width(available_width: f32) -> f32 {
        available_width.clamp(0.0, 960.0)
    }

    /// Keeps the header, transcript, and composer in one readable column.
    ///
    /// Wide detached AI Studio windows should not turn a vertical conversation
    /// into full-window controls. Narrow windows keep using all available width.
    /// The transcript opts into the available height so the horizontal centering
    /// row cannot shrink the scroll area to the height of its current entries.
    fn show_conversation_lane<R>(
        ui: &mut egui::Ui,
        fill_available_height: bool,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> R {
        let available_width = ui.available_width();
        let available_height = ui.available_height();
        let lane_width = Self::conversation_lane_width(available_width);
        let gutter = ((available_width - lane_width) * 0.5).max(0.0);
        ui.horizontal(|ui| {
            if fill_available_height {
                ui.set_min_height(available_height);
            }
            ui.add_space(gutter);
            ui.vertical(|ui| {
                ui.set_width(lane_width);
                add_contents(ui)
            })
            .inner
        })
        .inner
    }

    fn show_contents(&mut self, ui: &mut egui::Ui) {
        // Scoped to this Ui and its children, so the surrounding Editor chrome
        // keeps the style installed by `crate::ui::chrome`.
        theme::apply_studio_style(ui);
        Self::show_conversation_lane(ui, false, |ui| self.show_studio_header(ui));
        // ADR 0158 §1: one transcript is the primary surface, with the composer
        // pinned to its lower edge. ADR 0162 §4 narrows what may share that
        // dock to the decisions that block the user, one run status line, and
        // the composer, so the transcript keeps the height ADR 0158 intended.
        egui::Panel::bottom("ai_studio_composer_dock")
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                Self::show_conversation_lane(ui, false, |ui| {
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
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                Self::show_conversation_lane(ui, true, |ui| self.show_transcript(ui));
            });
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
        let mode = self.conversation_mode;
        transcript_scroll.show(ui, |ui| {
            if transcript.entries.is_empty() {
                show_empty_transcript(ui, mode);
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
            theme::status_pill(ui, run_state_tone(span.state), span.state.label());
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
    /// Returns who runs the next message.
    ///
    /// ADR 0164 §1: one value, read by everything that has to know which
    /// executor the composer is displaying.
    fn selected_ai(&self) -> SelectedAi {
        match self.selected_ai_family {
            SelectedAiFamily::Agent => SelectedAi::Agent(self.external_provider_kind),
            SelectedAiFamily::Model => SelectedAi::Model(self.model_backend),
        }
    }

    /// Records who runs the next message.
    ///
    /// This is the only writer of the family and the entry together, so the
    /// two cannot drift into the separate selections ADR 0164 §1 replaced.
    fn select_ai(&mut self, selection: SelectedAi) {
        if self.selected_ai() == selection {
            return;
        }
        match selection {
            SelectedAi::Agent(kind) => {
                self.selected_ai_family = SelectedAiFamily::Agent;
                if self.external_provider_kind != kind {
                    self.external_provider_kind = kind;
                    self.external_provider_status = ExternalAgentProviderStatus::unchecked(kind);
                }
            }
            SelectedAi::Model(backend) => {
                self.selected_ai_family = SelectedAiFamily::Model;
                self.model_backend = backend;
                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
            }
        }
        self.save_preferences();
    }

    /// Returns the name of the selected AI, as the composer displays it.
    fn selected_ai_label(&mut self) -> String {
        match self.selected_ai() {
            SelectedAi::Agent(kind) => kind.label().to_owned(),
            SelectedAi::Model(_) => match self.described_native_model_config() {
                Ok(config) => config.label(),
                Err(_) => self.model_backend.label().to_owned(),
            },
        }
    }

    /// Returns why the selected AI cannot serve the selected mode.
    ///
    /// ADR 0164 §2: the studio states this and refuses to send. It never
    /// answers on a different AI, because the composer would then name one
    /// executor while another performed the work, and no audit of the
    /// transcript could recover which one ran.
    fn selected_ai_unavailable(&mut self) -> Option<String> {
        match self.selected_ai() {
            SelectedAi::Agent(kind) => {
                agent_unavailable_for_mode(kind, self.conversation_mode, self.agent_readiness(kind))
            }
            SelectedAi::Model(ModelBackendPreference::ManagedLocal) => {
                match self.described_managed_model_config() {
                    Err(error) => Some(error),
                    Ok(config) if GooseLocalAcpRuntime::setup_required(&config) => Some(
                        "Managed Local uses Goose over ACP. Goose is not set up on this machine yet; open Settings > Models > Managed Local AI and choose Install Goose."
                            .to_owned(),
                    ),
                    Ok(_) => None,
                }
            }
            SelectedAi::Model(_) => self.described_native_model_config().err(),
        }
    }

    /// Returns the identity the companion uses for the selected AI.
    fn selected_ai_id(&self) -> String {
        ai_entry_id(self.selected_ai(), &self.managed_model_id)
    }

    /// Returns every AI this machine offers, as the companion receives it.
    ///
    /// ADR 0164 §5: entries that already exist, with the readiness the local
    /// composer shows. Nothing here is a way to create one.
    fn ai_entries(&self) -> Vec<serde_json::Value> {
        let mut entries = Vec::new();
        for kind in ExternalAgentProviderKind::ALL {
            entries.push(serde_json::json!({
                "id": ai_entry_id(SelectedAi::Agent(kind), ""),
                "group": "Agents",
                "label": kind.label(),
                "readiness": self.agent_readiness(kind).label(),
            }));
        }
        for model in self
            .managed_local_runtime
            .registered_models()
            .unwrap_or_default()
        {
            entries.push(serde_json::json!({
                "id": ai_entry_id(
                    SelectedAi::Model(ModelBackendPreference::ManagedLocal),
                    &model.model_id,
                ),
                "group": "Local models",
                // A registered GGUF is named by the user and can carry a path,
                // which ADR 0133 §13 keeps out of the remote projection.
                "label": crate::remote_ai_studio::sanitize_text(&model.display_name),
                "readiness": "registered",
            }));
        }
        for backend in ModelBackendPreference::ALL
            .into_iter()
            .filter(|backend| *backend != ModelBackendPreference::ManagedLocal)
        {
            entries.push(serde_json::json!({
                "id": ai_entry_id(SelectedAi::Model(backend), ""),
                "group": match backend.ai_group() {
                    AiGroup::LocalModels => "Local models",
                    AiGroup::Cloud => "Cloud",
                },
                "label": backend.label(),
                "readiness": self.backend_readiness(backend),
            }));
        }
        entries
    }

    /// Returns the composer's three selections and everything selectable.
    fn selection_json(&mut self) -> serde_json::Value {
        let entries = self.ai_entries();
        let selected_ai = self.selected_ai_id();
        let unavailable = self.selected_ai_unavailable();
        serde_json::json!({
            "mode": {
                "selected": self.conversation_mode.remote_id(),
                "entries": ConversationMode::ALL
                    .into_iter()
                    .map(|mode| serde_json::json!({
                        "id": mode.remote_id(),
                        "label": mode.label(),
                        "summary": mode.summary(),
                    }))
                    .collect::<Vec<_>>(),
            },
            "ai": {
                "selected": selected_ai,
                "entries": entries,
            },
            "effort": {
                "selected": effort_id(self.quality_preference),
                "entries": QualityPreference::ALL
                    .into_iter()
                    .map(|quality| serde_json::json!({
                        "id": effort_id(quality),
                        "label": quality.label(),
                    }))
                    .collect::<Vec<_>>(),
            },
            // ADR 0164 §2 applies remotely too: the companion states this and
            // refuses to send rather than substituting another AI.
            "unavailable": unavailable,
        })
    }

    /// Returns which entry an AI identity names, when this machine offers one.
    ///
    /// ADR 0164 §5: an identity that is not in the list the host published is
    /// rejected rather than created, so a remote client cannot register a model
    /// or configure an agent by naming one.
    fn ai_selection_for_id(&self, id: &str) -> Option<(SelectedAi, Option<String>)> {
        for kind in ExternalAgentProviderKind::ALL {
            if ai_entry_id(SelectedAi::Agent(kind), "") == id {
                return Some((SelectedAi::Agent(kind), None));
            }
        }
        for model in self
            .managed_local_runtime
            .registered_models()
            .unwrap_or_default()
        {
            let managed = SelectedAi::Model(ModelBackendPreference::ManagedLocal);
            if ai_entry_id(managed, &model.model_id) == id {
                return Some((managed, Some(model.model_id)));
            }
        }
        for backend in ModelBackendPreference::ALL
            .into_iter()
            .filter(|backend| *backend != ModelBackendPreference::ManagedLocal)
        {
            if ai_entry_id(SelectedAi::Model(backend), "") == id {
                return Some((SelectedAi::Model(backend), None));
            }
        }
        None
    }

    /// Applies a companion selection change to this machine's own state.
    ///
    /// ADR 0164 §6: there is one selection, and every presentation reads and
    /// writes it, so a change made on the phone is the change the PC shows.
    ///
    /// # Errors
    ///
    /// Returns which supplied identity this machine does not offer. Nothing is
    /// applied when any part of the request is rejected.
    fn apply_remote_selection(
        &mut self,
        mode: Option<&str>,
        ai: Option<&str>,
        effort: Option<&str>,
    ) -> Result<(), String> {
        let mode = mode
            .map(|id| {
                ConversationMode::ALL
                    .into_iter()
                    .find(|mode| mode.remote_id() == id)
                    .ok_or_else(|| format!("{id} is not a conversation mode."))
            })
            .transpose()?;
        let effort = effort
            .map(|id| effort_from_id(id).ok_or_else(|| format!("{id} is not an effort level.")))
            .transpose()?;
        let ai = ai
            .map(|id| {
                self.ai_selection_for_id(id)
                    .ok_or_else(|| format!("{id} is not an AI this machine offers."))
            })
            .transpose()?;
        if let Some(mode) = mode {
            self.conversation_mode = mode;
        }
        if let Some(effort) = effort {
            self.quality_preference = effort;
        }
        if let Some((selection, managed_model_id)) = ai {
            if let Some(model_id) = managed_model_id {
                self.managed_model_id = model_id;
                self.last_model_resource_telemetry = ModelResourceTelemetry::default();
            }
            self.select_ai(selection);
        }
        self.save_preferences();
        Ok(())
    }

    fn show_composer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            self.show_mode_selection(ui);
            self.show_ai_selection(ui);
            self.show_effort_selection(ui);
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.message_draft)
                .desired_rows(2)
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

    /// Draws the AI entry of the composer's selection tier.
    ///
    /// ADR 0164 §1: one list is the single place the executor of the next turn
    /// is chosen, grouped by what the reader is choosing between rather than by
    /// which internal path serves the entry. ADR 0162 §5 limits it to entries
    /// that already exist: nothing here installs, registers, authenticates, or
    /// removes anything, and the one action that leaves the tier opens the
    /// configuration section that owns the entry.
    fn show_ai_selection(&mut self, ui: &mut egui::Ui) {
        let selected_label = self.selected_ai_label();
        let managed_models = self
            .managed_local_runtime
            .registered_models()
            .unwrap_or_default();
        let agent_readiness: Vec<_> = ExternalAgentProviderKind::ALL
            .into_iter()
            .map(|kind| (kind, self.agent_readiness(kind)))
            .collect();
        let mut chosen = None;
        let mut open_configuration = None;
        egui::ComboBox::from_id_salt("ai_studio_composer_ai")
            .selected_text(selected_label)
            .width(300.0)
            .show_ui(ui, |ui| {
                theme::caption(ui, "Agents");
                for (kind, readiness) in &agent_readiness {
                    let selected = self.selected_ai() == SelectedAi::Agent(*kind);
                    if ui
                        .selectable_label(
                            selected,
                            format!("{} · {}", kind.label(), readiness.label()),
                        )
                        .clicked()
                    {
                        chosen = Some(SelectedAi::Agent(*kind));
                    }
                }
                ui.separator();
                theme::caption(ui, "Local models");
                if managed_models.is_empty() {
                    theme::hint(ui, "No GGUF is registered on this machine yet.");
                }
                for model in &managed_models {
                    let selected = self.selected_ai()
                        == SelectedAi::Model(ModelBackendPreference::ManagedLocal)
                        && self.managed_model_id == model.model_id;
                    if ui.selectable_label(selected, &model.display_name).clicked() {
                        self.managed_model_id = model.model_id.clone();
                        chosen = Some(SelectedAi::Model(ModelBackendPreference::ManagedLocal));
                    }
                }
                self.show_ai_backend_entries(ui, AiGroup::LocalModels, &mut chosen);
                ui.separator();
                theme::caption(ui, "Cloud");
                self.show_ai_backend_entries(ui, AiGroup::Cloud, &mut chosen);
                ui.separator();
                if ui.button("Set up agents…").clicked() {
                    open_configuration = Some(SettingsSection::Agents);
                }
                if ui.button("Set up models…").clicked() {
                    open_configuration = Some(SettingsSection::Models);
                }
            });
        if let Some(selection) = chosen {
            // The managed model id may have changed without the family or the
            // backend changing, so the preference write is not left to
            // `select_ai` alone.
            self.select_ai(selection);
            self.save_preferences();
        }
        if let Some(section) = open_configuration {
            self.settings_section = section;
            self.settings_open = true;
        }
    }

    /// Draws one group of model-backend entries inside the AI list.
    fn show_ai_backend_entries(
        &mut self,
        ui: &mut egui::Ui,
        group: AiGroup,
        chosen: &mut Option<SelectedAi>,
    ) {
        for backend in ModelBackendPreference::ALL.into_iter().filter(|backend| {
            // Managed Local AI appears above as the models it has registered,
            // so it is not repeated as a backend of its own.
            backend.ai_group() == group && *backend != ModelBackendPreference::ManagedLocal
        }) {
            let selected = self.selected_ai() == SelectedAi::Model(backend);
            if ui
                .selectable_label(
                    selected,
                    format!("{} · {}", backend.label(), self.backend_readiness(backend)),
                )
                .clicked()
            {
                *chosen = Some(SelectedAi::Model(backend));
            }
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
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_id_salt("ai_studio_session")
                    .selected_text(current_title.clone())
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
                                self.session_title_draft = None;
                                self.session_delete_confirmation = None;
                            }
                        }
                    });
                if ui.button("New session").clicked() {
                    match self.host.create_session("New AI Studio session") {
                        Ok(id) => {
                            self.selected_session = id;
                            self.proposal_draft = AgentProposal::default();
                            self.session_title_draft =
                                Some("New AI Studio session".to_owned());
                            self.session_delete_confirmation = None;
                            self.status = Some("Created a private local AI session.".to_owned());
                        }
                        Err(error) => self.status = Some(error.to_string()),
                    }
                }
                let has_session = self.host.session(&self.selected_session).is_ok();
                if ui
                    .add_enabled(has_session, egui::Button::new("Rename"))
                    .clicked()
                {
                    self.session_title_draft = Some(current_title.clone());
                    self.session_delete_confirmation = None;
                }
                if ui
                    .add_enabled(has_session, egui::Button::new("Delete"))
                    .clicked()
                {
                    self.session_delete_confirmation = Some(self.selected_session.clone());
                    self.session_title_draft = None;
                }
                if ui
                    .add_enabled(has_session, egui::Button::new("Share with project"))
                    .clicked()
                {
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

            let mut rename_to = None;
            let mut cancel_rename = false;
            if let Some(title_draft) = self.session_title_draft.as_mut() {
                ui.horizontal(|ui| {
                    ui.label("Session name");
                    ui.add(egui::TextEdit::singleline(title_draft).desired_width(260.0));
                    let can_save = !title_draft.trim().is_empty();
                    if ui
                        .add_enabled(can_save, egui::Button::new("Save"))
                        .clicked()
                    {
                        rename_to = Some(title_draft.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        cancel_rename = true;
                    }
                });
            }
            if cancel_rename {
                self.session_title_draft = None;
            }
            if let Some(title) = rename_to {
                match self.host.rename_session(&self.selected_session, title) {
                    Ok(()) => {
                        self.session_title_draft = None;
                        self.status = Some("Renamed AI session.".to_owned());
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }

            let delete_is_pending = self
                .session_delete_confirmation
                .as_deref()
                .is_some_and(|id| id == self.selected_session);
            if delete_is_pending {
                let (shared_with_project, has_active_run) = self
                    .host
                    .session(&self.selected_session)
                    .map(|session| {
                        (
                            session.shared_with_project,
                            session.runs.iter().any(|run| !run.state.is_terminal()),
                        )
                    })
                    .unwrap_or((false, false));
                let question_busy = (self.native_question.is_some()
                    && self.native_question_session.as_deref()
                        == Some(self.selected_session.as_str()))
                    || (self.external_question.is_some()
                        && self.external_question_session.as_deref()
                            == Some(self.selected_session.as_str()))
                    || self
                        .pending_question_permission
                        .as_ref()
                        .is_some_and(|pending| pending.session_id == self.selected_session);
                let delete_block_reason = if has_active_run {
                    Some("Stop the session's active run before deleting it.")
                } else if question_busy {
                    Some("Stop the in-progress answer before deleting this session.")
                } else {
                    None
                };
                let mut confirm_delete = false;
                let mut cancel_delete = false;
                ui.group(|ui| {
                    if shared_with_project {
                        ui.label(
                            "Delete this session from this machine? Its local conversation, code workspace, and artifacts will be removed. The project-shared history is kept.",
                        );
                    } else {
                        ui.label(
                            "Delete this session from this machine? Its local conversation, code workspace, and artifacts will be removed.",
                        );
                    }
                    if let Some(reason) = delete_block_reason {
                        ui.small(reason);
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                delete_block_reason.is_none(),
                                egui::Button::new("Delete permanently"),
                            )
                            .clicked()
                        {
                            confirm_delete = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel_delete = true;
                        }
                    });
                });
                if cancel_delete {
                    self.session_delete_confirmation = None;
                }
                if confirm_delete {
                    let deleted_session = self.selected_session.clone();
                    let deleted_run_ids = self
                        .host
                        .session(&deleted_session)
                        .map(|session| {
                            session
                                .runs
                                .iter()
                                .map(|run| run.id.clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    match self.host.delete_session(&deleted_session) {
                        Ok(()) => {
                            let deleted_active_run =
                                self.active_run_id.as_ref().is_some_and(|active_run_id| {
                                    deleted_run_ids
                                        .iter()
                                        .any(|run_id| run_id == active_run_id)
                                });
                            if deleted_active_run {
                                self.active_run_id = None;
                                self.active_runtime_mode = None;
                                self.active_external_provider = None;
                                self.active_external_program = None;
                                self.active_external_args = None;
                                self.native_agent_runtime = None;
                                self.native_mcp_task = None;
                                self.pending_native_mcp_tool = None;
                                self.process = None;
                                self.process_purpose = None;
                                self.code_workspace = None;
                                self.pending_code_changes.clear();
                                self.pending_permission = None;
                                self.pending_runtime_action = None;
                                self.managed_runtime_observation = None;
                                self.managed_runtime_debug_observation = None;
                                self.managed_playtest_started_at = None;
                                self.last_captured_frame = None;
                            }
                            self.session_delete_confirmation = None;
                            self.session_title_draft = None;
                            self.message_draft.clear();
                            self.deferred_intent = None;

                            let (replacement, created_replacement) =
                                match self.host.session_ids().into_iter().next_back() {
                                    Some(id) => (Ok(id), false),
                                    None => (
                                        self.host.create_session("New AI Studio session"),
                                        true,
                                    ),
                                };
                            match replacement {
                                Ok(id) => {
                                    self.selected_session = id.clone();
                                    self.proposal_draft = self
                                        .host
                                        .session(&id)
                                        .map(|session| session.proposal.clone())
                                        .unwrap_or_default();
                                    self.session_title_draft = created_replacement
                                        .then(|| "New AI Studio session".to_owned());
                                    self.status = Some(if created_replacement {
                                        "Deleted the local AI session and created a new empty session."
                                            .to_owned()
                                    } else {
                                        "Deleted the local AI session and its local artifacts."
                                            .to_owned()
                                    });
                                }
                                Err(error) => {
                                    self.status = Some(format!(
                                        "Deleted the session, but could not create a replacement: {error}"
                                    ));
                                }
                            }
                        }
                        Err(error) => self.status = Some(error.to_string()),
                    }
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
        let commit_blocked = self.send_blocked();
        let can_send = !self.message_draft.trim().is_empty()
            && self.native_question.is_none()
            && self.external_question.is_none()
            && self.pending_question_permission.is_none()
            && commit_blocked.is_none();
        let mut stop_answer_requested = false;
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
            if let Some(kind) = self
                .external_question
                .as_ref()
                .map(ExternalAgentQuestionTask::kind)
            {
                ui.spinner();
                ui.small(format!(
                    "{} is reading the project read-only…",
                    kind.label()
                ));
                if ui.button("Stop answering").clicked() {
                    stop_answer_requested = true;
                }
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
                    if let SelectedAi::Agent(kind) = self.selected_ai() {
                        ui.small(format!("Answered by {}.", kind.label()));
                    }
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
        if stop_answer_requested {
            self.cancel_external_question();
        }
    }

    /// Stops a provider-served answer without recording a transcript entry.
    ///
    /// The user asked for the answer to stop, so the terminated process is not
    /// reported as a provider failure.
    fn cancel_external_question(&mut self) {
        if let Some(task) = self.external_question.take() {
            task.cancel();
            self.status = Some(format!("Stopped the {} answer.", task.kind().label()));
        }
        self.external_question_session = None;
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
    fn intent_commit_blocked(&mut self) -> Option<String> {
        if self.process.is_some() || self.native_runtime_busy() {
            return Some("An agent process is already running.".to_owned());
        }
        if self.external_question.is_some() {
            return Some("The selected agent is answering a question.".to_owned());
        }
        if self.pending_permission.is_some() || self.pending_question_permission.is_some() {
            return Some("Resolve the pending approval first.".to_owned());
        }
        self.selected_ai_unavailable()
    }

    /// Returns why the composer cannot submit the draft right now.
    ///
    /// ADR 0164 §2: the reason is stated in one line at the composer and Send
    /// refuses. The same predicate guards submission itself, so no other path
    /// can answer a turn on an AI the composer is not displaying.
    fn send_blocked(&mut self) -> Option<String> {
        if self.native_run_awaits_user() {
            return None;
        }
        match self.conversation_mode {
            ConversationMode::Ask => self.selected_ai_unavailable(),
            // ADR 0162 §3 records this instruction for the next run rather than
            // executing it now, so the selection is checked when that run
            // starts and not while the previous one is still executing.
            ConversationMode::Build if self.run_is_active() => None,
            ConversationMode::Build => self.intent_commit_blocked(),
        }
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
        // ADR 0164 §2: the draft is kept rather than recorded, so changing the
        // selection and sending the same text is one interaction.
        if let Some(reason) = self.send_blocked() {
            self.status = Some(reason);
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
            ConversationMode::Ask => self.start_question(),
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
    /// The submitted instruction becomes both the goal and a deterministic
    /// minimum structured plan. Explicitly edited draft fields are preserved;
    /// an entirely empty draft receives a language-independent Build scope so
    /// the provider can inspect the project and determine the concrete files,
    /// authoring operations, assets, and runtime checks from the user's goal.
    fn derive_intent_proposal(&mut self, message: &str) {
        self.proposal_draft.goal = message.to_owned();
        if !self
            .proposal_draft
            .requirements
            .iter()
            .any(|item| item == message)
        {
            self.proposal_draft.requirements.push(message.to_owned());
        }
        if self.proposal_draft.acceptance_criteria.is_empty() {
            self.proposal_draft.acceptance_criteria.push(
                "The submitted request is completed without changes outside its stated scope."
                    .to_owned(),
            );
        }

        ensure_build_scope(&mut self.proposal_draft);
        if build_executor_requires_external_process(self.selected_ai()) {
            self.proposal_draft
                .requested_capabilities
                .insert(AgentCapability::ExternalAgentProcess);
        }
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
            Ok(ManagedSetupResult::GooseInstalled(installation)) => {
                self.status = Some(format!(
                    "Managed Local ACP runtime ready: Goose {} · verified {}.",
                    installation.version, installation.asset_sha256
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

    /// Answers the submitted Ask turn with whichever runtime is selected.
    ///
    /// First-class external agents route through ACP. A route selected for ACP
    /// never silently falls back to the legacy provider harness when its adapter
    /// is unavailable; explicit Legacy routes and model backends retain their
    /// existing behavior.
    fn start_question(&mut self) {
        let driver = match self.selected_execution_driver() {
            Ok(driver) => driver,
            Err(error) => {
                self.status = Some(error);
                return;
            }
        };
        match driver {
            AiExecutionDriver::Acp { agent_id } => self.start_acp_question(&agent_id),
            AiExecutionDriver::Legacy => {
                if self.provider_answers_questions() {
                    self.start_external_question();
                } else {
                    self.start_native_question();
                }
            }
        }
    }

    fn selected_execution_driver(&mut self) -> Result<AiExecutionDriver, String> {
        let selection = self.selected_ai();
        if self.benchmark_child_active()
            && matches!(
                selection,
                SelectedAi::Model(ModelBackendPreference::ManagedLocal)
            )
            && !self.benchmark_child_uses_acp_runtime()
        {
            // Legacy campaign plans continue to measure the existing Native harness.
            // Only an explicit schema-v3 agent-inclusive runtime identity may switch a
            // benchmark child onto Goose ACP.
            return Ok(AiExecutionDriver::Legacy);
        }
        let logical_ai_id = ai_entry_id(selection, &self.managed_model_id);
        let route_key = match selection {
            SelectedAi::Model(ModelBackendPreference::ManagedLocal) => {
                "model:managed_local".to_owned()
            }
            _ => logical_ai_id.clone(),
        };
        let configured_agent = match selection {
            SelectedAi::Agent(ExternalAgentProviderKind::Codex) => Some(CODEX_ACP_DESCRIPTOR_ID),
            SelectedAi::Agent(ExternalAgentProviderKind::ClaudeCode) => Some(CLAUDE_ACP_AGENT_ID),
            SelectedAi::Model(ModelBackendPreference::ManagedLocal) => {
                Some(GOOSE_LOCAL_ACP_DESCRIPTOR_ID)
            }
            _ => None,
        };
        if let Some(agent_id) = configured_agent {
            self.ensure_acp_runtime(agent_id)?;
        }
        self.execution_router.sync_registry(self.acp.registry());
        self.execution_router
            .resolve(logical_ai_id, route_key)
            .map(|resolution| resolution.driver)
            .map_err(|error| error.to_string())
    }

    fn ensure_acp_runtime(&mut self, agent_id: &str) -> Result<(), String> {
        let placement = self.external_agent_placement();
        match agent_id {
            CODEX_ACP_DESCRIPTOR_ID => {
                let runtime = CodexAcpRuntime::discover(
                    placement,
                    CodexAcpSessionPreferences {
                        model: None,
                        reasoning_effort: Self::codex_reasoning_effort(self.quality_preference)
                            .map(str::to_owned),
                        fast_mode: None,
                    },
                )
                .map_err(|error| error.to_string())?;
                self.acp
                    .replace(Box::new(runtime))
                    .map_err(|error| error.to_string())
            }
            CLAUDE_ACP_AGENT_ID => {
                let registration = discover_claude_acp(&ClaudeAcpConfig::default(), &placement)
                    .map_err(|error| error.to_string())?;
                let runtime = AcpProcessRuntime::new(registration.descriptor)
                    .map_err(|error| error.to_string())?;
                self.acp
                    .replace(Box::new(runtime))
                    .map_err(|error| error.to_string())
            }
            GOOSE_LOCAL_ACP_DESCRIPTOR_ID => {
                let managed_model = self.described_managed_model_config()?;
                let config =
                    GooseLocalAcpConfig::new(managed_model).map_err(|error| error.to_string())?;
                let runtime = match GooseLocalAcpRuntime::discover(config) {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        self.settings_open = true;
                        self.settings_section = SettingsSection::Models;
                        let message = format!(
                            "Managed Local ACP cannot start until Goose is ready. Use Install Goose in Settings > Models > Managed Local AI, then retry. {error}"
                        );
                        self.status = Some(message.clone());
                        return Err(message);
                    }
                };
                self.acp
                    .replace(Box::new(runtime))
                    .map_err(|error| error.to_string())
            }
            _ => Err(format!(
                "ACP agent `{agent_id}` is not configured by AI Studio"
            )),
        }
    }

    fn codex_reasoning_effort(quality: QualityPreference) -> Option<&'static str> {
        match quality {
            QualityPreference::Auto => None,
            QualityPreference::Fast => Some("low"),
            QualityPreference::Balanced => Some("medium"),
            QualityPreference::Deep => Some("high"),
        }
    }

    fn start_acp_question(&mut self, agent_id: &str) {
        let session_id = self.selected_session.clone();
        let turns = match self.host.session(&session_id) {
            Ok(session) => session
                .messages
                .iter()
                .map(|message| ExternalAgentQuestionTurn {
                    role: match message.role {
                        ConversationRole::User => ExternalAgentQuestionRole::User,
                        ConversationRole::Assistant => ExternalAgentQuestionRole::Assistant,
                        ConversationRole::System => ExternalAgentQuestionRole::System,
                    },
                    text: message.text.clone(),
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                self.status = Some(error.to_string());
                return;
            }
        };
        let prompt = build_question_prompt(&turns);
        let acp_session_id = match self.acp.open_ask_session(&self.host, agent_id, &session_id) {
            Ok(session_id) => session_id,
            Err(error) => {
                self.status = Some(format!("Could not open ACP Ask session: {error}"));
                return;
            }
        };
        if let Err(error) = self
            .acp
            .send_prompt(&mut self.host, &acp_session_id, &prompt)
        {
            let _ = self.acp.close_session(&acp_session_id);
            self.status = Some(format!("Could not send ACP Ask prompt: {error}"));
            return;
        }
        self.acp_question = Some(AcpQuestionState {
            acp_session_id,
            gameengine_session_id: session_id,
            answer: String::new(),
            started_at: std::time::Instant::now(),
        });
        self.status = Some(format!(
            "{} is answering through the common ACP runtime with read-only Editor MCP authority.",
            self.external_provider_kind.label()
        ));
    }

    fn poll_acp_question(&mut self, context: &egui::Context) {
        let Some(acp_session_id) = self
            .acp_question
            .as_ref()
            .map(|question| question.acp_session_id.clone())
        else {
            return;
        };
        let poll = match self.acp.poll(&mut self.host, &acp_session_id) {
            Ok(poll) => poll,
            Err(error) => {
                let question = self.acp_question.take();
                let _ = self.acp.close_session(&acp_session_id);
                let diagnostic = format!("ACP Ask failed: {error}");
                self.status = Some(diagnostic.clone());
                if let Some(question) = question {
                    let _ = self.host.append_message(
                        &question.gameengine_session_id,
                        ConversationRole::System,
                        diagnostic,
                    );
                }
                return;
            }
        };
        match poll {
            AcpBridgePoll::Idle => {
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::AgentMessage { text }) => {
                if let Some(question) = self.acp_question.as_mut() {
                    question.answer.push_str(&text);
                }
                context.request_repaint_after(std::time::Duration::from_millis(16));
            }
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::Progress { detail, .. }) => {
                self.status = Some(detail);
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::Plan { entries }) => {
                self.status = Some(format!("ACP Ask plan: {}", entries.join("; ")));
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::ToolCall { title, status, .. }) => {
                self.status = Some(format!("ACP Ask tool `{title}` is {status:?}."));
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::SessionInfo { title }) => {
                if let Some(title) = title {
                    self.status = Some(format!("ACP Ask session: {title}"));
                }
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::ProtocolDiagnostic { message }) => {
                self.status = Some(message);
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::TurnFinished { stop_reason }) => {
                let Some(question) = self.acp_question.take() else {
                    return;
                };
                let elapsed_ms = question.started_at.elapsed().as_millis();
                let answer = question.answer.trim().to_owned();
                let close_result = self.acp.close_session(&acp_session_id);
                if answer.is_empty() {
                    self.status = Some(format!(
                        "ACP Ask returned control with `{stop_reason}` but produced no assistant message."
                    ));
                    return;
                }
                if let Err(error) = self.host.append_message(
                    &question.gameengine_session_id,
                    ConversationRole::Assistant,
                    answer,
                ) {
                    self.status = Some(error.to_string());
                    return;
                }
                if let Err(error) = close_result {
                    self.status = Some(format!(
                        "ACP Ask answered but session close failed: {error}"
                    ));
                } else {
                    self.status = Some(format!(
                        "{} answered read-only through ACP in {elapsed_ms} ms.",
                        self.external_provider_kind.label()
                    ));
                }
            }
            AcpBridgePoll::AskEvent(AcpNormalizedEvent::PermissionRequest(_)) => {
                self.status = Some(
                    "ACP Ask requested permission after the bridge should have denied it; the session remains read-only."
                        .to_owned(),
                );
            }
            AcpBridgePoll::Recorded { .. }
            | AcpBridgePoll::RecordedEvent { .. }
            | AcpBridgePoll::PermissionRequired { .. }
            | AcpBridgePoll::ValidationReady { .. }
            | AcpBridgePoll::TurnFailed { .. } => {
                self.status = Some(
                    "ACP Ask received an unexpected run-bound bridge result and was not promoted to write authority."
                        .to_owned(),
                );
            }
        }
    }

    /// Whether the selected agent is ready to answer this Ask turn.
    fn provider_answers_questions(&self) -> bool {
        ask_is_agent_served(self.selected_ai(), &self.current_external_provider_status())
    }

    /// Starts the read-only provider process that answers one Ask turn.
    ///
    /// ADR 0163 §1: the answer never enters the run lifecycle. It takes no work
    /// claim and prepares no code workspace. It receives the Editor's separate
    /// read-only MCP credential so unsaved authoritative state is visible while
    /// mutation remains impossible at the transport boundary.
    fn start_external_question(&mut self) {
        let kind = self.external_provider_kind;
        let session_id = self.selected_session.clone();
        let turns = match self.host.session(&session_id) {
            Ok(session) => session
                .messages
                .iter()
                .map(|message| ExternalAgentQuestionTurn {
                    role: match message.role {
                        ConversationRole::User => ExternalAgentQuestionRole::User,
                        ConversationRole::Assistant => ExternalAgentQuestionRole::Assistant,
                        ConversationRole::System => ExternalAgentQuestionRole::System,
                    },
                    text: message.text.clone(),
                })
                .collect::<Vec<_>>(),
            Err(error) => {
                self.status = Some(error.to_string());
                return;
            }
        };
        let prompt = build_question_prompt(&turns);
        let placement = self.external_agent_placement();
        let plan = match build_question_launch_plan(
            kind,
            &placement,
            &prompt,
            &self.connection.endpoint,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.status = Some(error);
                return;
            }
        };
        let mut environment = vec![(
            OsString::from("GAMEENGINE_MCP_AUTH_TOKEN"),
            OsString::from(&self.connection.read_only_authorization_token),
        )];
        if placement.environment == ExternalAgentExecutionEnvironment::Wsl2Linux {
            environment.push(wsl_environment_forwarding(&environment, &[]));
        }
        match ExternalAgentQuestionTask::spawn(kind, plan, self.project_root.clone(), environment) {
            Ok(task) => {
                self.external_question = Some(task);
                self.external_question_session = Some(session_id);
                self.status = Some(format!(
                    "{} is answering with its own provider account; no GameEngine model credential is used.",
                    kind.label()
                ));
            }
            Err(error) => self.status = Some(error),
        }
    }

    /// Records a provider-served answer on the session it belongs to.
    fn poll_external_question(&mut self, context: &egui::Context) {
        let Some(task) = self.external_question.as_ref() else {
            return;
        };
        let Some(result) = task.poll() else {
            context.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let kind = task.kind();
        self.external_question = None;
        let session_id = self
            .external_question_session
            .take()
            .unwrap_or_else(|| self.selected_session.clone());
        match result {
            Ok(answer) => {
                match self.host.append_message(
                    &session_id,
                    ConversationRole::Assistant,
                    answer.text.clone(),
                ) {
                    Ok(()) => {
                        self.status = Some(format!(
                            "{} answered read-only in {} ms.",
                            kind.label(),
                            answer.elapsed_ms
                        ));
                    }
                    Err(error) => self.status = Some(error.to_string()),
                }
            }
            Err(error) => {
                let diagnostic = format!("{} could not answer: {error}", kind.label());
                self.status = Some(diagnostic.clone());
                if let Err(append_error) =
                    self.host
                        .append_message(&session_id, ConversationRole::System, diagnostic)
                {
                    self.status = Some(format!(
                        "{} could not answer: {error}; Conversation diagnostic could not be recorded: {append_error}",
                        kind.label()
                    ));
                }
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
        let task = if self.benchmark_child_active() {
            NativeQuestionTask::spawn_with_sampling(
                config,
                self.project_root.clone(),
                conversation,
                NativeSamplingOptions::deterministic_greedy(),
            )
        } else {
            NativeQuestionTask::spawn(config, self.project_root.clone(), conversation)
        };
        match task {
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
        if matches!(
            continuation.as_ref(),
            Some(ModelResourceContinuation::BenchmarkProfilePrepared)
        ) {
            match result {
                Ok(transition) => {
                    self.last_model_resource_telemetry = transition.after;
                    if let Some(child) = self.benchmark_child.as_mut() {
                        child.profile_prepared = true;
                    }
                    self.status = Some(format!(
                        "Verified benchmark model resource boundary {:?} in {} ms.",
                        transition.operation,
                        telemetry_u64_value(&transition.operation_latency_ms)
                    ));
                }
                Err(error) => self.write_benchmark_child_failure(
                    BenchmarkRunFailureKind::CapabilityUnavailable,
                    format!("benchmark execution profile could not be established: {error}"),
                ),
            }
            return;
        }
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
            ModelResourceContinuation::BenchmarkProfilePrepared => {
                unreachable!("benchmark continuation is handled by the resource poller")
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
            ask_uses_external_provider: default_ask_uses_external_provider(),
            selected_ai_family: Some(self.selected_ai_family),
            model_backend: self.model_backend,
            managed_execution_environment: self.managed_execution_environment,
            managed_model_id: self.managed_model_id.clone(),
            local_model_endpoint: self.local_model_endpoint.clone(),
            local_model_name: self.local_model_name.clone(),
            hosted_model_endpoint: self.hosted_model_endpoint.clone(),
            hosted_model_name: self.hosted_model_name.clone(),
            presentation_mode: self.presentation.mode,
            remote_phone_url_base: self.remote_phone_url_base.clone(),
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
                "Prepared isolated managed code workspace for AgentRuntime execution.",
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
            NativeAgentAction::CodeList { path } => {
                let result = self
                    .code_workspace
                    .as_ref()
                    .ok_or_else(|| "Managed code workspace is unavailable.".to_owned())
                    .and_then(|workspace| {
                        workspace
                            .list_files(PathBuf::from(&path).as_path())
                            .map_err(|error| error.to_string())
                    });
                let success = result.is_ok();
                let message = result
                    .and_then(|paths| {
                        serde_json::to_string(
                            &paths
                                .iter()
                                .map(|path| path.to_string_lossy().replace('\\', "/"))
                                .collect::<Vec<_>>(),
                        )
                        .map_err(|error| error.to_string())
                    })
                    .unwrap_or_else(|error| error);
                let _ = self.host.record_tool_action(
                    run_id,
                    "workspace.code_list",
                    path.clone(),
                    Some(success),
                );
                self.record_native_result_and_continue(
                    run_id,
                    format!("code_list:{path}"),
                    success,
                    message,
                );
            }
            NativeAgentAction::CodeRead { path } => {
                let result = self
                    .code_workspace
                    .as_ref()
                    .ok_or_else(|| "Managed code workspace is unavailable.".to_owned())
                    .and_then(|workspace| {
                        workspace
                            .read_text(PathBuf::from(&path).as_path())
                            .map_err(|error| error.to_string())
                    });
                let success = result.is_ok();
                let message = result.unwrap_or_else(|error| error);
                let _ = self.host.record_tool_action(
                    run_id,
                    "workspace.code_read",
                    path.clone(),
                    Some(success),
                );
                self.record_native_result_and_continue(
                    run_id,
                    format!("code_read:{path}"),
                    success,
                    message,
                );
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
                // A benchmark child has no human operator who can notice that
                // the model skipped the required inspection and still asked
                // the host to validate. Keep this task-specific evidence
                // check at the handoff boundary; rejected actions use the
                // normal native retry path below and therefore give the model
                // a chance to perform the missing operations.
                if self.benchmark_child_active() {
                    let readiness = self
                        .host
                        .run(run_id)
                        .map_err(|error| error.to_string())
                        .and_then(|run| self.validate_benchmark_ready_for_validation(run));
                    if let Err(error) = readiness {
                        // This is a rejected control-flow request, not a
                        // failed project tool call. Recording it as a failed
                        // ToolAction would permanently make a later recovered
                        // inspection fail the benchmark's zero-invalid-tool
                        // criterion. Keep the diagnostic in semantic progress
                        // while the runtime's internal failure budget still
                        // limits repeated policy violations.
                        let _ = self.host.record_semantic_progress(
                            run_id,
                            "native.policy",
                            format!("ready_for_validation rejected: {error}"),
                        );
                        self.record_native_result_and_continue(
                            run_id,
                            "ready_for_validation",
                            false,
                            error,
                        );
                        return;
                    }
                }
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

    /// Probes providers once per Editor session, before anything needs them.
    ///
    /// ADR 0163 §3: the first thing a user needs is to know whether a provider
    /// they already signed into is usable, so the probe does not wait for the
    /// settings surface to be opened.
    fn request_external_provider_probe_once(&mut self) {
        if self.external_provider_probe_requested {
            return;
        }
        // A benchmark child executes the backend its campaign froze and has no
        // settings surface to report provider status on. Probing would only run
        // provider processes beside the measured run.
        if self.benchmark_child_active() {
            return;
        }
        self.external_provider_probe_requested = true;
        self.begin_external_provider_probe();
    }

    /// Starts the background probe of every first-class provider.
    ///
    /// ADR 0163 §3: discovery and authentication both run provider processes,
    /// so they never run on the UI thread. One probe is in flight at a time.
    pub(super) fn begin_external_provider_probe(&mut self) {
        if self.external_provider_probe.is_some() {
            return;
        }
        match ExternalAgentProbeTask::spawn(self.external_agent_placement()) {
            Ok(task) => self.external_provider_probe = Some(task),
            Err(error) => self.status = Some(error),
        }
    }

    /// Applies a finished provider probe.
    ///
    /// ADR 0163 §3: a detected, signed-in provider is adopted as the selection
    /// only while nothing else has been configured, so an explicit choice is
    /// never overwritten.
    fn poll_external_provider_probe(&mut self, context: &egui::Context) {
        let Some(task) = self.external_provider_probe.as_ref() else {
            return;
        };
        let Some(reports) = task.poll() else {
            context.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        };
        self.external_provider_probe = None;
        self.external_provider_probe_results = reports;
        if let Some(report) = self
            .external_provider_probe_results
            .iter()
            .find(|report| report.status.kind == self.external_provider_kind)
        {
            self.external_provider_status = report.status.clone();
        }
        if self.external_provider_adoption_done {
            return;
        }
        self.external_provider_adoption_done = true;
        // A campaign froze which executor produces this child's evidence, which
        // outranks any detection: adopting a signed-in provider here would swap
        // the measured backend for whatever happens to be installed, and the
        // child's managed model is still describing itself when probes land.
        if self.benchmark_child_active() {
            return;
        }
        // ADR 0164 §1 made this one selection, so a usable model is now
        // something that has been configured: seeding over it would overwrite
        // an explicit choice, which ADR 0163 §3 does not permit.
        let nothing_configured = self.external_provider_kind == ExternalAgentProviderKind::Generic
            && self.provider_program.trim().is_empty()
            && self.described_native_model_config().is_err();
        if !nothing_configured {
            return;
        }
        let Some(detected) = self
            .external_provider_probe_results
            .iter()
            .map(|report| &report.status)
            .find(|status| status.ready())
            .cloned()
        else {
            return;
        };
        self.select_ai(SelectedAi::Agent(detected.kind));
        self.external_provider_status = detected.clone();
        self.save_preferences();
        self.status = Some(format!(
            "Detected a signed-in {} on this machine and selected it as the AI.",
            detected.kind.label()
        ));
    }

    /// Returns the probe report for one provider, when one has been produced.
    pub(super) fn external_provider_report(
        &self,
        kind: ExternalAgentProviderKind,
    ) -> Option<&ExternalAgentProviderReport> {
        self.external_provider_probe_results
            .iter()
            .find(|report| report.status.kind == kind)
    }

    /// Starts one provider setup step from the Editor.
    ///
    /// ADR 0163 §2 and §4: install and sign-in are both provider-owned work.
    /// GameEngine starts the provider's own command so the setup path does not
    /// leave the Editor, and owns neither the installed artifact nor the
    /// credential the provider stores.
    ///
    /// ADR 0164 §3 lists every agent in one section, so the step names the
    /// agent it sets up rather than inheriting whichever one is selected.
    pub(super) fn begin_external_provider_setup(
        &mut self,
        kind: ExternalAgentProviderKind,
        action: ExternalAgentSetupAction,
    ) {
        if self.external_setup.is_some() {
            return;
        }
        self.external_setup_log.clear();
        self.external_setup_input.clear();
        self.external_sign_in_url = None;
        match ExternalAgentSetupTask::spawn(
            kind,
            action,
            &self.external_agent_placement(),
            &self.project_root,
        ) {
            Ok(task) => {
                self.external_setup = Some(task);
                self.status = Some(match action {
                    ExternalAgentSetupAction::Install => format!(
                        "Installing or updating {}. This runs the provider's own package installer.",
                        kind.label()
                    ),
                    ExternalAgentSetupAction::SignIn => format!(
                        "{} sign-in started. Complete it in the browser window the provider opens.",
                        kind.label()
                    ),
                });
            }
            Err(error) => self.status = Some(error),
        }
    }

    /// Stops a setup step that has not finished.
    pub(super) fn cancel_external_provider_setup(&mut self) {
        let Some(mut task) = self.external_setup.take() else {
            return;
        };
        task.cancel();
        self.status = Some(format!("{} was cancelled.", task.action().progress_label()));
    }

    /// Sends the settings input field to the provider-owned sign-in process.
    pub(super) fn submit_external_setup_input(&mut self) {
        let input = self.external_setup_input.trim().to_owned();
        if input.is_empty() {
            return;
        }
        let Some(task) = self.external_setup.as_mut() else {
            self.status = Some("No provider sign-in is waiting for input.".to_owned());
            return;
        };
        match task.send_input(&input) {
            Ok(()) => {
                self.external_setup_input.clear();
                self.status = Some("Sent input to the provider sign-in process.".to_owned());
            }
            Err(error) => self.status = Some(error),
        }
    }

    /// Relays setup output and re-probes the provider once a step finishes.
    fn poll_external_setup(&mut self, context: &egui::Context) {
        let Some(task) = self.external_setup.as_mut() else {
            return;
        };
        let kind = task.kind();
        let action = task.action();
        for line in task.drain_output() {
            if action == ExternalAgentSetupAction::SignIn
                && self.external_sign_in_url.is_none()
                && let Some(url) = sign_in_url(&line)
            {
                self.external_sign_in_url = Some(url);
            }
            self.external_setup_log.push(line);
            if self.external_setup_log.len() > MAX_SETUP_LOG_LINES {
                self.external_setup_log.remove(0);
            }
        }
        let exit = match task.poll_exit() {
            Ok(exit) => exit,
            Err(error) => {
                self.external_setup = None;
                self.status = Some(error);
                return;
            }
        };
        let Some(status) = exit else {
            context.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        };
        self.external_setup = None;
        self.status = Some(match (action, status.success()) {
            (ExternalAgentSetupAction::Install, true) => format!(
                "{} was installed or updated. Re-checking provider status…",
                kind.label()
            ),
            (ExternalAgentSetupAction::Install, false) => format!(
                "Installing {} did not succeed. The installer output is shown in Settings.",
                kind.label()
            ),
            (ExternalAgentSetupAction::SignIn, true) => format!(
                "{} sign-in finished. Re-checking provider status…",
                kind.label()
            ),
            (ExternalAgentSetupAction::SignIn, false) => format!(
                "{} sign-in did not complete. Provider output is shown in Settings.",
                kind.label()
            ),
        });
        self.begin_external_provider_probe();
    }

    /// Returns the agent a run must use, or `None` when a model was selected.
    ///
    /// ADR 0164 §1: the executing path is derived from the one selection, so a
    /// run can no longer fall through to a `ModelBackend` while the composer
    /// names an agent.
    ///
    /// # Errors
    ///
    /// Returns why the selected agent cannot run, which is the same sentence
    /// §2 states at the composer before Send is pressed.
    fn selected_external_provider(&self) -> Result<Option<ExternalAgentProviderKind>, String> {
        let SelectedAi::Agent(kind) = self.selected_ai() else {
            return Ok(None);
        };
        let status = self.current_external_provider_status();
        if !status.ready() {
            return Err(format!(
                "{} is not ready (discovery: {}, authentication: {}). Set it up under Agents in settings, or choose another AI.",
                kind.label(),
                status.discovery.label(),
                status.auth.label(),
            ));
        }
        Ok(Some(kind))
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
                theme::status_pill(ui, run_state_tone(*state), state.label());
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
        self.cancel_external_question();
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
        let mut acp_cancelled_run = false;
        if let Some(acp_session_id) = self.active_acp_run_session.take() {
            match self.acp.cancel_run(&mut self.host, &acp_session_id) {
                Ok(()) => acp_cancelled_run = true,
                Err(error) => {
                    self.status = Some(format!("Could not stop ACP agent session: {error}"));
                }
            }
        }
        self.pending_acp_permission = None;
        if let Some(process) = self.process.as_mut()
            && let Err(error) = process.cancel()
        {
            self.status = Some(format!("Could not stop agent process: {error}"));
        }
        self.process = None;
        self.process_purpose = None;
        if !acp_cancelled_run
            && let Some(run_id) = run_id
            && let Err(error) = self.host.cancel_run(&run_id)
        {
            self.status = Some(error.to_string());
        }
        self.native_agent_runtime = None;
        self.active_runtime_mode = None;
        self.active_external_provider = None;
        self.active_acp_agent_id = None;
        self.active_external_program = None;
        self.active_external_args = None;
        self.external_provider_diagnostics = ExternalAgentDiagnostics::default();
        self.external_provider_mcp_failure = None;
        self.external_provider_reported_workspace_write = false;
        self.pending_external_work_owner = None;
    }

    fn resolve_pending_acp_permission(&mut self, scope: ApprovalScope) {
        let Some(pending) = self.pending_acp_permission.take() else {
            return;
        };
        match self.acp.resolve_permission(
            &mut self.host,
            &pending.acp_session_id,
            &pending.request_id,
            scope,
        ) {
            Ok(()) => {
                self.status = Some(if scope == ApprovalScope::Deny {
                    format!("Denied ACP permission for {}.", pending.capability.label())
                } else {
                    format!(
                        "Resolved ACP permission for {} through Agent Host.",
                        pending.capability.label()
                    )
                });
            }
            Err(error) => {
                self.status = Some(format!(
                    "Could not resolve ACP permission for run `{}`: {error}",
                    pending.run_id
                ));
            }
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
        if let Some(pending) = self.pending_acp_permission.as_ref() {
            let capability = pending.capability;
            ui.separator();
            ui.group(|ui| {
                ui.strong("ACP approval required");
                ui.label(capability.label());
                ui.small(
                    "The ACP agent requested provider permission. Agent Host remains authoritative for approval scope and the run-bound MCP writer identity.",
                );
                ui.horizontal(|ui| {
                    for (label, scope) in [
                        ("Allow once", ApprovalScope::Once),
                        ("This run", ApprovalScope::Run),
                        ("This project", ApprovalScope::Project),
                        ("Deny", ApprovalScope::Deny),
                    ] {
                        if ui.button(label).clicked() {
                            self.resolve_pending_acp_permission(scope);
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
                && self.pending_acp_permission.is_none()
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
        let execution_driver = self.selected_execution_driver()?;
        let (mode, provider_label, native_config, external_provider, acp_agent_id) =
            match execution_driver {
                AiExecutionDriver::Acp { agent_id } => {
                    let provider = match self.selected_ai() {
                        SelectedAi::Agent(kind) => Some(kind),
                        SelectedAi::Model(_) => None,
                    };
                    (
                        AgentRuntimeMode::External,
                        format!("acp:{agent_id}"),
                        None,
                        provider,
                        Some(agent_id),
                    )
                }
                AiExecutionDriver::Legacy => {
                    let external_provider = self.selected_external_provider()?;
                    if let Some(provider) = external_provider {
                        (
                            AgentRuntimeMode::External,
                            provider.run_label().to_owned(),
                            None,
                            Some(provider),
                            None,
                        )
                    } else {
                        let config = self.selected_native_model_config()?;
                        let label = config.label();
                        (AgentRuntimeMode::Native, label, Some(config), None, None)
                    }
                }
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
        let mut run_benchmark_identity = native_config.as_ref().map(|config| {
            let inventory = match config {
                NativeModelConfig::Local(_) => self.current_installed_inventory().cloned(),
                NativeModelConfig::Managed(config) => {
                    Some(self.managed_benchmark_inventory(config))
                }
                NativeModelConfig::Hosted(_) => None,
            };
            (config.backend_id().to_owned(), config.model_id(), inventory)
        });
        if run_benchmark_identity.is_none()
            && acp_agent_id.as_deref() == Some(GOOSE_LOCAL_ACP_DESCRIPTOR_ID)
            && self.benchmark_child_uses_acp_runtime()
        {
            let config = self.described_managed_model_config()?;
            run_benchmark_identity = Some((
                MANAGED_BACKEND_ID.to_owned(),
                config.model_id.clone(),
                Some(self.managed_benchmark_inventory(&config)),
            ));
        }
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
        self.active_acp_agent_id = acp_agent_id;
        self.active_acp_run_session = None;
        self.pending_acp_permission = None;
        self.active_external_program = external_provider.map(|_| self.provider_program.clone());
        self.active_external_args = external_provider.map(|_| self.provider_args.clone());
        self.external_provider_diagnostics = ExternalAgentDiagnostics::default();
        self.external_provider_mcp_failure = None;
        let benchmark_single_model = self.benchmark_child_active();
        let routing_candidates = native_config
            .as_ref()
            .map(|config| self.native_routing_candidates(config))
            .unwrap_or_default();
        self.native_agent_runtime = native_config.map(|config| {
            if benchmark_single_model {
                NativeAgentRuntime::configured_benchmark(config)
            } else {
                NativeAgentRuntime::configured_routed(
                    config,
                    routing_candidates,
                    &self.benchmark_records,
                )
            }
        });
        self.native_run_benchmark_context =
            run_benchmark_identity.map(|(backend_id, model_id, inventory)| {
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
        if self.benchmark_child_requires_initial_validation_failure() {
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
            Ok(PermissionCheck::RequiresApproval) if self.benchmark_child_active() => {
                self.refuse_unbudgeted_benchmark_child_permission(capability);
            }
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
                self.launch_selected_external_agent(run_id, ExternalAgentPurpose::BuildOrRepair)
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

    fn launch_selected_external_agent(&mut self, run_id: &str, purpose: ExternalAgentPurpose) {
        if purpose == ExternalAgentPurpose::BuildOrRepair && self.active_acp_agent_id.is_some() {
            self.launch_acp_agent(run_id);
        } else {
            self.launch_external_agent(run_id, purpose);
        }
    }

    fn launch_acp_agent(&mut self, run_id: &str) {
        let Some(agent_id) = self.active_acp_agent_id.clone() else {
            self.fail_run(
                run_id,
                "ACP execution was selected without a registered agent snapshot.".to_owned(),
            );
            return;
        };
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
            "Prepared isolated managed code workspace for ACP execution.",
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
        self.managed_candidate_input_recipe.clear();
        let repair_context = self.host.run(run_id).ok().and_then(|run| {
            (run.state == AgentRunState::Repairing)
                .then(|| managed_repair_context(run, self.managed_runtime_observation.as_ref()))
        });
        let prompt =
            external_agent_provider_prompt(&proposal_json, repair_context.as_deref(), None);
        let gameengine_session_id = self.selected_session.clone();
        let acp_session_id = match self.acp.open_run_session(
            &mut self.host,
            &agent_id,
            &gameengine_session_id,
            run_id,
            workspace.root().to_path_buf(),
        ) {
            Ok(acp_session_id) => acp_session_id,
            Err(error) => {
                self.fail_run(run_id, format!("Could not open ACP Build session: {error}"));
                return;
            }
        };
        let runtime_identity = match self.acp.runtime_identity(&acp_session_id) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = self.acp.close_session(&acp_session_id);
                let message = format!("Could not read negotiated ACP runtime identity: {error}");
                self.fail_run(run_id, message.clone());
                if self.benchmark_child_active() {
                    self.write_benchmark_child_failure(BenchmarkRunFailureKind::Harness, message);
                }
                return;
            }
        };
        if let Err(error) = self.validate_benchmark_acp_runtime_identity(&runtime_identity) {
            let _ = self.acp.close_session(&acp_session_id);
            let message = format!("ACP benchmark runtime mismatch: {error}");
            self.fail_run(run_id, message.clone());
            if self.benchmark_child_active() {
                self.write_benchmark_child_failure(BenchmarkRunFailureKind::Harness, message);
            }
            return;
        }
        if let Err(error) = self
            .acp
            .send_prompt(&mut self.host, &acp_session_id, &prompt)
        {
            let _ = self.acp.close_session(&acp_session_id);
            self.status = Some(format!("Could not send ACP Build prompt: {error}"));
            return;
        }
        self.code_workspace = Some(workspace);
        self.active_acp_run_session = Some(acp_session_id);
        if let Err(error) = self.host.transition_run(
            run_id,
            AgentRunState::Executing,
            "ACP external agent runtime started in the isolated code workspace.",
        ) {
            self.status = Some(error.to_string());
        } else {
            self.status = Some(format!(
                "ACP external agent `{agent_id}` started through the common Agent Host bridge."
            ));
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
        self.external_provider_mcp_failure = None;
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
                OsString::from(PROVIDER_EVENT_PROTOCOL_GUIDANCE),
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
        if let Err(error) = probe_wsl_loopback_reachability(
            &placement,
            &self.connection.endpoint,
            &self.connection.authorization_token,
        ) {
            match purpose {
                ExternalAgentPurpose::BuildOrRepair => self.fail_run(run_id, error),
                ExternalAgentPurpose::RuntimeEvaluation => {
                    self.record_runtime_evaluation_failure(run_id, error);
                }
            }
            return;
        }
        if purpose == ExternalAgentPurpose::BuildOrRepair
            && placement.environment == ExternalAgentExecutionEnvironment::WindowsNative
            && let Err(error) = probe_editor_mcp_endpoint(
                &self.connection.endpoint,
                &self.connection.authorization_token,
            )
        {
            self.fail_run(
                run_id,
                format!("Cannot start Build because the Editor MCP preflight failed: {error}"),
            );
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

    fn poll_acp_run(&mut self, context: &egui::Context) {
        let Some(acp_session_id) = self.active_acp_run_session.clone() else {
            return;
        };
        let poll = match self.acp.poll(&mut self.host, &acp_session_id) {
            Ok(poll) => poll,
            Err(error) => {
                self.active_acp_run_session = None;
                self.pending_acp_permission = None;
                let message = format!("ACP Build session failed: {error}");
                if let Some(run_id) = self.active_run_id.clone()
                    && self.benchmark_child_active()
                {
                    self.fail_run(&run_id, message);
                } else {
                    self.status = Some(message);
                }
                return;
            }
        };
        match poll {
            AcpBridgePoll::Idle => {
                context.request_repaint_after(std::time::Duration::from_millis(100));
            }
            AcpBridgePoll::RecordedEvent { run_id, event, .. } => {
                self.report_benchmark_acp_live_event(&event);
                if let AcpNormalizedEvent::AgentMessage { text } = event {
                    for line in text.lines() {
                        let Some(payload) = line.trim().strip_prefix(PROVIDER_EVENT_PREFIX) else {
                            continue;
                        };
                        match serde_json::from_str::<ProviderAgentEvent>(payload) {
                            Ok(event) => {
                                if let Err(error) =
                                    self.record_provider_semantic_event(&run_id, event)
                                {
                                    self.status = Some(error);
                                }
                            }
                            Err(error) => {
                                self.status = Some(format!(
                                    "ACP agent emitted an invalid semantic AgentEvent: {error}"
                                ));
                            }
                        }
                    }
                }
                context.request_repaint_after(std::time::Duration::from_millis(50));
            }
            AcpBridgePoll::Recorded { .. } => {
                context.request_repaint_after(std::time::Duration::from_millis(50));
            }
            AcpBridgePoll::PermissionRequired {
                run_id,
                request_id,
                capability,
                ..
            } => {
                if self.benchmark_child_active() {
                    if !self.benchmark_child_allows(capability) {
                        let _ = self.acp.resolve_permission(
                            &mut self.host,
                            &acp_session_id,
                            &request_id,
                            ApprovalScope::Deny,
                        );
                        self.refuse_unbudgeted_benchmark_child_permission(capability);
                        return;
                    }
                    match self.acp.resolve_permission(
                        &mut self.host,
                        &acp_session_id,
                        &request_id,
                        ApprovalScope::Run,
                    ) {
                        Ok(()) => {
                            context.request_repaint_after(std::time::Duration::from_millis(16));
                        }
                        Err(error) => self.write_benchmark_child_failure(
                            BenchmarkRunFailureKind::Harness,
                            format!(
                                "Could not resolve frozen ACP benchmark permission `{}`: {error}",
                                capability.label()
                            ),
                        ),
                    }
                    return;
                }
                self.pending_acp_permission = Some(AcpPendingPermission {
                    acp_session_id,
                    request_id,
                    run_id,
                    capability,
                });
            }
            AcpBridgePoll::ValidationReady { run_id } => {
                self.finish_acp_provider_execution(&run_id, &acp_session_id);
            }
            AcpBridgePoll::TurnFailed { reason, .. } => {
                self.active_acp_run_session = None;
                self.pending_acp_permission = None;
                let close_result = self.acp.close_session(&acp_session_id);
                self.status = Some(match close_result {
                    Ok(()) => reason,
                    Err(error) => {
                        format!("{reason} The ACP session also failed to close cleanly: {error}")
                    }
                });
            }
            AcpBridgePoll::AskEvent(_) => {
                self.status =
                    Some("Run-bound ACP session produced an Ask-only bridge result.".to_owned());
            }
        }
    }

    fn finish_acp_provider_execution(&mut self, run_id: &str, acp_session_id: &str) {
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
        match self
            .acp
            .begin_managed_validation(&mut self.host, acp_session_id, has_code_changes)
        {
            Ok(()) => {
                self.active_acp_run_session = None;
                self.pending_acp_permission = None;
                let close_result = self.acp.close_session(acp_session_id);
                self.status = Some(match close_result {
                    Ok(()) => {
                        "ACP agent returned control with required provider gates satisfied; engine-managed validation is active or has recorded its result."
                            .to_owned()
                    }
                    Err(error) => format!(
                        "Engine-managed validation started, but the ACP session could not close cleanly: {error}"
                    ),
                });
            }
            Err(error) => {
                self.status = Some(format!(
                    "ACP turn returned control but Agent Host validation is not ready: {error}"
                ));
            }
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
                                if let Err(error) = self.record_external_provider_progress(
                                    &run_id,
                                    step.to_owned(),
                                    detail.to_owned(),
                                ) {
                                    self.status = Some(error.to_string());
                                }
                            }
                            ExternalAgentSemanticEvent::ToolAction {
                                tool,
                                action,
                                success,
                            } => {
                                if tool == "workspace.file_change" {
                                    self.external_provider_reported_workspace_write = true;
                                }
                                self.note_external_provider_mcp_failure(&tool, action, success);
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
                            ExternalAgentSemanticEvent::ProtocolDiagnostic(message) => {
                                let _ = self.host.record_event(
                                    &run_id,
                                    AgentEventKind::Failure,
                                    message.clone(),
                                );
                                self.status = Some(message);
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

    fn record_external_provider_progress(
        &mut self,
        run_id: &str,
        step: String,
        detail: String,
    ) -> Result<(), String> {
        self.note_external_provider_mcp_failure(&step, &detail, None);
        self.host
            .record_semantic_progress(run_id, step, detail)
            .map_err(|error| error.to_string())
    }

    fn note_external_provider_mcp_failure(
        &mut self,
        step_or_tool: &str,
        detail: &str,
        success: Option<bool>,
    ) {
        let combined = format!("{step_or_tool} {detail}").to_ascii_lowercase();
        let identifies_editor_mcp = combined.contains("gameengine_editor")
            && (combined.contains("mcp") || combined.contains("tool"));
        let reports_failure = success == Some(false)
            || combined.contains("unavailable")
            || combined.contains("not available")
            || combined.contains("not found")
            || step_or_tool.eq_ignore_ascii_case("blocked");
        if identifies_editor_mcp && reports_failure && self.external_provider_mcp_failure.is_none()
        {
            self.external_provider_mcp_failure = Some(detail.to_owned());
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
        if let Some(detail) = self.external_provider_mcp_failure.take() {
            self.fail_run(
                run_id,
                format!("External provider could not use the injected Editor MCP server: {detail}"),
            );
            return;
        }
        let has_code_changes = !changes.is_empty();
        if self.active_external_provider == Some(ExternalAgentProviderKind::Codex)
            && self.external_provider_environment
                == ExternalAgentExecutionEnvironment::WindowsNative
            && self.external_provider_reported_workspace_write
            && !has_code_changes
        {
            self.fail_run(
                run_id,
                "Codex reported file-change activity but the isolated workspace is unchanged. The Windows workspace-write sandbox may have become effectively read-only; retry with the WSL2 Linux provider environment after its authenticated MCP probe succeeds."
                    .to_owned(),
            );
            return;
        }
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
            ProviderAgentEvent::Progress { step, detail } => {
                self.record_external_provider_progress(run_id, step, detail)
            }
            ProviderAgentEvent::ToolAction {
                tool,
                action,
                success,
            } => {
                self.note_external_provider_mcp_failure(&tool, &action, success);
                self.host
                    .record_tool_action(run_id, tool, action, success)
                    .map_err(|error| error.to_string())
            }
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
        "Act as a GameEngine external AgentRuntime for the immutable proposal below.\n\n{proposal_json}\n\nPersisted project authoring changes must use the injected gameengine_editor MCP server. Project code changes must stay inside the current isolated Agent Code Workspace. Do not commit, push, or alter Git history. Agent Host permissions, work claims, managed validation, Play/frame evidence, and completion gates remain authoritative. Never emit credentials, bearer tokens, or MCP authorization material.\n\n{PROVIDER_EVENT_PROTOCOL_GUIDANCE}"
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
///
/// The transcript, its composer, and the run status line share one column, so
/// the opening size is the one that reads a reply without being resized first.
const EMBEDDED_DEFAULT_SIZE: egui::Vec2 = egui::vec2(820.0_f32, 880.0_f32);
/// Smallest detached OS window that still shows a usable conversation.
const DETACHED_MIN_SIZE: [f32; 2] = [460.0_f32, 520.0_f32];
/// Size the detached studio's OS window opens at before the user resizes it.
///
/// Larger than the embedded default because an OS window is bounded by the
/// display rather than by the Editor around it.
const DETACHED_DEFAULT_SIZE: [f32; 2] = [900.0_f32, 880.0_f32];
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

/// Draws the transcript column before the session has any messages.
///
/// An empty column used to be one weak sentence pinned to the top-left corner
/// of the largest surface in the studio, which said neither what the studio
/// does with a message nor what the currently selected mode will do with it.
/// Both modes are named here because the mode decides whether sending writes.
fn show_empty_transcript(ui: &mut egui::Ui, mode: ConversationMode) {
    ui.add_space(28.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(460.0);
        theme::card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                theme::caption(ui, "No messages yet");
                theme::status_pill(
                    ui,
                    theme::StatusTone::Idle,
                    format!("Mode · {}", mode.label()),
                );
            });
            ui.add_space(4.0);
            ui.label("Describe what you want to build, change, inspect, or validate.");
            ui.add_space(6.0);
            for listed in ConversationMode::ALL {
                theme::field_row(
                    ui,
                    listed.label(),
                    egui::RichText::new(listed.summary())
                        .small()
                        .color(if listed == mode {
                            theme::TEXT
                        } else {
                            theme::TEXT_MUTED
                        }),
                );
            }
        });
    });
}

/// Returns the tone that carries what a run state means for the reader.
///
/// A run is either working on its own, waiting for the person watching it, or
/// finished one way or the other. The wording says which; the tone makes it
/// visible without reading.
fn run_state_tone(state: AgentRunState) -> theme::StatusTone {
    match state {
        AgentRunState::Completed => theme::StatusTone::Ready,
        AgentRunState::Failed => theme::StatusTone::Blocked,
        AgentRunState::Cancelled => theme::StatusTone::Idle,
        AgentRunState::AwaitingUser | AgentRunState::InterruptedForEditing => {
            theme::StatusTone::Attention
        }
        AgentRunState::Inspecting
        | AgentRunState::Planning
        | AgentRunState::Executing
        | AgentRunState::Validating
        | AgentRunState::Playtesting
        | AgentRunState::Evaluating
        | AgentRunState::Repairing => theme::StatusTone::Busy,
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
        (Some(_), None) => "delete",
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

/// Ensures that a Build submission has an immutable execution scope without
/// trying to guess its meaning from a keyword list or language-specific text.
///
/// The provider still has to inspect the project and choose concrete paths and
/// operations that satisfy the user's goal. This fallback only declares the
/// kinds of governed work that a Build request may need; each operation keeps
/// its existing permission, revision, stale-file, and completion checks.
fn build_executor_requires_external_process(selection: SelectedAi) -> bool {
    matches!(
        selection,
        SelectedAi::Agent(_) | SelectedAi::Model(ModelBackendPreference::ManagedLocal)
    )
}

fn ensure_build_scope(proposal: &mut AgentProposal) {
    let has_explicit_scope = !proposal.planned_project_changes.is_empty()
        || !proposal.planned_code_changes.is_empty()
        || !proposal.planned_assets.is_empty()
        || !proposal.validation_plan.is_empty()
        || !proposal.playtest_plan.is_empty();
    if has_explicit_scope {
        return;
    }

    proposal.planned_project_changes.push(
        "Inspect and apply the project-authoring changes required by the submitted Build request."
            .to_owned(),
    );
    proposal.planned_code_changes.push(
        "Inspect and apply the source changes required by the submitted Build request.".to_owned(),
    );
    proposal
        .planned_assets
        .push("Use or acquire only the assets required by the submitted Build request.".to_owned());
    proposal.validation_plan.push(
        "Validate every changed project document and source package using the applicable managed checks."
            .to_owned(),
    );
    proposal.playtest_plan.push(
        "Run the managed runtime when the submitted request requires runtime verification."
            .to_owned(),
    );
    proposal.requested_capabilities.extend([
        AgentCapability::CodeWorkspaceApply,
        AgentCapability::ExternalAssetAcquisition,
        AgentCapability::FrameCapture,
        AgentCapability::RuntimeLaunch,
    ]);
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
    use crate::external_agent_provider::{ExternalAgentAuthStatus, ExternalAgentDiscoveryStatus};

    #[test]
    fn conversation_lane_caps_wide_windows() {
        assert_eq!(AiStudioPanel::conversation_lane_width(1_915.0), 960.0);
    }

    #[test]
    fn conversation_lane_preserves_narrow_windows() {
        assert_eq!(AiStudioPanel::conversation_lane_width(720.0), 720.0);
    }

    #[test]
    fn direct_args_do_not_invoke_shell_parsing() {
        assert_eq!(
            split_direct_args("--flag value ; echo nope"),
            ["--flag", "value", ";", "echo", "nope"]
        );
    }

    #[test]
    fn external_build_executors_budget_process_authority() {
        assert!(build_executor_requires_external_process(SelectedAi::Agent(
            ExternalAgentProviderKind::Codex
        )));
        assert!(build_executor_requires_external_process(SelectedAi::Model(
            ModelBackendPreference::ManagedLocal
        )));
        assert!(!build_executor_requires_external_process(
            SelectedAi::Model(ModelBackendPreference::Local)
        ));
    }

    #[test]
    fn codex_effort_uses_existing_quality_preference() {
        assert_eq!(
            AiStudioPanel::codex_reasoning_effort(QualityPreference::Auto),
            None
        );
        assert_eq!(
            AiStudioPanel::codex_reasoning_effort(QualityPreference::Fast),
            Some("low")
        );
        assert_eq!(
            AiStudioPanel::codex_reasoning_effort(QualityPreference::Balanced),
            Some("medium")
        );
        assert_eq!(
            AiStudioPanel::codex_reasoning_effort(QualityPreference::Deep),
            Some("high")
        );
    }

    #[test]
    fn language_independent_build_scope_covers_governed_work_without_keywords() {
        let mut proposal = AgentProposal::default();

        ensure_build_scope(&mut proposal);

        assert!(!proposal.planned_project_changes.is_empty());
        assert!(!proposal.planned_code_changes.is_empty());
        assert!(!proposal.planned_assets.is_empty());
        assert!(!proposal.validation_plan.is_empty());
        assert!(!proposal.playtest_plan.is_empty());
        assert!(
            proposal
                .requested_capabilities
                .contains(&AgentCapability::CodeWorkspaceApply)
        );
        assert!(
            proposal
                .requested_capabilities
                .contains(&AgentCapability::ExternalAssetAcquisition)
        );
        assert!(
            proposal
                .requested_capabilities
                .contains(&AgentCapability::RuntimeLaunch)
        );
        assert!(
            proposal
                .requested_capabilities
                .contains(&AgentCapability::FrameCapture)
        );
    }

    #[test]
    fn explicit_build_scope_is_preserved_without_expanding_it() {
        let mut proposal = AgentProposal::default();
        proposal
            .planned_code_changes
            .push("Only this source change is in scope.".to_owned());

        ensure_build_scope(&mut proposal);

        assert_eq!(proposal.planned_code_changes.len(), 1);
        assert!(proposal.planned_project_changes.is_empty());
        assert!(proposal.planned_assets.is_empty());
        assert!(proposal.validation_plan.is_empty());
        assert!(proposal.playtest_plan.is_empty());
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
    fn code_change_summary_reports_supported_deletion() {
        let change = CodeChange {
            relative_path: PathBuf::from("game/src/lib.rs"),
            before: Some("old".to_owned()),
            after: None,
        };
        assert_eq!(change_summary(&change), "delete");
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
            ask_uses_external_provider: true,
            model_backend: ModelBackendPreference::HostedApi,
            managed_execution_environment: ManagedExecutionEnvironment::WindowsNative,
            managed_model_id: String::new(),
            local_model_endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            local_model_name: String::new(),
            hosted_model_endpoint: "https://provider.example/v1/chat/completions".to_owned(),
            hosted_model_name: "example-model".to_owned(),
            selected_ai_family: Some(SelectedAiFamily::Agent),
            presentation_mode: AiStudioPresentationMode::default(),
            remote_phone_url_base: "https://my-pc.example.ts.net".to_owned(),
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

    /// An AI that cannot serve the mode is stated, never substituted.
    ///
    /// ADR 0164 §2: the reason names the entry and the mode, and offers the
    /// two places that change the state. Nothing in it proposes running the
    /// turn on a different AI, which is what the removed Ask routing toggle
    /// used to do silently.
    #[test]
    fn an_agent_that_cannot_serve_the_mode_is_named_with_its_reason() {
        let ask_with_generic = agent_unavailable_for_mode(
            ExternalAgentProviderKind::Generic,
            ConversationMode::Ask,
            ProviderReadiness::Ready,
        )
        .expect("a compatible agent command cannot answer Ask");
        assert!(ask_with_generic.contains(ExternalAgentProviderKind::Generic.label()));
        assert!(ask_with_generic.contains("cannot answer Ask"));

        // The same agent runs Build, so Build is not blocked by that rule.
        assert_eq!(
            agent_unavailable_for_mode(
                ExternalAgentProviderKind::Generic,
                ConversationMode::Build,
                ProviderReadiness::Ready,
            ),
            None
        );

        let signed_out = agent_unavailable_for_mode(
            ExternalAgentProviderKind::ClaudeCode,
            ConversationMode::Build,
            ProviderReadiness::SignInRequired,
        )
        .expect("a signed-out agent cannot run Build");
        assert!(signed_out.contains(ExternalAgentProviderKind::ClaudeCode.label()));
        assert!(signed_out.contains("Agents in settings"));

        assert_eq!(
            agent_unavailable_for_mode(
                ExternalAgentProviderKind::Codex,
                ConversationMode::Ask,
                ProviderReadiness::Ready,
            ),
            None
        );
    }

    /// An AI entry is named by what it is, not by where it sits in the list.
    ///
    /// ADR 0164 §5: the companion sends this identity back, and the host has to
    /// resolve it to the same entry after a restart or a reordering.
    #[test]
    fn ai_entry_identities_distinguish_every_entry_the_composer_lists() {
        let mut identities = std::collections::BTreeSet::new();
        for kind in ExternalAgentProviderKind::ALL {
            assert!(identities.insert(ai_entry_id(SelectedAi::Agent(kind), "")));
        }
        for backend in ModelBackendPreference::ALL {
            assert!(identities.insert(ai_entry_id(SelectedAi::Model(backend), "")));
        }
        // Two registered GGUF files are two entries, not one.
        assert_ne!(
            ai_entry_id(
                SelectedAi::Model(ModelBackendPreference::ManagedLocal),
                "qwen3-14b"
            ),
            ai_entry_id(
                SelectedAi::Model(ModelBackendPreference::ManagedLocal),
                "gemma-12b"
            )
        );
        // An agent identity can never be read as a model identity.
        assert!(
            ai_entry_id(SelectedAi::Agent(ExternalAgentProviderKind::ClaudeCode), "")
                .starts_with("agent:")
        );
    }

    /// Effort identities round-trip without a second table to keep in step.
    #[test]
    fn every_effort_level_round_trips_through_its_remote_identity() {
        for quality in QualityPreference::ALL {
            assert_eq!(effort_from_id(&effort_id(quality)), Some(quality));
        }
        assert_eq!(effort_from_id("thorough"), None);
    }

    /// Mode identities are the values the companion already sends.
    #[test]
    fn every_conversation_mode_round_trips_through_its_remote_identity() {
        for mode in ConversationMode::ALL {
            assert_eq!(
                ConversationMode::ALL
                    .into_iter()
                    .find(|candidate| candidate.remote_id() == mode.remote_id()),
                Some(mode)
            );
        }
        assert_eq!(ConversationMode::Ask.remote_id(), "ask");
        assert_eq!(ConversationMode::Build.remote_id(), "build");
    }

    #[test]
    fn ask_is_agent_served_only_by_a_ready_first_class_agent() {
        let ready = |kind| ExternalAgentProviderStatus {
            kind,
            discovery: ExternalAgentDiscoveryStatus::Available,
            auth: ExternalAgentAuthStatus::Authenticated,
        };
        assert!(ask_is_agent_served(
            SelectedAi::Agent(ExternalAgentProviderKind::Codex),
            &ready(ExternalAgentProviderKind::Codex)
        ));
        // ADR 0164 §1: selecting a model is the statement that the model
        // answers, whatever a signed-in agent happens to be installed.
        assert!(!ask_is_agent_served(
            SelectedAi::Model(ModelBackendPreference::ManagedLocal),
            &ready(ExternalAgentProviderKind::Codex)
        ));
        // A generic command has no launch shape GameEngine can prove read-only.
        assert!(!ask_is_agent_served(
            SelectedAi::Agent(ExternalAgentProviderKind::Generic),
            &ready(ExternalAgentProviderKind::Generic)
        ));
        assert!(!ask_is_agent_served(
            SelectedAi::Agent(ExternalAgentProviderKind::ClaudeCode),
            &ExternalAgentProviderStatus {
                kind: ExternalAgentProviderKind::ClaudeCode,
                discovery: ExternalAgentDiscoveryStatus::Available,
                auth: ExternalAgentAuthStatus::SignInRequired,
            }
        ));
        // A status left over from another agent never authorizes this one.
        assert!(!ask_is_agent_served(
            SelectedAi::Agent(ExternalAgentProviderKind::ClaudeCode),
            &ready(ExternalAgentProviderKind::Codex)
        ));
    }

    #[test]
    fn preferences_written_before_one_ai_selection_resolve_to_exactly_one() {
        // ADR 0164 Compatibility: a file written before the selection existed
        // records a provider, a backend, and an Ask routing flag. Exactly one
        // selection is recovered from them, so an upgrade never changes who
        // runs the next message.
        let routed_to_agent: AiStudioPreferences = serde_json::from_str(
            r#"{"schema_version":1,"model_backend":"local","external_agent_provider":"claude_code"}"#,
        )
        .expect("deserialize preferences written before ADR 0164");
        assert_eq!(
            routed_to_agent.resolved_selected_ai_family(),
            SelectedAiFamily::Agent
        );

        let routing_declined: AiStudioPreferences = serde_json::from_str(
            r#"{"schema_version":1,"model_backend":"managed_local","external_agent_provider":"claude_code","ask_uses_external_provider":false}"#,
        )
        .expect("deserialize preferences that declined provider-served Ask");
        assert_eq!(
            routing_declined.resolved_selected_ai_family(),
            SelectedAiFamily::Model
        );

        // A generic command cannot answer Ask, so the routing flag never made
        // it the Ask runtime and it does not become the selection now.
        let generic_command: AiStudioPreferences =
            serde_json::from_str(r#"{"schema_version":1,"model_backend":"local"}"#)
                .expect("deserialize preferences with no first-class provider");
        assert_eq!(
            generic_command.resolved_selected_ai_family(),
            SelectedAiFamily::Model
        );

        // A file written after the record keeps what it recorded.
        let recorded: AiStudioPreferences = serde_json::from_str(
            r#"{"schema_version":1,"model_backend":"local","selected_ai_family":"agent","ask_uses_external_provider":false}"#,
        )
        .expect("deserialize preferences written after ADR 0164");
        assert_eq!(
            recorded.resolved_selected_ai_family(),
            SelectedAiFamily::Agent
        );
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
