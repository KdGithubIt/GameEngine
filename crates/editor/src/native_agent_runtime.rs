//! Governed write-capable native AgentRuntime for AI Studio (ADR 0141).
//!
//! Models choose semantic actions; the existing Agent Host and managed Editor
//! services remain authoritative for every side effect and completion gate.

use crate::agent_host::{AgentRun, AgentRunState, AgentWorkingStateUpdate, CompletionStatus};
use crate::native_agent::{LocalModelConfig, ModelCapabilityProfile, NativeAgentError, NativeModelTask};
use engine_mcp::authoring_tool_descriptors;
use serde::Deserialize;
use serde_json::{json, Value};
use std::fmt;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_TOOL_RESULT_CHARS: usize = 12_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HarnessPolicy {
    pub(crate) max_model_turns: u32,
    pub(crate) max_tool_failures: u32,
    pub(crate) repair_budget: u32,
}

impl Default for HarnessPolicy {
    fn default() -> Self {
        Self { max_model_turns: 24, max_tool_failures: 4, repair_budget: 2 }
    }
}

pub(crate) trait ModelTurnTask: Send {
    fn interrupt(&self);
    fn poll(&self) -> Option<Result<String, NativeAgentError>>;
}

impl ModelTurnTask for NativeModelTask {
    fn interrupt(&self) { NativeModelTask::interrupt(self); }
    fn poll(&self) -> Option<Result<String, NativeAgentError>> { NativeModelTask::poll(self) }
}

/// Model-provider seam. Agent policy and tools never depend on a model family.
pub(crate) trait ModelBackend: Send + Sync {
    fn label(&self) -> String;
    fn profile(&self) -> ModelCapabilityProfile;
    fn start_turn(&self, prompt: String, images: Vec<Vec<u8>>)
        -> Result<Box<dyn ModelTurnTask>, NativeAgentError>;
}

#[derive(Debug, Clone)]
struct LocalModelBackend(LocalModelConfig);

impl ModelBackend for LocalModelBackend {
    fn label(&self) -> String { format!("native:{}", self.0.model.trim()) }
    fn profile(&self) -> ModelCapabilityProfile { self.0.capability_profile() }
    fn start_turn(&self, prompt: String, images: Vec<Vec<u8>>)
        -> Result<Box<dyn ModelTurnTask>, NativeAgentError>
    {
        NativeModelTask::spawn(self.0.clone(), prompt, images)
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
    McpCall { tool: String, #[serde(default)] arguments: Value },
    CodeWrite { path: String, text: String },
    RuntimeInput { input: Value },
    CompletionGate { gate: String, status: CompletionStatus, message: String },
    Progress { step: String, detail: String },
    AwaitUser { question: String },
    ReadyForValidation,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct NativeAgentTurn {
    #[serde(default)] pub(crate) summary: String,
    #[serde(default)] pub(crate) working_state: NativeWorkingStateUpdate,
    pub(crate) action: NativeAgentAction,
}

#[derive(Debug)]
pub(crate) enum NativeAgentRuntimeError {
    Backend(NativeAgentError), Busy, TurnBudget, ToolFailureBudget,
    InvalidOutput(String), Rejected(String),
}

impl fmt::Display for NativeAgentRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(e) => e.fmt(f),
            Self::Busy => write!(f, "native AgentRuntime already has an active model turn"),
            Self::TurnBudget => write!(f, "native AgentRuntime model-turn budget exhausted"),
            Self::ToolFailureBudget => write!(f, "native AgentRuntime tool-failure budget exhausted"),
            Self::InvalidOutput(e) => write!(f, "invalid native structured output: {e}"),
            Self::Rejected(e) => write!(f, "native action rejected: {e}"),
        }
    }
}
impl std::error::Error for NativeAgentRuntimeError {}
impl From<NativeAgentError> for NativeAgentRuntimeError {
    fn from(value: NativeAgentError) -> Self { Self::Backend(value) }
}

struct Exchange { tool: String, success: bool, result: String }

pub(crate) struct NativeAgentRuntime {
    backend: Box<dyn ModelBackend>, policy: HarnessPolicy, turns: u32, failures: u32,
    exchanges: Vec<Exchange>, active: Option<Box<dyn ModelTurnTask>>,
}

