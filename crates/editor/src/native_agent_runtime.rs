//! Governed write-capable native AgentRuntime for AI Studio (ADR 0141).
//!
//! Models choose semantic actions; the existing Agent Host and managed Editor
//! services remain authoritative for every side effect and completion gate.

use crate::agent_benchmark::BenchmarkRecord;
use crate::agent_host::{AgentRun, AgentRunState, AgentWorkingStateUpdate, CompletionStatus};
use crate::digest::sha256_hex;
use crate::model_router::{
    ModelRouteDecision, ModelRoutingError, ModelRoutingPolicy, RoutingWorkload,
};
use crate::native_agent::{
    LocalModelConfig, ModelCapabilityProfile, ModelTurnOutcome, NativeAgentError,
    NativeModelConfig, NativeModelTask,
};
use engine_mcp::authoring_tool_descriptors;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
pub(crate) const NATIVE_WRITE_HARNESS_VERSION: &str = "native-write-v1";
const MAX_TOOL_RESULT_CHARS: usize = 12_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HarnessPolicy {
    pub(crate) max_model_turns: u32,
    pub(crate) max_tool_failures: u32,
    pub(crate) repair_budget: u32,
}

impl Default for HarnessPolicy {
    fn default() -> Self {
        Self {
            max_model_turns: 24,
            max_tool_failures: 4,
            repair_budget: 2,
        }
    }
}

pub(crate) trait ModelTurnTask: Send {
    fn interrupt(&self);
    fn poll(&self) -> Option<Result<ModelTurnOutcome, NativeAgentError>>;
}

impl ModelTurnTask for NativeModelTask {
    fn interrupt(&self) {
        NativeModelTask::interrupt(self);
    }
    fn poll(&self) -> Option<Result<ModelTurnOutcome, NativeAgentError>> {
        NativeModelTask::poll(self)
    }
}

/// Model-provider seam. Agent policy and tools never depend on a model family.
pub(crate) trait ModelBackend: Send + Sync {
    fn label(&self) -> String;
    fn profile(&self) -> ModelCapabilityProfile;
    fn start_turn(
        &self,
        prompt: String,
        images: Vec<Vec<u8>>,
        response_schema: Option<Value>,
    ) -> Result<Box<dyn ModelTurnTask>, NativeAgentError>;
}

#[derive(Debug, Clone)]
struct ConfiguredModelBackend(NativeModelConfig);

