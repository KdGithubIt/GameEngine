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
static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(crate) enum AgentHostError {
    SessionNotFound(String),
    RunNotFound(String),
    ActiveWriterRun(String),
    InvalidTransition {
        from: AgentRunState,
        to: AgentRunState,
    },
    CompletionPending,
    InvalidRelativePath(PathBuf),
    UnsupportedCodeDeletion(PathBuf),
    StaleCodeFile(PathBuf),
    NonUtf8CodeFile(PathBuf),
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
        if let Some(remaining) = self.once.get_mut(&key) {
            if *remaining > 0 {
                *remaining -= 1;
                if *remaining == 0 {
                    self.once.remove(&key);
                }
                return PermissionCheck::Granted;
            }
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
    PermissionRequested,
    PermissionResolved,
    ProviderOutput,
    CodeWorkspacePrepared,
    CodeChangesDetected,
    CodeChangesApplied,
    Validation,
    Playtest,
    Cancellation,
    Failure,
    Completion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentEvent {
    pub(crate) sequence: u64,
    pub(crate) created_unix_ms: u64,
    pub(crate) kind: AgentEventKind,
    pub(crate) message: String,
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
pub(crate) struct AgentRun {
    pub(crate) id: String,
    pub(crate) proposal_snapshot: AgentProposal,
    pub(crate) provider_label: String,
    pub(crate) state: AgentRunState,
    pub(crate) events: Vec<AgentEvent>,
    pub(crate) completion: CompletionReport,
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
    pub(crate) runs: Vec<AgentRun>,
    pub(crate) shared_with_project: bool,
}

impl AgentSession {
    fn new(title: String) -> Self {
        Self {
            schema_version: SESSION_SCHEMA_VERSION,
            id: next_id("session"),
            title,
            messages: Vec::new(),
            proposal: AgentProposal::default(),
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
            let session: AgentSession = match serde_json::from_slice(&bytes) {
                Ok(session) => session,
                Err(_) => continue,
            };
            if session.schema_version == SESSION_SCHEMA_VERSION {
                sessions.insert(session.id.clone(), session);
            }
        }
        Ok(Self {
            project_root,
            storage_root,
            sessions,
            active_writer_run: None,
            permissions,
        })
    }

    pub(crate) fn session_ids(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
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
        session.proposal = proposal;
        self.persist_session(session_id)?;
        Ok(version)
    }

    pub(crate) fn start_run(
        &mut self,
        session_id: &str,
        provider_label: impl Into<String>,
    ) -> Result<String, AgentHostError> {
        if let Some(active) = &self.active_writer_run {
            return Err(AgentHostError::ActiveWriterRun(active.clone()));
        }
        let run_id = next_id("run");
        let provider_label = provider_label.into();
        let session = self.session_mut(session_id)?;
        let mut run = AgentRun {
            id: run_id.clone(),
            proposal_snapshot: session.proposal.clone(),
            provider_label,
            state: AgentRunState::Inspecting,
            events: Vec::new(),
            completion: CompletionReport::default(),
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

    pub(crate) fn cancel_run(&mut self, run_id: &str) -> Result<(), AgentHostError> {
        self.transition_run(
            run_id,
            AgentRunState::Cancelled,
            "Run cancelled. Already-applied side effects remain reviewable.",
        )
    }

    pub(crate) fn set_completion_status(
        &mut self,
        run_id: &str,
        update: impl FnOnce(&mut CompletionReport),
    ) -> Result<(), AgentHostError> {
        let (session_id, _) = self.run_location(run_id)?;
        let session = self.session_mut(&session_id)?;
        let run = session
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .ok_or_else(|| AgentHostError::RunNotFound(run_id.to_owned()))?;
        update(&mut run.completion);
        self.persist_session(&session_id)
    }

    pub(crate) fn complete_run(&mut self, run_id: &str) -> Result<(), AgentHostError> {
        if !self.run(run_id)?.completion.is_complete() {
            return Err(AgentHostError::CompletionPending);
        }
        self.transition_run(run_id, AgentRunState::Completed, "Completion contract satisfied.")?;
        self.record_event(run_id, AgentEventKind::Completion, "Run completed.")
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

    pub(crate) fn workspace_root(&self, run_id: &str) -> PathBuf {
        self.storage_root.join("workspaces").join(run_id)
    }

    fn session_mut(&mut self, id: &str) -> Result<&mut AgentSession, AgentHostError> {
        self.sessions
            .get_mut(id)
            .ok_or_else(|| AgentHostError::SessionNotFound(id.to_owned()))
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
    let sequence = run.events.last().map_or(1, |event| event.sequence + 1);
    run.events.push(AgentEvent {
        sequence,
        created_unix_ms: unix_ms(),
        kind,
        message,
    });
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

pub(crate) struct CodeWorkspace {
    project_root: PathBuf,
    workspace_root: PathBuf,
    baseline: BTreeMap<PathBuf, Option<String>>,
}

impl CodeWorkspace {
    pub(crate) fn create(
        project_root: &Path,
        workspace_root: PathBuf,
    ) -> Result<Self, AgentHostError> {
        if workspace_root.exists() {
            fs::remove_dir_all(&workspace_root)?;
        }
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
        Ok(Self {
            project_root: project_root.to_path_buf(),
            workspace_root,
            baseline,
        })
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
            if live != change.before {
                return Err(AgentHostError::StaleCodeFile(change.relative_path.clone()));
            }
        }
        for change in changes {
            let destination = self.project_root.join(&change.relative_path);
            let after = change.after.as_deref().ok_or_else(|| {
                AgentHostError::UnsupportedCodeDeletion(change.relative_path.clone())
            })?;
            write_text_atomic(&destination, after)?;
            self.baseline
                .insert(change.relative_path.clone(), Some(after.to_owned()));
        }
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

#[derive(Debug)]
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