impl NativeAgentRuntime {
    pub(crate) fn local(config: LocalModelConfig) -> Self {
        Self::new(Box::new(LocalModelBackend(config)), HarnessPolicy::default())
    }
    fn new(backend: Box<dyn ModelBackend>, policy: HarnessPolicy) -> Self {
        Self { backend, policy, turns: 0, failures: 0, exchanges: Vec::new(), active: None }
    }
    pub(crate) fn backend_label(&self) -> String { self.backend.label() }
    pub(crate) fn is_busy(&self) -> bool { self.active.is_some() }
    pub(crate) fn interrupt(&mut self) {
        if let Some(task) = self.active.as_ref() { task.interrupt(); }
        self.active = None;
    }
    pub(crate) fn start_turn(&mut self, run: &AgentRun, context: Option<&str>, images: Vec<Vec<u8>>)
        -> Result<(), NativeAgentRuntimeError>
    {
        if self.active.is_some() { return Err(NativeAgentRuntimeError::Busy); }
        if self.turns >= self.policy.max_model_turns { return Err(NativeAgentRuntimeError::TurnBudget); }
        let prompt = build_prompt(run, &self.backend.profile(), self.policy, &self.exchanges, context);
        self.active = Some(self.backend.start_turn(prompt, images)?);
        self.turns += 1;
        Ok(())
    }
    pub(crate) fn poll(&mut self) -> Option<Result<NativeAgentTurn, NativeAgentRuntimeError>> {
        let result = self.active.as_ref()?.poll()?;
        self.active = None;
        Some(match result { Ok(text) => parse_turn(&text), Err(e) => Err(e.into()) })
    }
    pub(crate) fn record_tool_result(&mut self, tool: impl Into<String>, success: bool, result: impl Into<String>)
        -> Result<(), NativeAgentRuntimeError>
    {
        if !success {
            self.failures += 1;
            if self.failures > self.policy.max_tool_failures { return Err(NativeAgentRuntimeError::ToolFailureBudget); }
        }
        self.exchanges.push(Exchange { tool: tool.into(), success, result: truncate(&result.into(), MAX_TOOL_RESULT_CHARS) });
        if self.exchanges.len() > 16 { self.exchanges.remove(0); }
        Ok(())
    }
    pub(crate) fn validate_action(&self, run: &AgentRun, action: &NativeAgentAction)
        -> Result<(), NativeAgentRuntimeError>
    {
        let executing = matches!(run.state, AgentRunState::Executing | AgentRunState::Repairing);
        match action {
            NativeAgentAction::McpCall { tool, .. } if !executing => return reject("MCP calls require execution/repair"),
            NativeAgentAction::McpCall { tool, .. }
                if mcp_write(tool) && run.proposal_snapshot.planned_project_changes.is_empty() =>
                    return reject("MCP mutation is outside immutable proposal project changes"),
            NativeAgentAction::CodeWrite { path, .. }
                if !executing || run.proposal_snapshot.planned_code_changes.is_empty() || !managed_code_path(Path::new(path)) =>
                    return reject("code_write is outside execution/proposal/managed workspace scope"),
            NativeAgentAction::RuntimeInput { .. }
                if !executing || run.proposal_snapshot.playtest_plan.is_empty() =>
                    return reject("runtime_input requires an immutable playtest plan"),
            NativeAgentAction::CompletionGate { gate, .. } => match gate.as_str() {
                "acceptance_criteria" | "authoring_validation" if executing => {},
                "visual_evaluation" if run.state == AgentRunState::Evaluating => {},
                _ => return reject("completion gate is not provider-reportable in this phase"),
            },
            NativeAgentAction::ReadyForValidation if !executing => return reject("validation requires execution/repair"),
            _ => {},
        }
        Ok(())
    }
}

fn reject<T>(message: &str) -> Result<T, NativeAgentRuntimeError> {
    Err(NativeAgentRuntimeError::Rejected(message.to_owned()))
}