impl ModelBackend for ConfiguredModelBackend {
    fn label(&self) -> String {
        self.0.label()
    }
    fn profile(&self) -> ModelCapabilityProfile {
        self.0.capability_profile()
    }
    fn start_turn(
        &self,
        prompt: String,
        images: Vec<Vec<u8>>,
        response_schema: Option<Value>,
    ) -> Result<Box<dyn ModelTurnTask>, NativeAgentError> {
        NativeModelTask::spawn(self.0.clone(), prompt, images, response_schema)
            .map(|task| Box::new(task) as Box<dyn ModelTurnTask>)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct NativeWorkingStateUpdate {
    pub(crate) architecture_constraints: Vec<String>,
    pub(crate) ownership_crate_decisions: Vec<String>,
    pub(crate) target_files_documents: Vec<String>,
    pub(crate) concrete_edits: Vec<String>,
    pub(crate) typed_mcp_operations: Vec<String>,
    pub(crate) tests: Vec<String>,
    pub(crate) assumptions: Vec<String>,
    pub(crate) replan_conditions: Vec<String>,
    pub(crate) relevant_source_provenance: Vec<String>,
    pub(crate) open_problems: Vec<String>,
}

impl From<NativeWorkingStateUpdate> for AgentWorkingStateUpdate {
    fn from(value: NativeWorkingStateUpdate) -> Self {
        Self {
            architecture_constraints: value.architecture_constraints,
            ownership_crate_decisions: value.ownership_crate_decisions,
            target_files_documents: value.target_files_documents,
            concrete_edits: value.concrete_edits,
            typed_mcp_operations: value.typed_mcp_operations,
            tests: value.tests,
            assumptions: value.assumptions,
            replan_conditions: value.replan_conditions,
            relevant_source_provenance: value.relevant_source_provenance,
            open_problems: value.open_problems,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum NativeAgentAction {
    McpCall {
        tool: String,
        #[serde(default)]
        arguments: Value,
    },
    CodeWrite {
        path: String,
        text: String,
    },
    RuntimeInput {
        input: Value,
    },
    CompletionGate {
        gate: String,
        status: CompletionStatus,
        message: String,
    },
    Progress {
        step: String,
        detail: String,
    },
    AwaitUser {
        question: String,
    },
    ReadyForValidation,
}

/// One recorded request/response pair against a model (ADR 0159).
///
/// The bounded fields are what the run record carries; `prompt` and `response`
/// are the full text the host writes to the per-run transcript artifact.
#[derive(Debug, Clone)]
pub(crate) struct ModelExchange {
    pub(crate) turn: u32,
    pub(crate) prompt: String,
    pub(crate) response: String,
    pub(crate) prompt_tokens: Option<u64>,
    pub(crate) response_tokens: Option<u64>,
    pub(crate) finish_reason: String,
    pub(crate) response_digest: String,
}

/// Bounded excerpt length carried in the run record instead of the full text.
const MODEL_EXCHANGE_EXCERPT_CHARS: usize = 600;

impl ModelExchange {
    fn new(turn: u32, prompt: String, outcome: ModelTurnOutcome) -> Self {
        Self {
            turn,
            prompt,
            response_digest: sha256_hex(outcome.text.as_bytes()),
            finish_reason: outcome
                .finish_reason
                .filter(|reason| !reason.trim().is_empty())
                .unwrap_or_else(|| "unreported".to_owned()),
            prompt_tokens: outcome.prompt_tokens,
            response_tokens: outcome.response_tokens,
            response: outcome.text,
        }
    }

    pub(crate) fn response_excerpt(&self) -> String {
        truncate(&self.response, MODEL_EXCHANGE_EXCERPT_CHARS)
    }
}

/// What one polled model turn produced, evidence first.
///
/// The exchange is present whenever the model answered at all, including when
/// the answer could not be parsed, so a failing run still records what it got.
pub(crate) struct NativeTurnOutcome {
    pub(crate) exchange: Option<ModelExchange>,
    pub(crate) result: Result<NativeAgentTurn, NativeAgentRuntimeError>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct NativeAgentTurn {
    #[serde(default)]
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) working_state: NativeWorkingStateUpdate,
    pub(crate) action: NativeAgentAction,
}

#[derive(Debug)]
pub(crate) enum NativeAgentRuntimeError {
    Backend(NativeAgentError),
    Busy,
    TurnBudget,
    ToolFailureBudget,
    InvalidOutput(String),
    Rejected(String),
    Routing(ModelRoutingError),
}

impl fmt::Display for NativeAgentRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(e) => e.fmt(f),
            Self::Busy => write!(f, "native AgentRuntime already has an active model turn"),
            Self::TurnBudget => write!(f, "native AgentRuntime model-turn budget exhausted"),
            Self::ToolFailureBudget => {
                write!(f, "native AgentRuntime tool-failure budget exhausted")
            }
            Self::InvalidOutput(e) => write!(f, "invalid native structured output: {e}"),
            Self::Rejected(e) => write!(f, "native action rejected: {e}"),
            Self::Routing(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for NativeAgentRuntimeError {}
impl From<NativeAgentError> for NativeAgentRuntimeError {
    fn from(value: NativeAgentError) -> Self {
        Self::Backend(value)
    }
}
impl From<ModelRoutingError> for NativeAgentRuntimeError {
    fn from(value: ModelRoutingError) -> Self {
        Self::Routing(value)
    }
}

struct Exchange {
    tool: String,
    success: bool,
    result: String,
}

pub(crate) struct NativeAgentRuntime {
    backend: Box<dyn ModelBackend>,
    backend_config: NativeModelConfig,
    routing: ModelRoutingPolicy,
    routing_decisions: Vec<ModelRouteDecision>,
    policy: HarnessPolicy,
    turns: u32,
    failures: u32,
    exchanges: Vec<Exchange>,
    active_prompt: Option<String>,
    active: Option<Box<dyn ModelTurnTask>>,
}

impl NativeAgentRuntime {
    pub(crate) fn configured(config: NativeModelConfig) -> Self {
        Self::configured_routed(config, Vec::new(), &[])
    }
    pub(crate) fn configured_routed(
        primary: NativeModelConfig,
        candidates: Vec<NativeModelConfig>,
        benchmark_records: &[BenchmarkRecord],
    ) -> Self {
        let routing = ModelRoutingPolicy::derive(primary.clone(), candidates, benchmark_records);
        Self::new(
            Box::new(ConfiguredModelBackend(primary.clone())),
            primary,
            routing,
            HarnessPolicy::default(),
        )
    }
    #[allow(dead_code)]
    pub(crate) fn local(config: LocalModelConfig) -> Self {
        Self::configured(NativeModelConfig::Local(config))
    }
    fn new(
        backend: Box<dyn ModelBackend>,
        backend_config: NativeModelConfig,
        routing: ModelRoutingPolicy,
        policy: HarnessPolicy,
    ) -> Self {
        Self {
            backend,
            backend_config,
            routing,
            routing_decisions: Vec::new(),
            policy,
            turns: 0,
            failures: 0,
            exchanges: Vec::new(),
            active_prompt: None,
            active: None,
        }
    }
    pub(crate) fn backend_label(&self) -> String {
        self.backend.label()
    }
    pub(crate) fn routing_policy_summary(&self) -> String {
        format!(
            "{} adopted specialist workload(s)",
            self.routing.adopted_specialist_count()
        )
    }
    pub(crate) fn take_routing_decisions(&mut self) -> Vec<ModelRouteDecision> {
        std::mem::take(&mut self.routing_decisions)
    }
    pub(crate) fn is_busy(&self) -> bool {
        self.active.is_some()
    }
    pub(crate) fn supports_visual_evaluation(&self) -> bool {
        self.routing
            .select(RoutingWorkload::VisualEvaluation, true)
            .is_ok()
    }
    pub(crate) fn interrupt(&mut self) {
        if let Some(task) = self.active.as_ref() {
            task.interrupt();
        }
        self.active = None;
        self.active_prompt = None;
    }
    /// Starts one benchmark-attributed turn on the runtime's exact configured model.
    ///
    /// Unlike `start_turn`, this entry point deliberately performs no ADR 0150
    /// specialist selection and no fallback. Backend or image-capability failure
    /// therefore remains attributable to this exact model representation.
    pub(crate) fn start_turn_single_model(
        &mut self,
        run: &AgentRun,
        context: Option<&str>,
        images: Vec<Vec<u8>>,
    ) -> Result<(), NativeAgentRuntimeError> {
        if self.active.is_some() {
            return Err(NativeAgentRuntimeError::Busy);
        }
        if self.turns >= self.policy.max_model_turns {
            return Err(NativeAgentRuntimeError::TurnBudget);
        }
        let prompt = build_prompt(
            run,
            &self.backend.profile(),
            self.policy,
            &self.exchanges,
            context,
        );
        let schema = self.constrained_response_schema();
        self.active_prompt = Some(prompt.clone());
        self.active = Some(self.backend.start_turn(prompt, images, schema)?);
        self.turns += 1;
        Ok(())
    }

    pub(crate) fn start_turn(
        &mut self,
        run: &AgentRun,
        context: Option<&str>,
        images: Vec<Vec<u8>>,
    ) -> Result<(), NativeAgentRuntimeError> {
        if self.active.is_some() {
            return Err(NativeAgentRuntimeError::Busy);
        }
        if self.turns >= self.policy.max_model_turns {
            return Err(NativeAgentRuntimeError::TurnBudget);
        }
        let workload = routing_workload(run.state, !images.is_empty());
        let decision = self.routing.select(workload, !images.is_empty())?;
        self.apply_route_decision(&decision);
        let prompt = build_prompt(
            run,
            &self.backend.profile(),
            self.policy,
            &self.exchanges,
            context,
        );
        let schema = self.constrained_response_schema();
        self.active_prompt = Some(prompt.clone());
        match self
            .backend
            .start_turn(prompt, images.clone(), schema.clone())
        {
            Ok(task) => self.active = Some(task),
            Err(error) => {
                let Some(fallback) =
                    self.routing
                        .fallback(&self.backend_config, workload, !images.is_empty())
                else {
                    return Err(error.into());
                };
                self.apply_route_decision(&fallback);
                let fallback_prompt = build_prompt(
                    run,
                    &self.backend.profile(),
                    self.policy,
                    &self.exchanges,
                    context,
                );
                self.active_prompt = Some(fallback_prompt.clone());
                self.active = Some(self.backend.start_turn(
                    fallback_prompt,
                    images,
                    self.constrained_response_schema(),
                )?);
            }
        }
        self.turns += 1;
        Ok(())
    }

    /// Response schema for backends that report constrained decoding support.
    ///
    /// A backend that does not report it receives none, so an adapter is never
    /// sent a request shape it did not declare it understands.
    fn constrained_response_schema(&self) -> Option<Value> {
        self.backend
            .profile()
            .structured_output
            .unwrap_or(false)
            .then(agent_turn_response_schema)
    }

    fn apply_route_decision(&mut self, decision: &ModelRouteDecision) {
        if !same_model(&self.backend_config, &decision.config) {
            self.backend_config = decision.config.clone();
            self.backend = Box::new(ConfiguredModelBackend(decision.config.clone()));
        }
        self.routing_decisions.push(decision.clone());
        if self.routing_decisions.len() > 128 {
            self.routing_decisions.remove(0);
        }
    }
    pub(crate) fn poll(&mut self) -> Option<NativeTurnOutcome> {
        let result = self.active.as_ref()?.poll()?;
        self.active = None;
        let prompt = self.active_prompt.take().unwrap_or_default();
        Some(match result {
            Ok(outcome) => {
                let exchange = ModelExchange::new(self.turns, prompt, outcome);
                let result = parse_turn(&exchange.response);
                NativeTurnOutcome {
                    exchange: Some(exchange),
                    result,
                }
            }
            Err(error) => NativeTurnOutcome {
                exchange: None,
                result: Err(error.into()),
            },
        })
    }
    pub(crate) fn record_tool_result(
        &mut self,
        tool: impl Into<String>,
        success: bool,
        result: impl Into<String>,
    ) -> Result<(), NativeAgentRuntimeError> {
        if !success {
            self.failures += 1;
            if self.failures > self.policy.max_tool_failures {
                return Err(NativeAgentRuntimeError::ToolFailureBudget);
            }
        }
        self.exchanges.push(Exchange {
            tool: tool.into(),
            success,
            result: truncate(&result.into(), MAX_TOOL_RESULT_CHARS),
        });
        if self.exchanges.len() > 16 {
            self.exchanges.remove(0);
        }
        Ok(())
    }
    pub(crate) fn validate_action(
        &self,
        run: &AgentRun,
        action: &NativeAgentAction,
    ) -> Result<(), NativeAgentRuntimeError> {
        let executing = matches!(
            run.state,
            AgentRunState::Executing | AgentRunState::Repairing
        );
        match action {
            NativeAgentAction::McpCall { tool, .. } if !executing => {
                return reject("MCP calls require execution/repair");
            }
            NativeAgentAction::McpCall { tool, .. }
                if mcp_write(tool) && run.proposal_snapshot.planned_project_changes.is_empty() =>
            {
                return reject("MCP mutation is outside immutable proposal project changes");
            }
            NativeAgentAction::CodeWrite { path, .. }
                if !executing
                    || run.proposal_snapshot.planned_code_changes.is_empty()
                    || !managed_code_path(Path::new(path)) =>
            {
                return reject("code_write is outside execution/proposal/managed workspace scope");
            }
            NativeAgentAction::RuntimeInput { .. }
                if !executing || run.proposal_snapshot.playtest_plan.is_empty() =>
            {
                return reject("runtime_input requires an immutable playtest plan");
            }
            NativeAgentAction::CompletionGate { gate, .. } => match gate.as_str() {
                "acceptance_criteria" | "authoring_validation" if executing => {}
                "visual_evaluation" if run.state == AgentRunState::Evaluating => {}
                _ => return reject("completion gate is not provider-reportable in this phase"),
            },
            NativeAgentAction::ReadyForValidation if !executing => {
                return reject("validation requires execution/repair");
            }
            _ => {}
        }
        Ok(())
    }
}

fn routing_workload(state: AgentRunState, has_images: bool) -> RoutingWorkload {
    match state {
        AgentRunState::Repairing => RoutingWorkload::ValidationRepair,
        AgentRunState::Evaluating if has_images => RoutingWorkload::VisualEvaluation,
        AgentRunState::Evaluating => RoutingWorkload::RuntimeInteraction,
        AgentRunState::Executing => RoutingWorkload::CodeImplementation,
        _ => RoutingWorkload::ProjectInspection,
    }
}

fn same_model(left: &NativeModelConfig, right: &NativeModelConfig) -> bool {
    left.backend_id() == right.backend_id() && left.model_id() == right.model_id()
}

fn reject<T>(message: &str) -> Result<T, NativeAgentRuntimeError> {
    Err(NativeAgentRuntimeError::Rejected(message.to_owned()))
}

fn build_prompt(
    run: &AgentRun,
    profile: &ModelCapabilityProfile,
    policy: HarnessPolicy,
    exchanges: &[Exchange],
    context: Option<&str>,
) -> String {
    let mut prompt = format!(
        "GameEngine NativeAgentRuntime ADR0141. Return exactly one JSON object, no markdown. Do not claim side effects; AgentHost owns truth.\nImmutable proposal={}\nstate={:?}\nworking_state={}\ncompletion={}\nbackend={} model={} structured={:?} tools={:?} image={:?} reasoning={:?} context_limit={:?} streaming={:?} usage={:?}\nHarnessPolicy turns={} failures={} repair_budget={}. Canonical authoring ONLY via mcp_call; source ONLY via code_write isolated AgentCodeWorkspace. Stale revision, permission, validation, import and runtime failures are evidence. ready_for_validation returns control to host validation. Never mark source_validation/play_launch/frame_capture/interaction_scenarios.\n",
        serde_json::to_string(&run.proposal_snapshot).unwrap_or_default(),
        run.state,
        serde_json::to_string(&run.working_state).unwrap_or_default(),
        serde_json::to_string(&run.completion).unwrap_or_default(),
        profile.backend_id,
        profile.model_id,
        profile.structured_output,
        profile.tool_use,
        profile.image_input,
        profile.reasoning,
        profile.context_limit,
        profile.streaming,
        profile.usage,
        policy.max_model_turns,
        policy.max_tool_failures,
        policy.repair_budget
    );
    if let Some(context) = context {
        prompt.push_str(&format!("phase_context={context}\n"));
    }
    for exchange in exchanges {
        prompt.push_str(&format!(
            "tool_result {} success={}: {}\n",
            exchange.tool, exchange.success, exchange.result
        ));
    }
    prompt.push_str("MCP tools:\n");
    for tool in authoring_tool_descriptors() {
        prompt.push_str(&format!(
            "{} schema={}\n",
            tool.name,
            truncate(&tool.input_schema.to_string(), 2400)
        ));
    }
    prompt.push_str(concat!(
        "Output {\"summary\":\"persistable facts\",\"working_state\":{},\"action\":ACTION}. ACTION one of:\n",
        "{\"type\":\"mcp_call\",\"tool\":\"...\",\"arguments\":{}}\n",
        "{\"type\":\"code_write\",\"path\":\"game/...\",\"text\":\"complete text\"}\n",
        "{\"type\":\"runtime_input\",\"input\":{\"kind\":\"key|hold_key|mouse_button|hold_mouse_button|gamepad_button|gamepad_axis|mouse_move|mouse_delta|mouse_scroll\",\"at_tick\":0,...}} -- at_tick is a fixed simulation tick offset; hold_* requires integer ticks and expands host-side without model wall-clock timing\n",
        "{\"type\":\"completion_gate\",\"gate\":\"acceptance_criteria|authoring_validation|visual_evaluation\",\"status\":\"passed|failed|not_applicable\",\"message\":\"...\"}\n",
        "{\"type\":\"progress\",\"step\":\"...\",\"detail\":\"...\"}\n",
        "{\"type\":\"await_user\",\"question\":\"...\"}\n{\"type\":\"ready_for_validation\"}\n"));
    prompt
}

/// JSON Schema for one agent turn, used for backend-constrained decoding.
///
/// The schema mirrors [`NativeAgentTurn`] exactly, so a backend that constrains
/// generation to it cannot return a turn the parser rejects. It is provider
/// independent: every backend GameEngine supports either accepts it or reports
/// that it does not, and the runtime then falls back to prompt-shaped output.
pub(crate) fn agent_turn_response_schema() -> Value {
    let string_list = json!({ "type": "array", "items": { "type": "string" } });
    let working_state_fields = [
        "architecture_constraints",
        "ownership_crate_decisions",
        "target_files_documents",
        "concrete_edits",
        "typed_mcp_operations",
        "tests",
        "assumptions",
        "replan_conditions",
        "relevant_source_provenance",
        "open_problems",
    ]
    .into_iter()
    .map(|field| (field.to_owned(), string_list.clone()))
    .collect::<serde_json::Map<_, _>>();
    let action = |kind: &str, fields: Value, required: Value| {
        let mut properties = serde_json::Map::new();
        properties.insert("type".to_owned(), json!({ "const": kind }));
        if let Value::Object(fields) = fields {
            properties.extend(fields);
        }
        let mut required_names = vec![json!("type")];
        if let Value::Array(names) = required {
            required_names.extend(names);
        }
        json!({
            "type": "object",
            "properties": Value::Object(properties),
            "required": Value::Array(required_names),
            "additionalProperties": false,
        })
    };
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "working_state": {
                "type": "object",
                "properties": Value::Object(working_state_fields),
                "additionalProperties": false,
            },
            "action": {
                "anyOf": [
                    action(
                        "mcp_call",
                        json!({ "tool": { "type": "string" }, "arguments": { "type": "object" } }),
                        json!(["tool", "arguments"]),
                    ),
                    action(
                        "code_write",
                        json!({ "path": { "type": "string" }, "text": { "type": "string" } }),
                        json!(["path", "text"]),
                    ),
                    action(
                        "runtime_input",
                        json!({ "input": { "type": "object" } }),
                        json!(["input"]),
                    ),
                    action(
                        "completion_gate",
                        json!({
                            "gate": { "type": "string" },
                            "status": { "enum": ["passed", "failed", "not_applicable"] },
                            "message": { "type": "string" },
                        }),
                        json!(["gate", "status", "message"]),
                    ),
                    action(
                        "progress",
                        json!({ "step": { "type": "string" }, "detail": { "type": "string" } }),
                        json!(["step", "detail"]),
                    ),
                    action(
                        "await_user",
                        json!({ "question": { "type": "string" } }),
                        json!(["question"]),
                    ),
                    action("ready_for_validation", json!({}), json!([])),
                ]
            }
        },
        "required": ["summary", "action"],
        "additionalProperties": false,
    })
}

fn parse_turn(text: &str) -> Result<NativeAgentTurn, NativeAgentRuntimeError> {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .unwrap_or(text);
    let text = text.strip_suffix("```").unwrap_or(text).trim();
    serde_json::from_str(text).map_err(|e| NativeAgentRuntimeError::InvalidOutput(e.to_string()))
}
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}
pub(crate) fn mcp_write(tool: &str) -> bool {
    ![
        ".describe",
        ".inspect",
        ".find",
        ".list",
        ".search",
        ".validate",
        ".preview",
        ".schemas",
        ".capabilities",
    ]
    .iter()
    .any(|suffix| tool.ends_with(suffix))
        && !matches!(tool, "project.describe" | "component.schemas")
}
fn managed_code_path(path: &Path) -> bool {
    !path.is_absolute()
        && !path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        && (path.starts_with("game")
            || path.starts_with("assets/scripts/rust")
            || path.starts_with("assets/scripts/rhai"))
}

/// Background live Editor MCP call. Credential lifetime is worker-only; only
/// structuredContent is returned to the runtime and therefore persistable.
pub(crate) struct NativeMcpTask {
    result: Receiver<Result<Value, String>>,
    interrupted: Arc<AtomicBool>,
    stream: Arc<Mutex<Option<TcpStream>>>,
}
impl NativeMcpTask {
    pub(crate) fn spawn(
        endpoint: String,
        token: String,
        tool: String,
        arguments: Value,
    ) -> Result<Self, String> {
        let (address, path) = parse_mcp_endpoint(&endpoint)?;
        let (sender, result) = mpsc::channel();
        let interrupted = Arc::new(AtomicBool::new(false));
        let stream = Arc::new(Mutex::new(None));
        let wi = Arc::clone(&interrupted);
        let ws = Arc::clone(&stream);
        std::thread::Builder::new()
            .name("ai-native-mcp".to_owned())
            .spawn(move || {
                let _ = sender.send(call_mcp(address, &path, &token, &tool, arguments, &wi, &ws));
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            result,
            interrupted,
            stream,
        })
    }
    pub(crate) fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
        if let Ok(guard) = self.stream.lock()
            && let Some(stream) = guard.as_ref()
        {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }
    pub(crate) fn poll(&self) -> Option<Result<Value, String>> {
        match self.result.try_recv() {
            Ok(v) => Some(v),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                Some(Err("native MCP worker disconnected".to_owned()))
            }
        }
    }
}
fn parse_mcp_endpoint(endpoint: &str) -> Result<(SocketAddr, String), String> {
    let rest = endpoint
        .strip_prefix("http://127.0.0.1:")
        .ok_or_else(|| "MCP endpoint must be Editor loopback HTTP".to_owned())?;
    let (port, path) = rest
        .split_once('/')
        .ok_or_else(|| "MCP endpoint path missing".to_owned())?;
    if path != "mcp" {
        return Err("MCP endpoint must target /mcp".to_owned());
    }
    Ok((
        format!("127.0.0.1:{port}")
            .parse::<SocketAddr>()
            .map_err(|e| e.to_string())?,
        "/mcp".to_owned(),
    ))
}
fn call_mcp(
    address: SocketAddr,
    path: &str,
    token: &str,
    tool: &str,
    arguments: Value,
    interrupted: &AtomicBool,
    active: &Mutex<Option<TcpStream>>,
) -> Result<Value, String> {
    if interrupted.load(Ordering::Acquire) {
        return Err("native MCP call interrupted".to_owned());
    }
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(35)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(35)))
        .map_err(|e| e.to_string())?;
    if let Ok(mut guard) = active.lock() {
        *guard = stream.try_clone().ok();
    }
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":arguments}})).map_err(|e| e.to_string())?;
    write!(stream, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: {MCP_PROTOCOL_VERSION}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
    stream.write_all(&body).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    stream
        .take(2 * 1024 * 1024)
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    if let Ok(mut guard) = active.lock() {
        *guard = None;
    }
    if interrupted.load(Ordering::Acquire) {
        return Err("native MCP call interrupted".to_owned());
    }
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "invalid MCP HTTP response".to_owned())?;
    let header =
        std::str::from_utf8(&response[..split]).map_err(|_| "invalid MCP headers".to_owned())?;
    if !header
        .lines()
        .next()
        .is_some_and(|line| line.contains(" 200 "))
    {
        return Err(format!(
            "MCP HTTP failure: {}",
            header.lines().next().unwrap_or("unknown")
        ));
    }
    let rpc: Value = serde_json::from_slice(&response[split + 4..]).map_err(|e| e.to_string())?;
    if let Some(error) = rpc.get("error") {
        return Err(format!("MCP JSON-RPC error: {error}"));
    }
    let result = rpc
        .get("result")
        .cloned()
        .ok_or_else(|| "MCP result missing".to_owned())?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(result.to_string());
    }
    Ok(result.get("structuredContent").cloned().unwrap_or(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_agent::DEFAULT_LOCAL_MODEL_ENDPOINT;
    #[test]
    fn parser_requires_structured_action() {
        assert!(parse_turn("I changed files").is_err());
        assert!(
            parse_turn(
                r#"{"action":{"type":"mcp_call","tool":"project.describe","arguments":{}}}"#
            )
            .is_ok()
        );
    }
    #[test]
    fn mcp_endpoint_is_loopback_only() {
        assert!(parse_mcp_endpoint("http://127.0.0.1:1234/mcp").is_ok());
        assert!(parse_mcp_endpoint("http://192.168.0.2:1234/mcp").is_err());
    }

    #[test]
    fn response_schema_covers_every_action_the_parser_accepts() {
        let schema = agent_turn_response_schema();
        let actions = schema["properties"]["action"]["anyOf"]
            .as_array()
            .expect("action alternatives")
            .iter()
            .map(|action| {
                action["properties"]["type"]["const"]
                    .as_str()
                    .expect("action tag")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            actions,
            vec![
                "mcp_call",
                "code_write",
                "runtime_input",
                "completion_gate",
                "progress",
                "await_user",
                "ready_for_validation",
            ]
        );
        for action in actions {
            let turn = json!({
                "summary": "",
                "action": match action.as_str() {
                    "mcp_call" => json!({"type": action, "tool": "project.describe", "arguments": {}}),
                    "code_write" => json!({"type": action, "path": "game/a.rs", "text": ""}),
                    "runtime_input" => json!({"type": action, "input": {}}),
                    "completion_gate" => json!({
                        "type": action,
                        "gate": "acceptance_criteria",
                        "status": "passed",
                        "message": "",
                    }),
                    "progress" => json!({"type": action, "step": "s", "detail": "d"}),
                    "await_user" => json!({"type": action, "question": "q"}),
                    _ => json!({"type": action}),
                }
            });
            parse_turn(&turn.to_string())
                .unwrap_or_else(|error| panic!("schema action {action} must parse: {error}"));
        }
    }

    #[test]
    fn a_backend_without_constrained_decoding_is_not_sent_a_schema() {
        struct Backend(Option<bool>);
        impl ModelBackend for Backend {
            fn label(&self) -> String {
                "test".to_owned()
            }
            fn profile(&self) -> ModelCapabilityProfile {
                let mut profile = LocalModelConfig {
                    endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
                    model: "test".to_owned(),
                }
                .capability_profile();
                profile.structured_output = self.0;
                profile
            }
            fn start_turn(
                &self,
                _prompt: String,
                _images: Vec<Vec<u8>>,
                _response_schema: Option<Value>,
            ) -> Result<Box<dyn ModelTurnTask>, NativeAgentError> {
                Err(NativeAgentError::EmptyResponse)
            }
        }
        let config = NativeModelConfig::Local(LocalModelConfig {
            endpoint: DEFAULT_LOCAL_MODEL_ENDPOINT.to_owned(),
            model: "test".to_owned(),
        });
        for (declared, expects_schema) in [(Some(true), true), (Some(false), false), (None, false)]
        {
            let runtime = NativeAgentRuntime::new(
                Box::new(Backend(declared)),
                config.clone(),
                ModelRoutingPolicy::derive(config.clone(), Vec::new(), &[]),
                HarnessPolicy::default(),
            );
            assert_eq!(
                runtime.constrained_response_schema().is_some(),
                expects_schema
            );
        }
    }

    #[test]
    fn mcp_write_classifier_keeps_read_only_tools_claim_free() {
        assert!(mcp_write("authoring.apply"));
        assert!(mcp_write("scene.set_transform"));
        assert!(!mcp_write("project.describe"));
        assert!(!mcp_write("authoring.inspect"));
        assert!(!mcp_write("authoring.preview"));
    }
}
