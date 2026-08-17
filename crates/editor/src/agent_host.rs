//! GUI-free orchestration primitives for the Editor-owned AI Studio.
//!
//! The module deliberately has no egui/eframe dependencies. It owns the
//! project-scoped agent session/run model, permission decisions, resumable
//! persistence, an isolated code workspace, and the generic external-process
//! adapter. The UI in `ai_studio` is only a frontend over these contracts.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSION_SCHEMA_VERSION: u32 = 1;
const POLICY_SCHEMA_VERSION: u32 = 1;
const MAX_PROVIDER_EVENT_CHARS: usize = 4_000;
const MAX_PERSISTED_EVENTS_PER_RUN: usize = 512;
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) enum AgentHostError {
    SessionNotFound(String),
    RunNotFound(String),
    ActiveWriterRun(String),
    StaleProposalVersion {
        expected: u64,
        current: u64,
    },
    InvalidTransition {
        from: AgentRunState,
        to: AgentRunState,
    },
    CompletionPending,
    InvalidRelativePath(PathBuf),
    UnsupportedCodeDeletion(PathBuf),
    StaleCodeFile(PathBuf),
    NonUtf8CodeFile(PathBuf),
    MultipleActiveWriterRuns(Vec<String>),
    Serialization(serde_json::Error),
    Io(io::Error),
}

impl fmt::Display for AgentHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SessionNotFound(id) => write!(formatter, "agent session `{id}` was not found"),
            Self::RunNotFound(id) => write!(formatter, "agent run `{id}` was not found"),
            Self::ActiveWriterRun(id) => write!(
                formatter,
                "agent run `{id}` already owns the project writer slot"
            ),
            Self::StaleProposalVersion { expected, current } => write!(
                formatter,
                "proposal version {expected} was authorized, but current proposal version is {current}"
            ),
            Self::InvalidTransition { from, to } => {
                write!(formatter, "agent run cannot transition from {from:?} to {to:?}")
            }
            Self::CompletionPending => write!(
                formatter,
                "agent run cannot complete while required completion checks are unresolved"
            ),
            Self::InvalidRelativePath(path) => {
                write!(formatter, "path `{}` is outside the managed code scope", path.display())
            }
            Self::UnsupportedCodeDeletion(path) => write!(
                formatter,
                "deleting `{}` is not supported by the managed code apply path",
                path.display()
            ),
            Self::StaleCodeFile(path) => write!(
                formatter,
                "live code file `{}` changed after the run workspace was created",
                path.display()
            ),
            Self::NonUtf8CodeFile(path) => {
                write!(formatter, "managed code file `{}` is not UTF-8 text", path.display())
            }
            Self::MultipleActiveWriterRuns(run_ids) => write!(
                formatter,
                "agent state contains multiple non-terminal writer runs: {}",
                run_ids.join(", ")
            ),
            Self::Serialization(error) => write!(formatter, "agent state JSON error: {error}"),
            Self::Io(error) => write!(formatter, "agent state I/O error: {error}"),
        }
    }
}