fn build_prompt(run: &AgentRun, profile: &ModelCapabilityProfile, policy: HarnessPolicy,
    exchanges: &[Exchange], context: Option<&str>) -> String
{
    let mut prompt = format!(
        "GameEngine NativeAgentRuntime ADR0141. Return exactly one JSON object, no markdown. Do not claim side effects; AgentHost owns truth.\nImmutable proposal={}\nstate={:?}\nworking_state={}\ncompletion={}\nbackend={} model={} structured={:?} tools={:?} image={:?} reasoning={:?} context_limit={:?}\nHarnessPolicy turns={} failures={} repair_budget={}. Canonical authoring ONLY via mcp_call; source ONLY via code_write isolated AgentCodeWorkspace. Stale revision, permission, validation, import and runtime failures are evidence. ready_for_validation returns control to host validation. Never mark source_validation/play_launch/frame_capture/interaction_scenarios.\n",
        serde_json::to_string(&run.proposal_snapshot).unwrap_or_default(), run.state,
        serde_json::to_string(&run.working_state).unwrap_or_default(),
        serde_json::to_string(&run.completion).unwrap_or_default(),
        profile.backend_id, profile.model_id, profile.structured_output, profile.tool_use,
        profile.image_input, profile.reasoning, profile.context_limit,
        policy.max_model_turns, policy.max_tool_failures, policy.repair_budget);
    if let Some(context) = context { prompt.push_str(&format!("phase_context={context}\n")); }
    for exchange in exchanges { prompt.push_str(&format!("tool_result {} success={}: {}\n", exchange.tool, exchange.success, exchange.result)); }
    prompt.push_str("MCP tools:\n");
    for tool in authoring_tool_descriptors() {
        prompt.push_str(&format!("{} schema={}\n", tool.name, truncate(&tool.input_schema.to_string(), 2400)));
    }
    prompt.push_str(concat!(
        "Output {\"summary\":\"persistable facts\",\"working_state\":{},\"action\":ACTION}. ACTION one of:\n",
        "{\"type\":\"mcp_call\",\"tool\":\"...\",\"arguments\":{}}\n",
        "{\"type\":\"code_write\",\"path\":\"game/...\",\"text\":\"complete text\"}\n",
        "{\"type\":\"runtime_input\",\"input\":{...}}\n",
        "{\"type\":\"completion_gate\",\"gate\":\"acceptance_criteria|authoring_validation|visual_evaluation\",\"status\":\"passed|failed|not_applicable\",\"message\":\"...\"}\n",
        "{\"type\":\"progress\",\"step\":\"...\",\"detail\":\"...\"}\n",
        "{\"type\":\"await_user\",\"question\":\"...\"}\n{\"type\":\"ready_for_validation\"}\n"));
    prompt
}

fn parse_turn(text: &str) -> Result<NativeAgentTurn, NativeAgentRuntimeError> {
    let text = text.trim();
    let text = text.strip_prefix("```json").or_else(|| text.strip_prefix("```")).unwrap_or(text);
    let text = text.strip_suffix("```").unwrap_or(text).trim();
    serde_json::from_str(text).map_err(|e| NativeAgentRuntimeError::InvalidOutput(e.to_string()))
}
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_owned() } else { format!("{}…", text.chars().take(max).collect::<String>()) }
}
fn mcp_write(tool: &str) -> bool {
    ![".describe", ".inspect", ".find", ".list", ".search", ".validate", ".preview", ".schemas", ".capabilities"]
        .iter().any(|suffix| tool.ends_with(suffix))
        && !matches!(tool, "project.describe" | "component.schemas")
}
fn managed_code_path(path: &Path) -> bool {
    !path.is_absolute() && !path.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_)))
        && (path.starts_with("game") || path.starts_with("assets/scripts/rust") || path.starts_with("assets/scripts/rhai"))
}

/// Background live Editor MCP call. Credential lifetime is worker-only; only
/// structuredContent is returned to the runtime and therefore persistable.
pub(crate) struct NativeMcpTask {
    result: Receiver<Result<Value, String>>, interrupted: Arc<AtomicBool>,
    stream: Arc<Mutex<Option<TcpStream>>>,
}
impl NativeMcpTask {
    pub(crate) fn spawn(endpoint: String, token: String, tool: String, arguments: Value) -> Result<Self, String> {
        let (address, path) = parse_mcp_endpoint(&endpoint)?;
        let (sender, result) = mpsc::channel();
        let interrupted = Arc::new(AtomicBool::new(false));
        let stream = Arc::new(Mutex::new(None));
        let wi = Arc::clone(&interrupted); let ws = Arc::clone(&stream);
        std::thread::Builder::new().name("ai-native-mcp".to_owned()).spawn(move || {
            let _ = sender.send(call_mcp(address, &path, &token, &tool, arguments, &wi, &ws));
        }).map_err(|e| e.to_string())?;
        Ok(Self { result, interrupted, stream })
    }
    pub(crate) fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
        if let Ok(guard) = self.stream.lock() && let Some(stream) = guard.as_ref() { let _ = stream.shutdown(Shutdown::Both); }
    }
    pub(crate) fn poll(&self) -> Option<Result<Value, String>> {
        match self.result.try_recv() { Ok(v) => Some(v), Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("native MCP worker disconnected".to_owned())) }
    }
}
fn parse_mcp_endpoint(endpoint: &str) -> Result<(SocketAddr, String), String> {
    let rest = endpoint.strip_prefix("http://127.0.0.1:").ok_or_else(|| "MCP endpoint must be Editor loopback HTTP".to_owned())?;
    let (port, path) = rest.split_once('/').ok_or_else(|| "MCP endpoint path missing".to_owned())?;
    if path != "mcp" { return Err("MCP endpoint must target /mcp".to_owned()); }
    Ok((format!("127.0.0.1:{port}").parse::<SocketAddr>().map_err(|e| e.to_string())?, "/mcp".to_owned()))
}
fn call_mcp(address: SocketAddr, path: &str, token: &str, tool: &str, arguments: Value,
    interrupted: &AtomicBool, active: &Mutex<Option<TcpStream>>) -> Result<Value, String>
{
    if interrupted.load(Ordering::Acquire) { return Err("native MCP call interrupted".to_owned()); }
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(35))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(35))).map_err(|e| e.to_string())?;
    if let Ok(mut guard) = active.lock() { *guard = stream.try_clone().ok(); }
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":arguments}})).map_err(|e| e.to_string())?;
    write!(stream, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: {MCP_PROTOCOL_VERSION}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).map_err(|e| e.to_string())?;
    stream.write_all(&body).map_err(|e| e.to_string())?; stream.flush().map_err(|e| e.to_string())?;
    let mut response = Vec::new(); stream.take(2 * 1024 * 1024).read_to_end(&mut response).map_err(|e| e.to_string())?;
    if let Ok(mut guard) = active.lock() { *guard = None; }
    if interrupted.load(Ordering::Acquire) { return Err("native MCP call interrupted".to_owned()); }
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| "invalid MCP HTTP response".to_owned())?;
    let header = std::str::from_utf8(&response[..split]).map_err(|_| "invalid MCP headers".to_owned())?;
    if !header.lines().next().is_some_and(|line| line.contains(" 200 ")) { return Err(format!("MCP HTTP failure: {}", header.lines().next().unwrap_or("unknown"))); }
    let rpc: Value = serde_json::from_slice(&response[split + 4..]).map_err(|e| e.to_string())?;
    if let Some(error) = rpc.get("error") { return Err(format!("MCP JSON-RPC error: {error}")); }
    let result = rpc.get("result").cloned().ok_or_else(|| "MCP result missing".to_owned())?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) { return Err(result.to_string()); }
    Ok(result.get("structuredContent").cloned().unwrap_or(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_requires_structured_action() {
        assert!(parse_turn("I changed files").is_err());
        assert!(parse_turn(r#"{"action":{"type":"mcp_call","tool":"project.describe","arguments":{}}}"#).is_ok());
    }
    #[test]
    fn mcp_endpoint_is_loopback_only() {
        assert!(parse_mcp_endpoint("http://127.0.0.1:1234/mcp").is_ok());
        assert!(parse_mcp_endpoint("http://192.168.0.2:1234/mcp").is_err());
    }
}