impl std::error::Error for AgentHostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialization(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for AgentHostError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for AgentHostError {
    fn from(value: serde_json::Error) -> Self {
        Self::Serialization(value)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentCapability {
    ExternalAgentProcess,
    NetworkAccess,
    ExternalAssetAcquisition,
    RuntimeLaunch,
    RuntimeInputControl,
    FrameCapture,
    RawWorkspaceFilesystem,
    ArbitraryCommandExecution,
    CodeWorkspaceApply,
}

impl AgentCapability {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ExternalAgentProcess => "Launch external agent runtime",
            Self::NetworkAccess => "Network access",
            Self::ExternalAssetAcquisition => "External asset acquisition",
            Self::RuntimeLaunch => "Launch Play/runtime",
            Self::RuntimeInputControl => "Runtime input/control",
            Self::FrameCapture => "Frame capture",
            Self::RawWorkspaceFilesystem => "Raw workspace filesystem access",
            Self::ArbitraryCommandExecution => "Arbitrary command execution",
            Self::CodeWorkspaceApply => "Apply managed code changes",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalScope {
    Once,
    Run,
    Project,
    Deny,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionCheck {
    Granted,
    RequiresApproval,
    Denied,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProjectPermission {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPermissionPolicy {
    schema_version: u32,
    decisions: BTreeMap<AgentCapability, ProjectPermission>,
}

#[derive(Debug, Default)]
struct PermissionBroker {
    project: BTreeMap<AgentCapability, ProjectPermission>,
    run: BTreeSet<(String, AgentCapability)>,
    once: BTreeMap<(String, AgentCapability), u32>,
}

impl PermissionBroker {
    fn load(path: &Path) -> Result<Self, AgentHostError> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        let persisted: PersistedPermissionPolicy = serde_json::from_slice(&bytes)?;
        if persisted.schema_version != POLICY_SCHEMA_VERSION {
            return Ok(Self::default());
        }
        Ok(Self {
            project: persisted.decisions,
            run: BTreeSet::new(),
            once: BTreeMap::new(),
        })
    }

    fn save(&self, path: &Path) -> Result<(), AgentHostError> {
        let persisted = PersistedPermissionPolicy {
            schema_version: POLICY_SCHEMA_VERSION,
            decisions: self.project.clone(),
        };
        write_json_atomic(path, &persisted)
    }

    fn check(&mut self, run_id: &str, capability: AgentCapability) -> PermissionCheck {
        if let Some(project) = self.project.get(&capability) {
            return match project {
                ProjectPermission::Allow => PermissionCheck::Granted,
                ProjectPermission::Deny => PermissionCheck::Denied,
            };
        }
        let key = (run_id.to_owned(), capability);
        if self.run.contains(&key) {
            return PermissionCheck::Granted;
        }
        if let Some(remaining) = self.once.get_mut(&key)
            && *remaining > 0
        {
            *remaining -= 1;
            if *remaining == 0 {
                self.once.remove(&key);
            }
            return PermissionCheck::Granted;
        }
        PermissionCheck::RequiresApproval
    }

    fn resolve(
        &mut self,
        run_id: &str,
        capability: AgentCapability,
        scope: ApprovalScope,
    ) {
        let key = (run_id.to_owned(), capability);
        match scope {
            ApprovalScope::Once => {
                self.once.insert(key, 1);
            }
            ApprovalScope::Run => {
                self.run.insert(key);
            }
            ApprovalScope::Project => {
                self.project.insert(capability, ProjectPermission::Allow);
            }
            ApprovalScope::Deny => {
                self.once.remove(&key);
                self.run.remove(&key);
            }
        }
    }

    fn clear_run(&mut self, run_id: &str) {
        self.run.retain(|(id, _)| id != run_id);
        self.once.retain(|(id, _), _| id != run_id);
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConversationRole {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ConversationMessage {
    pub(crate) role: ConversationRole,
    pub(crate) text: String,
    pub(crate) created_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentProposal {
    pub(crate) version: u64,
    pub(crate) goal: String,
    pub(crate) requirements: Vec<String>,
    pub(crate) assumptions: Vec<String>,
    pub(crate) acceptance_criteria: Vec<String>,
    pub(crate) planned_project_changes: Vec<String>,
    pub(crate) planned_code_changes: Vec<String>,
    pub(crate) planned_assets: Vec<String>,
    pub(crate) validation_plan: Vec<String>,
    pub(crate) playtest_plan: Vec<String>,
    pub(crate) requested_capabilities: BTreeSet<AgentCapability>,
}

impl Default for AgentProposal {
    fn default() -> Self {
        Self {
            version: 1,
            goal: String::new(),
            requirements: Vec::new(),
            assumptions: Vec::new(),
            acceptance_criteria: Vec::new(),
            planned_project_changes: Vec::new(),
            planned_code_changes: Vec::new(),
            planned_assets: Vec::new(),
            validation_plan: Vec::new(),
            playtest_plan: Vec::new(),
            requested_capabilities: BTreeSet::new(),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentRunState {
    Inspecting,
    Planning,
    Executing,
    AwaitingUser,
    Validating,
    Playtesting,
    Evaluating,
    Repairing,
    Completed,
    Failed,
    Cancelled,
}

impl AgentRunState {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentEventKind {
    RunStarted,
    StateChanged,
    UserMessage,
    AssistantMessage,
    Proposal,
    SemanticProgress,
    ToolAction,
    PermissionRequested,
    PermissionResolved,
    ProviderOutput,
    CodeWorkspacePrepared,
    CodeChangesDetected,
    CodeChangesApplied,
    Validation,
    Playtest,
    CapturedFrame,
    Cancellation,
    Failure,
    Completion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "evidence", rename_all = "snake_case")]
pub(crate) enum AgentEventEvidence {
    Progress { step: String, detail: String },
    ToolAction { tool: String, action: String, success: Option<bool> },
    Playtest { launched: bool, interactions_passed: Option<bool> },
    CapturedFrame { artifact_id: String, width: u32, height: u32 },
    CompletionGate { gate: String, status: CompletionStatus },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentEvent {
    pub(crate) sequence: u64,
    pub(crate) created_unix_ms: u64,
    pub(crate) kind: AgentEventKind,
    pub(crate) message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) validation: Option<ManagedValidationEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence: Option<AgentEventEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedValidationGate {
    Formatting,
    Check,
    Clippy,
    Tests,
    Documentation,
}

impl ManagedValidationGate {
    fn label(self) -> &'static str {
        match self {
            Self::Formatting => "formatting",
            Self::Check => "check",
            Self::Clippy => "clippy",
            Self::Tests => "tests",
            Self::Documentation => "documentation",
        }
    }

    fn cargo_arguments(self) -> &'static [&'static str] {
        match self {
            Self::Formatting => &["fmt", "--all", "--check"],
            Self::Check => &["check", "--all-targets"],
            Self::Clippy => &["clippy", "--all-targets", "--", "-D", "warnings"],
            Self::Tests => &["test", "--all-targets"],
            Self::Documentation => &["doc", "--no-deps"],
        }
    }
}

const STANDARD_VALIDATION_GATES: [ManagedValidationGate; 5] = [
    ManagedValidationGate::Formatting,
    ManagedValidationGate::Check,
    ManagedValidationGate::Clippy,
    ManagedValidationGate::Tests,
    ManagedValidationGate::Documentation,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedValidationGateStatus {
    Pending,
    Running,
    Passed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedValidationFailureKind {
    Preparation,
    Spawn,
    ProcessExit,
    Poll,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedValidationFailure {
    pub(crate) kind: ManagedValidationFailureKind,
    pub(crate) exit_code: Option<i32>,
    pub(crate) message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedValidationGateResult {
    pub(crate) gate: ManagedValidationGate,
    pub(crate) status: ManagedValidationGateStatus,
    pub(crate) exit_code: Option<i32>,
    pub(crate) failure: Option<ManagedValidationFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedValidationAttemptStatus {
    Running,
    Passed,
    Failed,
    Cancelled,
    Interrupted,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedValidationAttempt {
    pub(crate) id: String,
    pub(crate) gate_results: Vec<ManagedValidationGateResult>,
    pub(crate) unmanaged_plan_items: usize,
    pub(crate) status: ManagedValidationAttemptStatus,
    pub(crate) started_unix_ms: u64,
    pub(crate) finished_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum ManagedValidationEvent {
    Started {
        attempt_id: String,
        gates: Vec<ManagedValidationGate>,
        unmanaged_plan_items: usize,
    },
    GateStarted {
        attempt_id: String,
        gate: ManagedValidationGate,
    },
    GateFinished {
        attempt_id: String,
        result: ManagedValidationGateResult,
    },
    Finished {
        attempt_id: String,
        status: ManagedValidationAttemptStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompletionStatus {
    Pending,
    Passed,
    Failed,
    NotApplicable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CompletionReport {
    pub(crate) acceptance_criteria: CompletionStatus,
    pub(crate) authoring_validation: CompletionStatus,
    pub(crate) source_validation: CompletionStatus,
    pub(crate) play_launch: CompletionStatus,
    pub(crate) frame_capture: CompletionStatus,
    pub(crate) visual_evaluation: CompletionStatus,
    pub(crate) interaction_scenarios: CompletionStatus,
}

impl Default for CompletionReport {
    fn default() -> Self {
        Self {
            acceptance_criteria: CompletionStatus::Pending,
            authoring_validation: CompletionStatus::Pending,
            source_validation: CompletionStatus::Pending,
            play_launch: CompletionStatus::Pending,
            frame_capture: CompletionStatus::Pending,
            visual_evaluation: CompletionStatus::Pending,
            interaction_scenarios: CompletionStatus::Pending,
        }
    }
}

impl CompletionReport {
    fn is_complete(&self) -> bool {
        [
            self.acceptance_criteria,
            self.authoring_validation,
            self.source_validation,
            self.play_launch,
            self.frame_capture,
            self.visual_evaluation,
            self.interaction_scenarios,
        ]
        .into_iter()
        .all(|status| matches!(status, CompletionStatus::Passed | CompletionStatus::NotApplicable))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CodeCheckpointChange {
    pub(crate) relative_path: PathBuf,
    pub(crate) before: Option<String>,
    pub(crate) after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CodeCheckpoint {
    pub(crate) id: String,
    pub(crate) changes: Vec<CodeCheckpointChange>,
    pub(crate) created_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentRun {
    pub(crate) id: String,
    pub(crate) proposal_snapshot: AgentProposal,
    pub(crate) provider_label: String,
    pub(crate) state: AgentRunState,
    pub(crate) events: Vec<AgentEvent>,
    pub(crate) completion: CompletionReport,
    #[serde(default)]
    pub(crate) validation_attempts: Vec<ManagedValidationAttempt>,
    #[serde(default)]
    pub(crate) code_checkpoints: Vec<CodeCheckpoint>,
    pub(crate) started_unix_ms: u64,
    pub(crate) finished_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentSession {
    schema_version: u32,
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) messages: Vec<ConversationMessage>,
    pub(crate) proposal: AgentProposal,
    #[serde(default)]
    pub(crate) proposal_history: Vec<AgentProposal>,
    pub(crate) runs: Vec<AgentRun>,
    pub(crate) shared_with_project: bool,
}

impl AgentSession {
    fn new(title: String) -> Self {
        let proposal = AgentProposal::default();
        Self {
            schema_version: SESSION_SCHEMA_VERSION,
            id: next_id("session"),
            title,
            messages: Vec::new(),
            proposal: proposal.clone(),
            proposal_history: vec![proposal],
            runs: Vec::new(),
            shared_with_project: false,
        }
    }
}

pub(crate) struct AgentHost {
    project_root: PathBuf,
    storage_root: PathBuf,
    sessions: BTreeMap<String, AgentSession>,
    active_writer_run: Option<String>,
    permissions: PermissionBroker,
    active_validation: Option<ManagedValidationProcess>,
}

impl AgentHost {
    pub(crate) fn open(project_root: PathBuf, storage_root: PathBuf) -> Result<Self, AgentHostError> {
        fs::create_dir_all(storage_root.join("sessions"))?;
        let policy_path = storage_root.join("permissions.json");
        let permissions = PermissionBroker::load(&policy_path)?;
        let mut sessions = BTreeMap::new();
        for entry in fs::read_dir(storage_root.join("sessions"))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension() != Some(OsStr::new("json")) {
                continue;
            }
            let bytes = fs::read(&path)?;
            let mut session: AgentSession = match serde_json::from_slice(&bytes) {
                Ok(session) => session,
                Err(_) => continue,
            };
            if session.schema_version == SESSION_SCHEMA_VERSION {
                if session.proposal_history.is_empty() {
                    session.proposal_history.push(session.proposal.clone());
                }
                sessions.insert(session.id.clone(), session);
            }
        }
        let (active_writer_run, recovered_sessions) = recover_persisted_runs(&mut sessions)?;
        let host = Self {
            project_root,
            storage_root,
            sessions,
            active_writer_run,
            permissions,
            active_validation: None,
        };
        for id in recovered_sessions {
            host.persist_session(&id)?;
        }
        Ok(host)
    }

    pub(crate) fn session_ids(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    pub(crate) fn active_writer_run_id(&self) -> Option<&str> {
        self.active_writer_run.as_deref()
    }

    pub(crate) fn session(&self, id: &str) -> Result<&AgentSession, AgentHostError> {
        self.sessions
            .get(id)
            .ok_or_else(|| AgentHostError::SessionNotFound(id.to_owned()))
    }

    pub(crate) fn create_session(&mut self, title: impl Into<String>) -> Result<String, AgentHostError> {
        let session = AgentSession::new(title.into());
        let id = session.id.clone();
        self.sessions.insert(id.clone(), session);
        self.persist_session(&id)?;
        Ok(id)
    }

    pub(crate) fn append_message(
        &mut self,
        session_id: &str,
        role: ConversationRole,
        text: impl Into<String>,
    ) -> Result<(), AgentHostError> {
        let text = text.into();
        let session = self.session_mut(session_id)?;
        session.messages.push(ConversationMessage {
            role: role.clone(),
            text: text.clone(),
            created_unix_ms: unix_ms(),
        });
        if let Some(run) = session.runs.last_mut().filter(|run| !run.state.is_terminal()) {
            let kind = match role {
                ConversationRole::User => AgentEventKind::UserMessage,
                ConversationRole::Assistant | ConversationRole::System => AgentEventKind::AssistantMessage,
            };
            push_event(run, kind, text);
        }
        self.persist_session(session_id)
    }

    pub(crate) fn update_proposal(
        &mut self,
        session_id: &str,
        mut proposal: AgentProposal,
    ) -> Result<u64, AgentHostError> {
        let session = self.session_mut(session_id)?;
        proposal.version = session.proposal.version.saturating_add(1);
        let version = proposal.version;
        session.proposal = proposal.clone();
        session.proposal_history.push(proposal);
        self.persist_session(session_id)?;
        Ok(version)
    }

    #[cfg(test)]
    fn start_run(
        &mut self,
        session_id: &str,
        provider_label: impl Into<String>,
    ) -> Result<String, AgentHostError> {
        let version = self.session(session_id)?.proposal.version;
        self.start_run_authorized(session_id, version, provider_label)
    }

    pub(crate) fn start_run_authorized(
        &mut self,
        session_id: &str,
        authorized_proposal_version: u64,
        provider_label: impl Into<String>,
    ) -> Result<String, AgentHostError> {
        if let Some(active) = &self.active_writer_run {
            return Err(AgentHostError::ActiveWriterRun(active.clone()));
        }
        let run_id = next_id("run");
        let provider_label = provider_label.into();
        let session = self.session_mut(session_id)?;
        if session.proposal.version != authorized_proposal_version {
            return Err(AgentHostError::StaleProposalVersion {
                expected: authorized_proposal_version,
                current: session.proposal.version,
            });
        }
        let mut run = AgentRun {
            id: run_id.clone(),
            proposal_snapshot: session.proposal.clone(),
            provider_label,
            state: AgentRunState::Inspecting,
            events: Vec::new(),
            completion: CompletionReport::default(),
            validation_attempts: Vec::new(),
            code_checkpoints: Vec::new(),
            started_unix_ms: unix_ms(),
            finished_unix_ms: None,
        };
        let snapshot_version = run.proposal_snapshot.version;
        push_event(
            &mut run,
            AgentEventKind::RunStarted,
            format!("Run started from immutable proposal version {snapshot_version}."),
        );
        session.runs.push(run);
        self.active_writer_run = Some(run_id.clone());
        self.persist_session(session_id)?;
        Ok(run_id)
    }

    pub(crate) fn run(&self, run_id: &str) -> Result<&AgentRun, AgentHostError> {
        self.sessions
            .values()
            .flat_map(|session| session.runs.iter())
            .find(|run| run.id == run_id)
            .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))
    }

    pub(crate) fn transition_run(
        &mut self,
        run_id: &str,
        state: AgentRunState,
        message: impl Into<String>,
    ) -> Result<(), AgentHostError> {
        let (session_id, current) = self.run_location(run_id)?;
        if !valid_transition(current, state) {
            return Err(AgentHostError::InvalidTransition {
                from: current,
                to: state,
            });
        }
        {
            let session = self.session_mut(&session_id)?;
            let run = session
                .runs
                .iter_mut()
                .find(|run| run.id == run_id)
                .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
            run.state = state;
            push_event(run, AgentEventKind::StateChanged, message.into());
            if state.is_terminal() {
                run.finished_unix_ms = Some(unix_ms());
            }
        }
        if state.is_terminal() {
            self.release_writer(run_id);
        }
        self.persist_session(&session_id)
    }

    pub(crate) fn record_event(
        &mut self,
        run_id: &str,
        kind: AgentEventKind,
        message: impl Into<String>,
    ) -> Result<(), AgentHostError> {
        let (session_id, _) = self.run_location(run_id)?;
        let session = self.session_mut(&session_id)?;
        let run = session
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
        push_event(run, kind, message.into());
        self.persist_session(&session_id)
    }

    pub(crate) fn record_code_checkpoint(
        &mut self,
        run_id: &str,
        changes: &[CodeChange],
    ) -> Result<String, AgentHostError> {
        let checkpoint_id = next_id("code-checkpoint");
        let (session_id, _) = self.run_location(run_id)?;
        let run = self.run_mut_in_session(&session_id, run_id)?;
        run.code_checkpoints.push(CodeCheckpoint {
            id: checkpoint_id.clone(),
            changes: changes
                .iter()
                .map(|change| CodeCheckpointChange {
                    relative_path: change.relative_path.clone(),
                    before: change.before.clone(),
                    after: change.after.clone(),
                })
                .collect(),
            created_unix_ms: unix_ms(),
        });
        push_event(
            run,
            AgentEventKind::CodeChangesDetected,
            format!(
                "Recorded code checkpoint {checkpoint_id} with {} changed file(s).",
                changes.len()
            ),
        );
        self.persist_session(&session_id)?;
        Ok(checkpoint_id)
    }

    pub(crate) fn record_semantic_progress(
        &mut self,
        run_id: &str,
        step: impl Into<String>,
        detail: impl Into<String>,
    ) -> Result<(), AgentHostError> {
        let step = step.into();
        let detail = detail.into();
        let (session_id, _) = self.run_location(run_id)?;
        let run = self.run_mut_in_session(&session_id, run_id)?;
        push_event_with_evidence(
            run,
            AgentEventKind::SemanticProgress,
            format!("{step}: {detail}"),
            None,
            Some(AgentEventEvidence::Progress { step, detail }),
        );
        self.persist_session(&session_id)
    }

    pub(crate) fn record_tool_action(
        &mut self,
        run_id: &str,
        tool: impl Into<String>,
        action: impl Into<String>,
        success: Option<bool>,
    ) -> Result<(), AgentHostError> {
        let tool = tool.into();
        let action = action.into();
        let (session_id, _) = self.run_location(run_id)?;
        let run = self.run_mut_in_session(&session_id, run_id)?;
        push_event_with_evidence(
            run,
            AgentEventKind::ToolAction,
            format!("{tool}: {action}"),
            None,
            Some(AgentEventEvidence::ToolAction { tool, action, success }),
        );
        self.persist_session(&session_id)
    }

    pub(crate) fn record_completion_gate(
        &mut self,
        run_id: &str,
        gate: &str,
        status: CompletionStatus,
        message: impl Into<String>,
    ) -> Result<(), AgentHostError> {
        let (session_id, state) = self.run_location(run_id)?;
        {
            let run = self.run_mut_in_session(&session_id, run_id)?;
            match gate {
                "acceptance_criteria" => run.completion.acceptance_criteria = status,
                "authoring_validation" => run.completion.authoring_validation = status,
                "visual_evaluation" => run.completion.visual_evaluation = status,
                _ => return Err(AgentHostError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("completion gate `{gate}` is not provider-reportable"),
                ))),
            }
            push_event_with_evidence(
                run,
                AgentEventKind::SemanticProgress,
                message.into(),
                None,
                Some(AgentEventEvidence::CompletionGate {
                    gate: gate.to_owned(),
                    status,
                }),
            );
        }
        self.persist_session(&session_id)?;
        if status == CompletionStatus::Failed && state == AgentRunState::Evaluating {
            self.transition_run(
                run_id,
                AgentRunState::Repairing,
                format!("Completion gate `{gate}` failed; repair is required."),
            )?;
        }
        Ok(())
    }

    pub(crate) fn record_playtest_result(
        &mut self,
        run_id: &str,
        launched: bool,
        interactions_passed: Option<bool>,
        message: impl Into<String>,
    ) -> Result<(), AgentHostError> {
        let (session_id, state) = self.run_location(run_id)?;
        {
            let run = self.run_mut_in_session(&session_id, run_id)?;
            run.completion.play_launch = if launched { CompletionStatus::Passed } else { CompletionStatus::Failed };
            run.completion.interaction_scenarios = match interactions_passed {
                Some(true) => CompletionStatus::Passed,
                Some(false) => CompletionStatus::Failed,
                None if run.proposal_snapshot.playtest_plan.is_empty() => CompletionStatus::NotApplicable,
                None => CompletionStatus::Pending,
            };
            push_event_with_evidence(
                run,
                AgentEventKind::Playtest,
                message.into(),
                None,
                Some(AgentEventEvidence::Playtest { launched, interactions_passed }),
            );
        }
        self.persist_session(&session_id)?;
        if (!launched || interactions_passed == Some(false))
            && matches!(state, AgentRunState::Playtesting | AgentRunState::Evaluating)
        {
            self.transition_run(
                run_id,
                AgentRunState::Repairing,
                "Managed playtest failed; repair is required.",
            )?;
        }
        Ok(())
    }

    pub(crate) fn record_captured_frame(
        &mut self,
        run_id: &str,
        artifact_id: impl Into<String>,
        width: u32,
        height: u32,
    ) -> Result<(), AgentHostError> {
        let artifact_id = artifact_id.into();
        let (session_id, state) = self.run_location(run_id)?;
        {
            let run = self.run_mut_in_session(&session_id, run_id)?;
            run.completion.frame_capture = CompletionStatus::Passed;
            push_event_with_evidence(
                run,
                AgentEventKind::CapturedFrame,
                format!("Captured managed Play frame {artifact_id} ({width}x{height})."),
                None,
                Some(AgentEventEvidence::CapturedFrame { artifact_id, width, height }),
            );
        }
        self.persist_session(&session_id)?;
        if state == AgentRunState::Playtesting {
            self.transition_run(
                run_id,
                AgentRunState::Evaluating,
                "Managed Play frame captured; visual evaluation remains required.",
            )?;
        }
        Ok(())
    }

    pub(crate) fn cancel_run(&mut self, run_id: &str) -> Result<(), AgentHostError> {
        if self
            .active_validation
            .as_ref()
            .is_some_and(|process| process.run_id == run_id)
        {
            let mut process = self
                .active_validation
                .take()
                .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
            let exit_code = process.cancel()?.and_then(|status| status.code());
            self.finish_validation_gate(
                run_id,
                &process.attempt_id,
                process.gate_index,
                ManagedValidationGateStatus::Cancelled,
                exit_code,
                Some(ManagedValidationFailure {
                    kind: ManagedValidationFailureKind::Cancelled,
                    exit_code,
                    message: "Managed validation was cancelled by the user.".to_owned(),
                }),
            )?;
            self.finish_validation_attempt(
                run_id,
                &process.attempt_id,
                ManagedValidationAttemptStatus::Cancelled,
                CompletionStatus::Pending,
            )?;
        }
        self.record_event(
            run_id,
            AgentEventKind::Cancellation,
            "Run cancellation requested.",
        )?;
        self.transition_run(
            run_id,
            AgentRunState::Cancelled,
            "Run cancelled. Already-applied side effects remain reviewable.",
        )
    }

    pub(crate) fn begin_managed_validation(
        &mut self,
        run_id: &str,
        code_changes_present: bool,
    ) -> Result<(), AgentHostError> {
        let (_, state) = self.run_location(run_id)?;
        if !matches!(
            state,
            AgentRunState::Executing | AgentRunState::AwaitingUser | AgentRunState::Repairing
        ) {
            return Err(AgentHostError::InvalidTransition {
                from: state,
                to: AgentRunState::Validating,
            });
        }
        let proposal = self.run(run_id)?.proposal_snapshot.clone();
        let (gates, unmanaged_plan_items) =
            managed_validation_plan(&proposal, code_changes_present);
        self.transition_run(
            run_id,
            AgentRunState::Validating,
            "Engine-managed source validation started.",
        )?;

        let attempt_id = next_id("validation");
        let started_unix_ms = unix_ms();
        let gate_results = gates
            .iter()
            .copied()
            .map(|gate| ManagedValidationGateResult {
                gate,
                status: ManagedValidationGateStatus::Pending,
                exit_code: None,
                failure: None,
            })
            .collect();
        let attempt_status = if gates.is_empty() {
            ManagedValidationAttemptStatus::NotApplicable
        } else {
            ManagedValidationAttemptStatus::Running
        };
        let (session_id, _) = self.run_location(run_id)?;
        {
            let run = self.run_mut_in_session(&session_id, run_id)?;
            run.completion.source_validation = CompletionStatus::Pending;
            run.validation_attempts.push(ManagedValidationAttempt {
                id: attempt_id.clone(),
                gate_results,
                unmanaged_plan_items,
                status: attempt_status,
                started_unix_ms,
                finished_unix_ms: gates.is_empty().then_some(started_unix_ms),
            });
            push_validation_event(
                run,
                format!(
                    "Managed validation attempt started with {} allow-listed gate(s); {} plan item(s) require another capability.",
                    gates.len(), unmanaged_plan_items
                ),
                ManagedValidationEvent::Started {
                    attempt_id: attempt_id.clone(),
                    gates: gates.clone(),
                    unmanaged_plan_items,
                },
            );
        }
        self.persist_session(&session_id)?;

        if gates.is_empty() {
            self.finish_validation_without_process(run_id, &attempt_id, unmanaged_plan_items)?;
            return Ok(());
        }
        self.spawn_validation_gate(run_id, &attempt_id, 0)
    }

    pub(crate) fn poll_managed_validation(
        &mut self,
        run_id: &str,
    ) -> Result<bool, AgentHostError> {
        let Some(process) = self.active_validation.as_mut() else {
            return Ok(false);
        };
        if process.run_id != run_id {
            return Ok(false);
        }
        let poll = process.poll_exit();
        match poll {
            Ok(None) => Ok(true),
            Ok(Some(status)) => {
                let process = self
                    .active_validation
                    .take()
                    .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
                let exit_code = status.code();
                if !status.success() {
                    self.fail_managed_validation(
                        run_id,
                        &process.attempt_id,
                        process.gate_index,
                        ManagedValidationFailure {
                            kind: ManagedValidationFailureKind::ProcessExit,
                            exit_code,
                            message: format!(
                                "Managed validation gate exited unsuccessfully with {exit_code:?}."
                            ),
                        },
                    )?;
                    return Ok(false);
                }
                self.finish_validation_gate(
                    run_id,
                    &process.attempt_id,
                    process.gate_index,
                    ManagedValidationGateStatus::Passed,
                    exit_code,
                    None,
                )?;
                let next_index = process.gate_index + 1;
                let gate_count = self
                    .validation_attempt(run_id, &process.attempt_id)?
                    .gate_results
                    .len();
                if next_index < gate_count {
                    self.spawn_validation_gate(run_id, &process.attempt_id, next_index)?;
                    Ok(self.active_validation.is_some())
                } else {
                    self.complete_managed_validation(run_id, &process.attempt_id)?;
                    Ok(false)
                }
            }
            Err(error) => {
                let process = self
                    .active_validation
                    .take()
                    .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
                self.fail_managed_validation(
                    run_id,
                    &process.attempt_id,
                    process.gate_index,
                    ManagedValidationFailure {
                        kind: ManagedValidationFailureKind::Poll,
                        exit_code: None,
                        message: format!("Managed validation process status could not be read: {error}"),
                    },
                )?;
                Ok(false)
            }
        }
    }

    pub(crate) fn complete_run(&mut self, run_id: &str) -> Result<(), AgentHostError> {
        if !self.run(run_id)?.completion.is_complete() {
            return Err(AgentHostError::CompletionPending);
        }
        self.transition_run(run_id, AgentRunState::Completed, "Completion contract satisfied.")?;
        self.record_event(run_id, AgentEventKind::Completion, "Run completed.")
    }

    fn spawn_validation_gate(
        &mut self,
        run_id: &str,
        attempt_id: &str,
        gate_index: usize,
    ) -> Result<(), AgentHostError> {
        let gate = self
            .validation_attempt(run_id, attempt_id)?
            .gate_results
            .get(gate_index)
            .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?
            .gate;
        let game_dir = self.workspace_paths(run_id)?.0.join("game");
        if !game_dir.join("Cargo.toml").is_file() {
            self.fail_managed_validation(
                run_id,
                attempt_id,
                gate_index,
                ManagedValidationFailure {
                    kind: ManagedValidationFailureKind::Preparation,
                    exit_code: None,
                    message: "Managed validation workspace is missing game/Cargo.toml.".to_owned(),
                },
            )?;
            return Ok(());
        }
        let (session_id, _) = self.run_location(run_id)?;
        {
            let run = self.run_mut_in_session(&session_id, run_id)?;
            let attempt = run
                .validation_attempts
                .iter_mut()
                .find(|attempt| attempt.id == attempt_id)
                .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
            let result = attempt
                .gate_results
                .get_mut(gate_index)
                .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
            result.status = ManagedValidationGateStatus::Running;
            push_validation_event(
                run,
                format!("Managed validation gate `{}` started.", gate.label()),
                ManagedValidationEvent::GateStarted {
                    attempt_id: attempt_id.to_owned(),
                    gate,
                },
            );
        }
        self.persist_session(&session_id)?;
        match ManagedValidationProcess::spawn(
            run_id.to_owned(),
            attempt_id.to_owned(),
            gate_index,
            gate,
            &game_dir,
        ) {
            Ok(process) => {
                self.active_validation = Some(process);
                Ok(())
            }
            Err(error) => self.fail_managed_validation(
                run_id,
                attempt_id,
                gate_index,
                ManagedValidationFailure {
                    kind: ManagedValidationFailureKind::Spawn,
                    exit_code: None,
                    message: format!("Managed validation gate could not be started: {error}"),
                },
            ),
        }
    }

    fn finish_validation_gate(
        &mut self,
        run_id: &str,
        attempt_id: &str,
        gate_index: usize,
        status: ManagedValidationGateStatus,
        exit_code: Option<i32>,
        failure: Option<ManagedValidationFailure>,
    ) -> Result<(), AgentHostError> {
        let (session_id, _) = self.run_location(run_id)?;
        {
            let run = self.run_mut_in_session(&session_id, run_id)?;
            let attempt = run
                .validation_attempts
                .iter_mut()
                .find(|attempt| attempt.id == attempt_id)
                .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
            let result = attempt
                .gate_results
                .get_mut(gate_index)
                .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
            result.status = status;
            result.exit_code = exit_code;
            result.failure = failure;
            let event_result = result.clone();
            push_validation_event(
                run,
                format!(
                    "Managed validation gate `{}` finished as {:?}.",
                    event_result.gate.label(), event_result.status
                ),
                ManagedValidationEvent::GateFinished {
                    attempt_id: attempt_id.to_owned(),
                    result: event_result,
                },
            );
        }
        self.persist_session(&session_id)
    }

    fn finish_validation_attempt(
        &mut self,
        run_id: &str,
        attempt_id: &str,
        status: ManagedValidationAttemptStatus,
        source_validation: CompletionStatus,
    ) -> Result<(), AgentHostError> {
        let (session_id, _) = self.run_location(run_id)?;
        {
            let run = self.run_mut_in_session(&session_id, run_id)?;
            let attempt = run
                .validation_attempts
                .iter_mut()
                .find(|attempt| attempt.id == attempt_id)
                .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
            attempt.status = status;
            attempt.finished_unix_ms = Some(unix_ms());
            run.completion.source_validation = source_validation;
            push_validation_event(
                run,
                format!("Managed validation attempt finished as {status:?}."),
                ManagedValidationEvent::Finished {
                    attempt_id: attempt_id.to_owned(),
                    status,
                },
            );
        }
        self.persist_session(&session_id)
    }

    fn fail_managed_validation(
        &mut self,
        run_id: &str,
        attempt_id: &str,
        gate_index: usize,
        failure: ManagedValidationFailure,
    ) -> Result<(), AgentHostError> {
        let exit_code = failure.exit_code;
        let message = failure.message.clone();
        self.finish_validation_gate(
            run_id,
            attempt_id,
            gate_index,
            ManagedValidationGateStatus::Failed,
            exit_code,
            Some(failure),
        )?;
        self.finish_validation_attempt(
            run_id,
            attempt_id,
            ManagedValidationAttemptStatus::Failed,
            CompletionStatus::Failed,
        )?;
        self.record_event(run_id, AgentEventKind::Failure, message.clone())?;
        self.transition_run(
            run_id,
            AgentRunState::Repairing,
            format!("Managed validation failed; repair is required. {message}"),
        )
    }

    fn finish_validation_without_process(
        &mut self,
        run_id: &str,
        attempt_id: &str,
        unmanaged_plan_items: usize,
    ) -> Result<(), AgentHostError> {
        let source_status = if unmanaged_plan_items == 0 {
            CompletionStatus::NotApplicable
        } else {
            CompletionStatus::Pending
        };
        self.finish_validation_attempt(
            run_id,
            attempt_id,
            ManagedValidationAttemptStatus::NotApplicable,
            source_status,
        )?;
        if unmanaged_plan_items > 0 {
            return self.transition_run(
                run_id,
                AgentRunState::AwaitingUser,
                "Validation plan contains items outside the managed allow-list; they were not executed and remain unresolved.",
            );
        }
        self.advance_after_validation(run_id)
    }

    fn complete_managed_validation(
        &mut self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<(), AgentHostError> {
        let unmanaged_plan_items = self
            .validation_attempt(run_id, attempt_id)?
            .unmanaged_plan_items;
        let source_status = if unmanaged_plan_items == 0 {
            CompletionStatus::Passed
        } else {
            CompletionStatus::Pending
        };
        self.finish_validation_attempt(
            run_id,
            attempt_id,
            ManagedValidationAttemptStatus::Passed,
            source_status,
        )?;
        if unmanaged_plan_items > 0 {
            return self.transition_run(
                run_id,
                AgentRunState::AwaitingUser,
                "Managed validation gates passed, but validation plan items outside the allow-list remain unresolved.",
            );
        }
        self.advance_after_validation(run_id)
    }

    fn advance_after_validation(&mut self, run_id: &str) -> Result<(), AgentHostError> {
        let playtest_required = !self.run(run_id)?.proposal_snapshot.playtest_plan.is_empty();
        if !playtest_required {
            let (session_id, _) = self.run_location(run_id)?;
            let run = self.run_mut_in_session(&session_id, run_id)?;
            run.completion.play_launch = CompletionStatus::NotApplicable;
            run.completion.frame_capture = CompletionStatus::NotApplicable;
            run.completion.visual_evaluation = CompletionStatus::NotApplicable;
            run.completion.interaction_scenarios = CompletionStatus::NotApplicable;
            self.persist_session(&session_id)?;
        }
        let next = if playtest_required { AgentRunState::Playtesting } else { AgentRunState::Evaluating };
        self.transition_run(
            run_id,
            next,
            if playtest_required {
                "Managed source validation passed; playtest evidence is still required."
            } else {
                "Managed source validation passed; no playtest was requested by this proposal."
            },
        )
    }

    fn validation_attempt(
        &self,
        run_id: &str,
        attempt_id: &str,
    ) -> Result<&ManagedValidationAttempt, AgentHostError> {
        self.run(run_id)?
            .validation_attempts
            .iter()
            .find(|attempt| attempt.id == attempt_id)
            .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))
    }

    pub(crate) fn check_permission(
        &mut self,
        run_id: &str,
        capability: AgentCapability,
    ) -> Result<PermissionCheck, AgentHostError> {
        self.run(run_id)?;
        let check = self.permissions.check(run_id, capability);
        if check == PermissionCheck::RequiresApproval {
            self.record_event(
                run_id,
                AgentEventKind::PermissionRequested,
                format!("Permission requested: {}.", capability.label()),
            )?;
        }
        Ok(check)
    }

    pub(crate) fn resolve_permission(
        &mut self,
        run_id: &str,
        capability: AgentCapability,
        scope: ApprovalScope,
    ) -> Result<(), AgentHostError> {
        self.run(run_id)?;
        self.permissions.resolve(run_id, capability, scope);
        if matches!(scope, ApprovalScope::Project) {
            self.permissions.save(&self.storage_root.join("permissions.json"))?;
        }
        self.record_event(
            run_id,
            AgentEventKind::PermissionResolved,
            format!("Permission {} resolved as {scope:?}.", capability.label()),
        )
    }

    pub(crate) fn export_shared_session(&mut self, session_id: &str) -> Result<PathBuf, AgentHostError> {
        let mut portable = self.session(session_id)?.clone();
        portable.shared_with_project = true;
        for run in &mut portable.runs {
            for event in &mut run.events {
                if event.kind == AgentEventKind::ProviderOutput {
                    event.message = "[provider output omitted from project-shared history]".to_owned();
                }
            }
        }
        let path = self
            .project_root
            .join(".gameengine")
            .join("ai")
            .join("sessions")
            .join(session_id)
            .join("session.json");
        write_json_atomic(&path, &portable)?;
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.shared_with_project = true;
        }
        self.persist_session(session_id)?;
        Ok(path)
    }

    pub(crate) fn workspace_paths(
        &self,
        run_id: &str,
    ) -> Result<(PathBuf, PathBuf), AgentHostError> {
        let (session_id, _) = self.run_location(run_id)?;
        Ok((
            self.storage_root.join("workspaces").join(&session_id),
            self.storage_root
                .join("workspace-baselines")
                .join(format!("{session_id}.json")),
        ))
    }

    pub(crate) fn store_captured_frame_artifact(
        &mut self,
        run_id: &str,
        width: u32,
        height: u32,
        png_bytes: &[u8],
    ) -> Result<(String, PathBuf), AgentHostError> {
        self.run(run_id)?;
        let artifact_id = next_id("frame");
        let directory = self.storage_root.join("artifacts").join(run_id);
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{artifact_id}.png"));
        fs::write(&path, png_bytes)?;
        self.record_captured_frame(run_id, artifact_id.clone(), width, height)?;
        Ok((artifact_id, path))
    }

    fn session_mut(&mut self, id: &str) -> Result<&mut AgentSession, AgentHostError> {
        self.sessions
            .get_mut(id)
            .ok_or_else(|| AgentHostError::SessionNotFound(id.to_owned()))
    }

    fn run_mut_in_session(
        &mut self,
        session_id: &str,
        run_id: &str,
    ) -> Result<&mut AgentRun, AgentHostError> {
        self.session_mut(session_id)?
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))
    }

    fn run_location(&self, run_id: &str) -> Result<(String, AgentRunState), AgentHostError> {
        for (session_id, session) in &self.sessions {
            if let Some(run) = session.runs.iter().find(|run| run.id == run_id) {
                return Ok((session_id.clone(), run.state));
            }
        }
        Err(AgentHostError::RunNotFound(run_id.to_owned()))
    }

    fn release_writer(&mut self, run_id: &str) {
        if self.active_writer_run.as_deref() == Some(run_id) {
            self.active_writer_run = None;
        }
        self.permissions.clear_run(run_id);
    }

    fn persist_session(&self, id: &str) -> Result<(), AgentHostError> {
        let session = self.session(id)?;
        let path = self.storage_root.join("sessions").join(format!("{id}.json"));
        write_json_atomic(&path, session)
    }
}

fn valid_transition(from: AgentRunState, to: AgentRunState) -> bool {
    if from.is_terminal() {
        return false;
    }
    if matches!(to, AgentRunState::Failed | AgentRunState::Cancelled) {
        return true;
    }
    matches!(
        (from, to),
        (AgentRunState::Inspecting, AgentRunState::Planning)
            | (AgentRunState::Inspecting, AgentRunState::Executing)
            | (AgentRunState::Inspecting, AgentRunState::AwaitingUser)
            | (AgentRunState::Planning, AgentRunState::Executing)
            | (AgentRunState::Planning, AgentRunState::AwaitingUser)
            | (AgentRunState::Executing, AgentRunState::AwaitingUser)
            | (AgentRunState::Executing, AgentRunState::Validating)
            | (AgentRunState::Executing, AgentRunState::Repairing)
            | (AgentRunState::AwaitingUser, AgentRunState::Executing)
            | (AgentRunState::AwaitingUser, AgentRunState::Validating)
            | (AgentRunState::Validating, AgentRunState::AwaitingUser)
            | (AgentRunState::Validating, AgentRunState::Playtesting)
            | (AgentRunState::Validating, AgentRunState::Evaluating)
            | (AgentRunState::Validating, AgentRunState::Repairing)
            | (AgentRunState::Playtesting, AgentRunState::Evaluating)
            | (AgentRunState::Playtesting, AgentRunState::Repairing)
            | (AgentRunState::Evaluating, AgentRunState::Repairing)
            | (AgentRunState::Evaluating, AgentRunState::Completed)
            | (AgentRunState::Repairing, AgentRunState::Executing)
            | (AgentRunState::Repairing, AgentRunState::Validating)
            | (AgentRunState::Repairing, AgentRunState::Playtesting)
            | (AgentRunState::Repairing, AgentRunState::Evaluating)
    )
}

fn push_event(run: &mut AgentRun, kind: AgentEventKind, message: String) {
    push_event_with_evidence(run, kind, message, None, None);
}

fn push_event_with_evidence(
    run: &mut AgentRun,
    kind: AgentEventKind,
    message: String,
    validation: Option<ManagedValidationEvent>,
    evidence: Option<AgentEventEvidence>,
) {
    let sequence = run.events.last().map_or(1, |event| event.sequence + 1);
    run.events.push(AgentEvent {
        sequence,
        created_unix_ms: unix_ms(),
        kind,
        message,
        validation,
        evidence,
    });
    if run.events.len() > MAX_PERSISTED_EVENTS_PER_RUN {
        let overflow = run.events.len() - MAX_PERSISTED_EVENTS_PER_RUN;
        run.events.drain(..overflow);
    }
}

fn push_validation_event(
    run: &mut AgentRun,
    message: String,
    validation: ManagedValidationEvent,
) {
    push_event_with_evidence(
        run,
        AgentEventKind::Validation,
        message,
        Some(validation),
        None,
    );
}

fn managed_validation_plan(
    proposal: &AgentProposal,
    code_changes_present: bool,
) -> (Vec<ManagedValidationGate>, usize) {
    let mut gates = BTreeSet::new();
    let mut unmanaged_plan_items = 0;
    for item in &proposal.validation_plan {
        let normalized = item.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            continue;
        }
        let all = matches!(
            normalized.as_str(),
            "all" | "full" | "full validation" | "source validation"
        );
        if all {
            gates.extend(STANDARD_VALIDATION_GATES);
            continue;
        }
        let mut matched = false;
        for (gate, needles) in [
            (
                ManagedValidationGate::Formatting,
                &[
                    "format",
                    "formatting",
                    "fmt",
                    "cargo fmt",
                    "cargo fmt --all --check",
                ][..],
            ),
            (
                ManagedValidationGate::Check,
                &["check", "cargo check", "cargo check --all-targets"][..],
            ),
            (
                ManagedValidationGate::Clippy,
                &[
                    "clippy",
                    "cargo clippy",
                    "cargo clippy --all-targets",
                    "cargo clippy --all-targets -- -d warnings",
                ][..],
            ),
            (
                ManagedValidationGate::Tests,
                &["test", "tests", "cargo test", "cargo test --all-targets"][..],
            ),
            (
                ManagedValidationGate::Documentation,
                &[
                    "documentation",
                    "docs",
                    "rustdoc",
                    "cargo doc",
                    "cargo doc --no-deps",
                ][..],
            ),
        ] {
            if needles.iter().any(|needle| normalized == *needle) {
                gates.insert(gate);
                matched = true;
            }
        }
        if !matched {
            unmanaged_plan_items += 1;
        }
    }
    if gates.is_empty() && code_changes_present {
        gates.extend(STANDARD_VALIDATION_GATES);
    }
    let ordered = STANDARD_VALIDATION_GATES
        .into_iter()
        .filter(|gate| gates.contains(gate))
        .collect();
    (ordered, unmanaged_plan_items)
}

fn recover_persisted_runs(
    sessions: &mut BTreeMap<String, AgentSession>,
) -> Result<(Option<String>, BTreeSet<String>), AgentHostError> {
    let mut active_runs = Vec::new();
    let mut recovered_sessions = BTreeSet::new();
    for session in sessions.values_mut() {
        let session_id = session.id.clone();
        for run in &mut session.runs {
            if run.state.is_terminal() {
                continue;
            }
            active_runs.push(run.id.clone());
            let Some(attempt) = run
                .validation_attempts
                .last_mut()
                .filter(|attempt| attempt.status == ManagedValidationAttemptStatus::Running)
            else {
                continue;
            };
            recovered_sessions.insert(session_id.clone());
            for result in &mut attempt.gate_results {
                if result.status == ManagedValidationGateStatus::Running {
                    result.status = ManagedValidationGateStatus::Interrupted;
                    result.failure = Some(ManagedValidationFailure {
                        kind: ManagedValidationFailureKind::Interrupted,
                        exit_code: None,
                        message: "Managed validation process did not survive the Editor restart."
                            .to_owned(),
                    });
                }
            }
            attempt.status = ManagedValidationAttemptStatus::Interrupted;
            attempt.finished_unix_ms = Some(unix_ms());
            let attempt_id = attempt.id.clone();
            run.completion.source_validation = CompletionStatus::Pending;
            push_validation_event(
                run,
                "Previous managed validation attempt was interrupted by Editor restart; no process was resumed.".to_owned(),
                ManagedValidationEvent::Finished {
                    attempt_id,
                    status: ManagedValidationAttemptStatus::Interrupted,
                },
            );
            if run.state == AgentRunState::Validating {
                run.state = AgentRunState::Repairing;
                push_event(
                    run,
                    AgentEventKind::StateChanged,
                    "Validation execution was interrupted; a new attempt is required before completion."
                        .to_owned(),
                );
            }
        }
    }
    active_runs.sort();
    active_runs.dedup();
    let active_writer_run = match active_runs.as_slice() {
        [] => None,
        [run_id] => Some(run_id.clone()),
        _ => return Err(AgentHostError::MultipleActiveWriterRuns(active_runs)),
    };
    Ok((active_writer_run, recovered_sessions))
}

struct ManagedValidationProcess {
    run_id: String,
    attempt_id: String,
    gate_index: usize,
    child: Child,
}

impl ManagedValidationProcess {
    fn spawn(
        run_id: String,
        attempt_id: String,
        gate_index: usize,
        gate: ManagedValidationGate,
        working_directory: &Path,
    ) -> io::Result<Self> {
        let child = Command::new("cargo")
            .args(gate.cargo_arguments())
            .current_dir(working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            run_id,
            attempt_id,
            gate_index,
            child,
        })
    }

    fn poll_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    fn cancel(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.child.try_wait()? {
            Some(status) => Ok(Some(status)),
            None => {
                match self.child.kill() {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
                    Err(error) => return Err(error),
                }
                self.child.wait().map(Some)
            }
        }
    }
}

impl Drop for ManagedValidationProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), AgentHostError> {
    let parent = path.parent().ok_or_else(|| {
        AgentHostError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "JSON path has no parent",
        ))
    })?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let temporary = parent.join(format!(".{}.tmp", next_id("write")));
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn next_id(prefix: &str) -> String {
    let counter = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{nanos:032x}{counter:016x}")
}

pub(crate) fn project_storage_key(project_id: &str, project_root: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    project_id.hash(&mut hasher);
    project_root.hash(&mut hasher);
    format!("{project_id}-{:016x}", hasher.finish())
}

#[derive(Debug, Clone)]
pub(crate) struct CodeChange {
    pub(crate) relative_path: PathBuf,
    pub(crate) before: Option<String>,
    pub(crate) after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedCodeWorkspaceBaseline {
    schema_version: u32,
    files: BTreeMap<PathBuf, Option<String>>,
}

pub(crate) struct CodeWorkspace {
    project_root: PathBuf,
    workspace_root: PathBuf,
    baseline_path: Option<PathBuf>,
    baseline: BTreeMap<PathBuf, Option<String>>,
}

impl CodeWorkspace {
    #[cfg(test)]
    pub(crate) fn create(
        project_root: &Path,
        workspace_root: PathBuf,
    ) -> Result<Self, AgentHostError> {
        Self::initialize(project_root, workspace_root, None)
    }

    pub(crate) fn open_or_create(
        project_root: &Path,
        workspace_root: PathBuf,
        baseline_path: PathBuf,
    ) -> Result<Self, AgentHostError> {
        if workspace_root.is_dir() && baseline_path.is_file() {
            let persisted: PersistedCodeWorkspaceBaseline =
                serde_json::from_slice(&fs::read(&baseline_path)?)?;
            if persisted.schema_version == 1 {
                return Ok(Self {
                    project_root: project_root.to_path_buf(),
                    workspace_root,
                    baseline_path: Some(baseline_path),
                    baseline: persisted.files,
                });
            }
        }
        if workspace_root.exists() {
            fs::remove_dir_all(&workspace_root)?;
        }
        if baseline_path.exists() {
            fs::remove_file(&baseline_path)?;
        }
        Self::initialize(project_root, workspace_root, Some(baseline_path))
    }

    fn initialize(
        project_root: &Path,
        workspace_root: PathBuf,
        baseline_path: Option<PathBuf>,
    ) -> Result<Self, AgentHostError> {
        fs::create_dir_all(&workspace_root)?;
        let mut baseline = BTreeMap::new();
        for relative in [
            Path::new("game"),
            Path::new("assets/scripts/rust"),
            Path::new("assets/scripts/rhai"),
        ] {
            let source = project_root.join(relative);
            if source.exists() {
                copy_code_tree(project_root, &workspace_root, relative, &mut baseline)?;
            }
        }
        let workspace = Self {
            project_root: project_root.to_path_buf(),
            workspace_root,
            baseline_path,
            baseline,
        };
        workspace.persist_baseline()?;
        Ok(workspace)
    }

    fn persist_baseline(&self) -> Result<(), AgentHostError> {
        let Some(path) = &self.baseline_path else {
            return Ok(());
        };
        write_json_atomic(
            path,
            &PersistedCodeWorkspaceBaseline {
                schema_version: 1,
                files: self.baseline.clone(),
            },
        )
    }

    pub(crate) fn root(&self) -> &Path {
        &self.workspace_root
    }

    pub(crate) fn collect_changes(&self) -> Result<Vec<CodeChange>, AgentHostError> {
        let mut paths = self.baseline.keys().cloned().collect::<BTreeSet<_>>();
        collect_workspace_code_paths(&self.workspace_root, &mut paths)?;
        let mut changes = Vec::new();
        for relative in paths {
            validate_code_relative_path(&relative)?;
            let before = self.baseline.get(&relative).cloned().flatten();
            let after = read_optional_utf8(&self.workspace_root.join(&relative), &relative)?;
            if before != after {
                changes.push(CodeChange {
                    relative_path: relative,
                    before,
                    after,
                });
            }
        }
        Ok(changes)
    }

    pub(crate) fn apply_changes(&mut self, changes: &[CodeChange]) -> Result<(), AgentHostError> {
        for change in changes {
            validate_code_relative_path(&change.relative_path)?;
            if change.after.is_none() {
                return Err(AgentHostError::UnsupportedCodeDeletion(
                    change.relative_path.clone(),
                ));
            }
            let live = read_optional_utf8(
                &self.project_root.join(&change.relative_path),
                &change.relative_path,
            )?;
            if live != change.before && live != change.after {
                return Err(AgentHostError::StaleCodeFile(change.relative_path.clone()));
            }
        }
        for change in changes {
            let destination = self.project_root.join(&change.relative_path);
            let after = change.after.as_deref().ok_or_else(|| {
                AgentHostError::UnsupportedCodeDeletion(change.relative_path.clone())
            })?;
            let live = read_optional_utf8(&destination, &change.relative_path)?;
            if live.as_deref() != Some(after) {
                write_text_atomic(&destination, after)?;
            }
            self.baseline
                .insert(change.relative_path.clone(), Some(after.to_owned()));
        }
        self.persist_baseline()?;
        Ok(())
    }
}

fn copy_code_tree(
    project_root: &Path,
    workspace_root: &Path,
    relative: &Path,
    baseline: &mut BTreeMap<PathBuf, Option<String>>,
) -> Result<(), AgentHostError> {
    validate_code_relative_path(relative)?;
    let source = project_root.join(relative);
    if source.is_file() {
        let text = read_utf8(&source, relative)?;
        let destination = workspace_root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&destination, text.as_bytes())?;
        baseline.insert(relative.to_path_buf(), Some(text));
        return Ok(());
    }
    for entry in fs::read_dir(&source)? {
        let entry = entry?;
        let child_relative = relative.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            if entry.file_name() == OsStr::new("target") || entry.file_name() == OsStr::new(".git") {
                continue;
            }
            copy_code_tree(project_root, workspace_root, &child_relative, baseline)?;
        } else if entry.file_type()?.is_file() && is_managed_code_file(&child_relative) {
            copy_code_tree(project_root, workspace_root, &child_relative, baseline)?;
        }
    }
    Ok(())
}

fn collect_workspace_code_paths(
    workspace_root: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), AgentHostError> {
    for relative in [
        Path::new("game"),
        Path::new("assets/scripts/rust"),
        Path::new("assets/scripts/rhai"),
    ] {
        let root = workspace_root.join(relative);
        if root.exists() {
            collect_files_recursive(workspace_root, &root, paths)?;
        }
    }
    Ok(())
}

fn collect_files_recursive(
    workspace_root: &Path,
    directory: &Path,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), AgentHostError> {
    if directory.is_file() {
        let relative = directory
            .strip_prefix(workspace_root)
            .map_err(|_| AgentHostError::InvalidRelativePath(directory.to_path_buf()))?
            .to_path_buf();
        validate_code_relative_path(&relative)?;
        paths.insert(relative);
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if entry.file_name() == OsStr::new("target") || entry.file_name() == OsStr::new(".git") {
                continue;
            }
            collect_files_recursive(workspace_root, &entry.path(), paths)?;
        } else if entry.file_type()?.is_file() {
            let relative = entry
                .path()
                .strip_prefix(workspace_root)
                .map_err(|_| AgentHostError::InvalidRelativePath(entry.path()))?
                .to_path_buf();
            if is_managed_code_file(&relative) {
                collect_files_recursive(workspace_root, &entry.path(), paths)?;
            }
        }
    }
    Ok(())
}


fn is_managed_code_file(path: &Path) -> bool {
    let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    if matches!(file_name, "Cargo.toml" | "Cargo.lock" | "build.rs" | "config.toml") {
        return true;
    }
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("rs" | "rhai" | "toml" | "json" | "ron" | "yaml" | "yml" | "txt" | "md" | "lock")
    )
}

fn validate_code_relative_path(path: &Path) -> Result<(), AgentHostError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir | Component::Prefix(_)))
    {
        return Err(AgentHostError::InvalidRelativePath(path.to_path_buf()));
    }
    let allowed = path.starts_with("game")
        || path.starts_with("assets/scripts/rust")
        || path.starts_with("assets/scripts/rhai");
    if !allowed {
        return Err(AgentHostError::InvalidRelativePath(path.to_path_buf()));
    }
    Ok(())
}

fn read_utf8(path: &Path, relative: &Path) -> Result<String, AgentHostError> {
    let bytes = fs::read(path)?;
    String::from_utf8(bytes).map_err(|_| AgentHostError::NonUtf8CodeFile(relative.to_path_buf()))
}

fn read_optional_utf8(path: &Path, relative: &Path) -> Result<Option<String>, AgentHostError> {
    if !path.exists() {
        return Ok(None);
    }
    read_utf8(path, relative).map(Some)
}

fn write_text_atomic(path: &Path, text: &str) -> Result<(), AgentHostError> {
    let parent = path.parent().ok_or_else(|| {
        AgentHostError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "code path has no parent",
        ))
    })?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", next_id("code")));
    {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub(crate) struct ProcessLine {
    pub(crate) stream: ProcessStream,
    pub(crate) text: String,
}

pub(crate) struct ExternalAgentProcess {
    child: Child,
    output: Receiver<ProcessLine>,
    exit_status: Option<ExitStatus>,
}

impl ExternalAgentProcess {
    pub(crate) fn spawn<I, S>(
        program: &OsStr,
        args: I,
        working_directory: &Path,
        environment: &[(OsString, OsString)],
    ) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(working_directory)
            .envs(environment.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (sender, output) = mpsc::channel();
        if let Some(stdout) = stdout {
            let sender = sender.clone();
            std::thread::Builder::new()
                .name("ai-agent-stdout".to_owned())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        let _ = sender.send(ProcessLine {
                            stream: ProcessStream::Stdout,
                            text: truncate_provider_output(line),
                        });
                    }
                })?;
        }
        if let Some(stderr) = stderr {
            std::thread::Builder::new()
                .name("ai-agent-stderr".to_owned())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        let _ = sender.send(ProcessLine {
                            stream: ProcessStream::Stderr,
                            text: truncate_provider_output(line),
                        });
                    }
                })?;
        }
        Ok(Self {
            child,
            output,
            exit_status: None,
        })
    }

    pub(crate) fn drain_output(&self) -> Vec<ProcessLine> {
        self.output.try_iter().collect()
    }

    pub(crate) fn poll_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let Some(status) = self.child.try_wait()? else {
            return Ok(None);
        };
        self.exit_status = Some(status);
        Ok(Some(status))
    }

    pub(crate) fn cancel(&mut self) -> io::Result<()> {
        if self.exit_status.is_none() {
            match self.child.kill() {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => {}
                Err(error) => return Err(error),
            }
            self.exit_status = Some(self.child.wait()?);
        }
        Ok(())
    }
}

impl Drop for ExternalAgentProcess {
    fn drop(&mut self) {
        if self.exit_status.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn truncate_provider_output(mut line: String) -> String {
    if line.chars().count() <= MAX_PROVIDER_EVENT_CHARS {
        return line;
    }
    line = line.chars().take(MAX_PROVIDER_EVENT_CHARS).collect();
    line.push_str("… [truncated]");
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("gameengine-agent-{name}-{}", next_id("test")))
    }

    #[test]
    fn run_snapshots_proposal_version() {
        let project = temp_path("snapshot-project");
        let storage = temp_path("snapshot-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Test").expect("session");
        let mut proposal = host.session(&session).expect("session").proposal.clone();
        proposal.goal = "First".to_owned();
        host.update_proposal(&session, proposal).expect("proposal");
        let run = host.start_run(&session, "test").expect("run");
        let snapshot = host.run(&run).expect("run").proposal_snapshot.clone();
        let mut proposal = host.session(&session).expect("session").proposal.clone();
        proposal.goal = "Second".to_owned();
        host.update_proposal(&session, proposal).expect("proposal");
        assert_eq!(snapshot.goal, "First");
        assert_ne!(snapshot.version, host.session(&session).expect("session").proposal.version);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn only_one_writer_run_is_active() {
        let project = temp_path("writer-project");
        let storage = temp_path("writer-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let first = host.create_session("First").expect("first session");
        let second = host.create_session("Second").expect("second session");
        let run = host.start_run(&first, "test").expect("first run");
        assert!(matches!(
            host.start_run(&second, "test"),
            Err(AgentHostError::ActiveWriterRun(_))
        ));
        host.cancel_run(&run).expect("cancel run");
        host.start_run(&second, "test").expect("second run");
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn proposal_revisions_are_retained_and_stale_go_is_rejected() {
        let project = temp_path("proposal-history-project");
        let storage = temp_path("proposal-history-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Proposal").expect("session");
        let mut proposal = host.session(&session).expect("session").proposal.clone();
        proposal.goal = "first revision".to_owned();
        let authorized = host.update_proposal(&session, proposal).expect("proposal");
        let mut revised = host.session(&session).expect("session").proposal.clone();
        revised.goal = "second revision".to_owned();
        let current = host.update_proposal(&session, revised).expect("proposal");
        let session_state = host.session(&session).expect("session");
        assert_eq!(session_state.proposal_history.len(), 3);
        assert_eq!(session_state.proposal_history[1].version, authorized);
        assert_eq!(session_state.proposal_history[2].version, current);
        assert!(matches!(
            host.start_run_authorized(&session, authorized, "test"),
            Err(AgentHostError::StaleProposalVersion { expected, current: actual })
                if expected == authorized && actual == current
        ));
        let run = host
            .start_run_authorized(&session, current, "test")
            .expect("authorized run");
        assert_eq!(host.run(&run).expect("run").proposal_snapshot.version, current);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn session_runs_share_one_code_workspace_identity() {
        let project = temp_path("session-workspace-project");
        let storage = temp_path("session-workspace-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Workspace").expect("session");
        let first = host.start_run(&session, "test").expect("first run");
        let first_paths = host.workspace_paths(&first).expect("first workspace");
        host.cancel_run(&first).expect("cancel first");
        let second = host.start_run(&session, "test").expect("second run");
        let second_paths = host.workspace_paths(&second).expect("second workspace");
        assert_eq!(first_paths, second_paths);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn reopened_session_workspace_preserves_stale_apply_baseline() {
        let project = temp_path("reopen-workspace-project");
        let workspace = temp_path("reopen-workspace");
        let baseline = temp_path("reopen-workspace-baseline.json");
        fs::create_dir_all(project.join("game/src")).expect("project tree");
        fs::write(project.join("game/src/lib.rs"), "pub fn value() -> u32 { 1 }\n")
            .expect("base file");
        {
            let code = CodeWorkspace::open_or_create(
                &project,
                workspace.clone(),
                baseline.clone(),
            )
            .expect("workspace");
            fs::write(
                code.root().join("game/src/lib.rs"),
                "pub fn value() -> u32 { 2 }\n",
            )
            .expect("workspace edit");
        }
        fs::write(project.join("game/src/lib.rs"), "pub fn value() -> u32 { 3 }\n")
            .expect("human edit");
        let mut reopened = CodeWorkspace::open_or_create(&project, workspace.clone(), baseline.clone())
            .expect("reopened workspace");
        let changes = reopened.collect_changes().expect("changes");
        assert!(matches!(
            reopened.apply_changes(&changes),
            Err(AgentHostError::StaleCodeFile(_))
        ));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(workspace);
        let _ = fs::remove_file(baseline);
    }

    #[test]
    fn code_apply_rejects_stale_live_file() {
        let project = temp_path("code-project");
        let workspace = temp_path("code-workspace");
        fs::create_dir_all(project.join("game/src")).expect("project tree");
        fs::write(project.join("game/src/lib.rs"), "pub fn value() -> u32 { 1 }\n")
            .expect("base file");
        let mut code = CodeWorkspace::create(&project, workspace.clone()).expect("workspace");
        fs::write(
            workspace.join("game/src/lib.rs"),
            "pub fn value() -> u32 { 2 }\n",
        )
        .expect("workspace edit");
        let changes = code.collect_changes().expect("changes");
        fs::write(project.join("game/src/lib.rs"), "pub fn value() -> u32 { 3 }\n")
            .expect("concurrent edit");
        assert!(matches!(
            code.apply_changes(&changes),
            Err(AgentHostError::StaleCodeFile(_))
        ));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn code_apply_retry_is_idempotent_after_success() {
        let project = temp_path("code-idempotent-project");
        let workspace = temp_path("code-idempotent-workspace");
        fs::create_dir_all(project.join("game/src")).expect("project tree");
        fs::write(project.join("game/src/lib.rs"), "pub fn value() -> u32 { 1 }\n").expect("base file");
        let mut code = CodeWorkspace::create(&project, workspace.clone()).expect("workspace");
        fs::write(workspace.join("game/src/lib.rs"), "pub fn value() -> u32 { 2 }\n").expect("workspace edit");
        let changes = code.collect_changes().expect("changes");
        code.apply_changes(&changes).expect("first apply");
        code.apply_changes(&changes).expect("idempotent retry");
        assert_eq!(fs::read_to_string(project.join("game/src/lib.rs")).expect("live file"), "pub fn value() -> u32 { 2 }\n");
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(workspace);
    }

    #[test]
    fn event_history_is_bounded_and_sequence_remains_monotonic() {
        let project = temp_path("event-bound-project");
        let storage = temp_path("event-bound-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Events").expect("session");
        let run = host.start_run(&session, "test").expect("run");
        for index in 0..(MAX_PERSISTED_EVENTS_PER_RUN + 25) {
            host.record_semantic_progress(&run, "step", format!("event {index}")).expect("event");
        }
        let events = &host.run(&run).expect("run").events;
        assert_eq!(events.len(), MAX_PERSISTED_EVENTS_PER_RUN);
        assert!(events.windows(2).all(|pair| pair[0].sequence < pair[1].sequence));
        assert!(events.first().expect("first").sequence > 1);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn managed_frame_artifact_sets_structured_completion_evidence() {
        let project = temp_path("frame-project");
        let storage = temp_path("frame-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Frame").expect("session");
        let run = host.start_run(&session, "test").expect("run");
        host.transition_run(&run, AgentRunState::Executing, "execute").expect("executing");
        host.transition_run(&run, AgentRunState::Validating, "validate").expect("validating");
        host.transition_run(&run, AgentRunState::Playtesting, "playtest").expect("playtesting");
        let (artifact_id, path) = host.store_captured_frame_artifact(&run, 2, 2, b"png").expect("artifact");
        assert!(path.is_file());
        let run_state = host.run(&run).expect("run");
        assert_eq!(run_state.completion.frame_capture, CompletionStatus::Passed);
        assert_eq!(run_state.state, AgentRunState::Evaluating);
        assert!(run_state.events.iter().any(|event| matches!(
            &event.evidence,
            Some(AgentEventEvidence::CapturedFrame { artifact_id: id, width: 2, height: 2 }) if id == &artifact_id
        )));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn managed_validation_plan_never_turns_unknown_text_into_a_command() {
        let proposal = AgentProposal {
            validation_plan: vec![
                "echo should-not-run".to_owned(),
                "powershell cargo check; Write-Host should-not-run".to_owned(),
            ],
            ..AgentProposal::default()
        };
        let (gates, unmanaged) = managed_validation_plan(&proposal, false);
        assert!(gates.is_empty());
        assert_eq!(unmanaged, 2);
    }

    #[test]
    fn code_changes_default_to_standard_managed_validation_gates() {
        let (gates, unmanaged) = managed_validation_plan(&AgentProposal::default(), true);
        assert_eq!(gates, STANDARD_VALIDATION_GATES);
        assert_eq!(unmanaged, 0);
    }

    #[test]
    fn managed_validation_without_source_work_needs_no_shell_permission() {
        let project = temp_path("validation-na-project");
        let storage = temp_path("validation-na-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Validation").expect("session");
        let run = host.start_run(&session, "test").expect("run");
        host.transition_run(&run, AgentRunState::Executing, "execute")
            .expect("executing");
        host.begin_managed_validation(&run, false)
            .expect("managed validation");
        let run_state = host.run(&run).expect("run");
        assert_eq!(run_state.state, AgentRunState::Evaluating);
        assert_eq!(
            run_state.completion.source_validation,
            CompletionStatus::NotApplicable
        );
        assert!(!run_state.events.iter().any(|event| {
            event.kind == AgentEventKind::PermissionRequested
                && event.message.contains("Arbitrary command execution")
        }));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn unmanaged_validation_plan_item_remains_pending_without_execution() {
        let project = temp_path("validation-unmanaged-project");
        let storage = temp_path("validation-unmanaged-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Validation").expect("session");
        let mut proposal = host.session(&session).expect("session").proposal.clone();
        proposal.validation_plan = vec!["powershell ./custom-validator.ps1".to_owned()];
        host.update_proposal(&session, proposal).expect("proposal");
        let run = host.start_run(&session, "test").expect("run");
        host.transition_run(&run, AgentRunState::Executing, "execute")
            .expect("executing");
        host.begin_managed_validation(&run, false)
            .expect("managed validation");
        let run_state = host.run(&run).expect("run");
        assert_eq!(run_state.state, AgentRunState::AwaitingUser);
        assert_eq!(run_state.completion.source_validation, CompletionStatus::Pending);
        assert_eq!(
            run_state.validation_attempts.last().expect("attempt").unmanaged_plan_items,
            1
        );
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn validation_failure_moves_run_to_repairing_and_blocks_completion() {
        let project = temp_path("validation-failure-project");
        let storage = temp_path("validation-failure-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Validation").expect("session");
        let run = host.start_run(&session, "test").expect("run");
        host.transition_run(&run, AgentRunState::Executing, "execute")
            .expect("executing");
        host.transition_run(&run, AgentRunState::Validating, "validate")
            .expect("validating");
        let attempt_id = next_id("validation-test");
        {
            let session_id = host.run_location(&run).expect("location").0;
            let run_state = host
                .run_mut_in_session(&session_id, &run)
                .expect("mutable run");
            run_state.validation_attempts.push(ManagedValidationAttempt {
                id: attempt_id.clone(),
                gate_results: vec![ManagedValidationGateResult {
                    gate: ManagedValidationGate::Check,
                    status: ManagedValidationGateStatus::Running,
                    exit_code: None,
                    failure: None,
                }],
                unmanaged_plan_items: 0,
                status: ManagedValidationAttemptStatus::Running,
                started_unix_ms: unix_ms(),
                finished_unix_ms: None,
            });
        }
        host.fail_managed_validation(
            &run,
            &attempt_id,
            0,
            ManagedValidationFailure {
                kind: ManagedValidationFailureKind::ProcessExit,
                exit_code: Some(1),
                message: "check failed".to_owned(),
            },
        )
        .expect("failure handling");
        let run_state = host.run(&run).expect("run");
        assert_eq!(run_state.state, AgentRunState::Repairing);
        assert_eq!(run_state.completion.source_validation, CompletionStatus::Failed);
        assert!(matches!(host.complete_run(&run), Err(AgentHostError::CompletionPending)));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn restart_marks_running_validation_as_interrupted_without_resuming_process() {
        let project = temp_path("validation-restart-project");
        let storage = temp_path("validation-restart-storage");
        fs::create_dir_all(&project).expect("test project directory");
        {
            let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
            let session = host.create_session("Validation").expect("session");
            let run = host.start_run(&session, "test").expect("run");
            host.transition_run(&run, AgentRunState::Executing, "execute")
                .expect("executing");
            host.transition_run(&run, AgentRunState::Validating, "validate")
                .expect("validating");
            let session_id = host.run_location(&run).expect("location").0;
            let run_state = host
                .run_mut_in_session(&session_id, &run)
                .expect("mutable run");
            run_state.validation_attempts.push(ManagedValidationAttempt {
                id: "validation_persisted".to_owned(),
                gate_results: vec![ManagedValidationGateResult {
                    gate: ManagedValidationGate::Check,
                    status: ManagedValidationGateStatus::Running,
                    exit_code: None,
                    failure: None,
                }],
                unmanaged_plan_items: 0,
                status: ManagedValidationAttemptStatus::Running,
                started_unix_ms: unix_ms(),
                finished_unix_ms: None,
            });
            host.persist_session(&session_id).expect("persist");
        }
        let host = AgentHost::open(project.clone(), storage.clone()).expect("reopen");
        let run_state = host
            .sessions
            .values()
            .flat_map(|session| session.runs.iter())
            .find(|run| run.id.starts_with("run_"))
            .expect("run");
        assert_eq!(run_state.state, AgentRunState::Repairing);
        assert_eq!(
            run_state.validation_attempts.last().expect("attempt").status,
            ManagedValidationAttemptStatus::Interrupted
        );
        assert!(host.active_validation.is_none());
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn project_shared_export_omits_provider_output() {
        let project = temp_path("shared-project");
        let storage = temp_path("shared-storage");
        fs::create_dir_all(&project).expect("test project directory");
        let mut host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        let session = host.create_session("Shared").expect("session");
        let run = host.start_run(&session, "test").expect("run");
        host.record_event(&run, AgentEventKind::ProviderOutput, "secret-looking output")
            .expect("event");
        let path = host.export_shared_session(&session).expect("export");
        let text = fs::read_to_string(path).expect("shared JSON");
        assert!(!text.contains("secret-looking output"));
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }
}
