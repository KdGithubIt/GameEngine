//! GUI-free AI Studio agent orchestration contracts and managed services.
//!
//! The Editor frontend may depend on this module, but the contracts in this
//! module intentionally do not depend on egui. Structured project authoring
//! remains owned by the Editor MCP endpoint and `engine-authoring`; this module
//! owns only AI session/run orchestration, application-level permissions,
//! session persistence, managed source workspaces, provider descriptors, and
//! completion evidence.

use engine_authoring::{AuthoringPermission, AuthoringPermissions};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current portable AI session document schema.
pub const AI_SESSION_SCHEMA_VERSION: u32 = 1;
/// Current code-workspace checkpoint schema.
pub const CODE_CHECKPOINT_SCHEMA_VERSION: u32 = 1;
/// Current persisted project-level agent policy schema.
pub const AGENT_POLICY_SCHEMA_VERSION: u32 = 1;

static NEXT_GENERATED_ID: AtomicU64 = AtomicU64::new(1);

fn generated_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let serial = NEXT_GENERATED_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{nanos:032x}-{serial:016x}")
}

macro_rules! string_id {
    ($name:ident, $prefix:literal, $docs:literal) => {
        #[doc = $docs]
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Generates a process-local unique identifier suitable for persisted history.
            pub fn generate() -> Self {
                Self(generated_id($prefix))
            }

            /// Creates an identifier from an already validated persisted value.
            pub fn from_string(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the serialized identifier text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(
    AgentSessionId,
    "session",
    "Stable identifier for one project-scoped AI Studio conversation."
);
string_id!(
    AgentRunId,
    "run",
    "Stable identifier for one immutable proposal execution attempt."
);
string_id!(
    PermissionRequestId,
    "permission",
    "Stable identifier for one agent permission escalation request."
);

/// Monotonic proposal revision within one [`AgentSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProposalVersion(u64);

impl ProposalVersion {
    /// Creates a proposal version from its monotonic number.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric proposal revision.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Origin of one conversational message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRole {
    /// Human-authored request, answer, or clarification.
    User,
    /// Agent-authored question, proposal explanation, or result.
    Agent,
    /// Host-authored lifecycle or policy notice shown in the conversation.
    System,
}

/// One portable AI Studio conversation entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationMessage {
    role: ConversationRole,
    text: String,
}

impl ConversationMessage {
    /// Creates one conversation entry.
    pub fn new(role: ConversationRole, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }

    /// Returns the message origin.
    pub const fn role(&self) -> ConversationRole {
        self.role
    }

    /// Returns the message text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// One capability that belongs to the agent application layer rather than the
/// shared authoring permission vocabulary.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    /// Access remote network services.
    NetworkAccess,
    /// Search for or acquire external assets.
    ExternalAssetAcquisition,
    /// Launch the normal project runtime.
    RuntimeLaunch,
    /// Send input or other interactive control to the running project.
    RuntimeControl,
    /// Capture rendered frames for evaluation.
    FrameCapture,
    /// Use raw workspace filesystem access outside managed source operations.
    WorkspaceFilesystem,
    /// Execute commands outside the engine-managed validation allow-list.
    ArbitraryCommandExecution,
}

/// Unified capability subject used by the agent permission broker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "domain", content = "capability")]
pub enum AgentPermission {
    /// Existing shared authoring permission enforced by `engine-authoring`.
    Authoring(AuthoringPermission),
    /// AI Studio application-level capability.
    Agent(AgentCapability),
}

/// Scope chosen by the user for a permission escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// Permit exactly one matching authorization check.
    AllowOnce,
    /// Permit the capability until the current run reaches a terminal state.
    AllowForRun,
    /// Persist the capability decision for the current project host.
    AllowForProject,
    /// Reject the requested capability.
    Deny,
}

/// Pending escalation that explains why a run needs a capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    id: PermissionRequestId,
    run_id: AgentRunId,
    permission: AgentPermission,
    reason: String,
}

impl PermissionRequest {
    /// Returns the request identifier.
    pub fn id(&self) -> &PermissionRequestId {
        &self.id
    }

    /// Returns the run that requested escalation.
    pub fn run_id(&self) -> &AgentRunId {
        &self.run_id
    }

    /// Returns the requested capability.
    pub const fn permission(&self) -> AgentPermission {
        self.permission
    }

    /// Returns the human-readable escalation reason.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Application-level capability broker for AI Studio runs.
///
/// Project grants are durable policy. Run and one-shot grants are intentionally
/// transient and are cleared when their run ends.
#[derive(Debug, Default)]
pub struct AgentPermissionBroker {
    project_grants: BTreeSet<AgentPermission>,
    run_grants: BTreeMap<AgentRunId, BTreeSet<AgentPermission>>,
    once_grants: BTreeMap<AgentRunId, BTreeMap<AgentPermission, u32>>,
    pending: BTreeMap<PermissionRequestId, PermissionRequest>,
}

impl AgentPermissionBroker {
    /// Restores the persistent project policy without restoring transient run grants.
    pub fn with_project_grants(project_grants: BTreeSet<AgentPermission>) -> Self {
        Self {
            project_grants,
            ..Self::default()
        }
    }

    /// Returns the durable project-level capability decisions.
    pub fn project_grants(&self) -> &BTreeSet<AgentPermission> {
        &self.project_grants
    }

    /// Returns all unresolved permission escalation requests.
    pub fn pending_requests(&self) -> impl Iterator<Item = &PermissionRequest> {
        self.pending.values()
    }

    /// Explicitly updates one durable project-level capability toggle.
    pub fn set_project_grant(&mut self, permission: AgentPermission, allowed: bool) {
        if allowed {
            self.project_grants.insert(permission);
        } else {
            self.project_grants.remove(&permission);
        }
    }

    /// Returns whether a permission is currently available without consuming
    /// an allow-once grant.
    pub fn is_allowed(&self, run_id: &AgentRunId, permission: AgentPermission) -> bool {
        self.project_grants.contains(&permission)
            || self
                .run_grants
                .get(run_id)
                .is_some_and(|grants| grants.contains(&permission))
            || self
                .once_grants
                .get(run_id)
                .and_then(|grants| grants.get(&permission))
                .is_some_and(|count| *count > 0)
    }

    /// Authorizes one operation and consumes an allow-once grant when that is
    /// the only matching grant.
    pub fn authorize(&mut self, run_id: &AgentRunId, permission: AgentPermission) -> bool {
        if self.project_grants.contains(&permission)
            || self
                .run_grants
                .get(run_id)
                .is_some_and(|grants| grants.contains(&permission))
        {
            return true;
        }

        let Some(grants) = self.once_grants.get_mut(run_id) else {
            return false;
        };
        let Some(count) = grants.get_mut(&permission) else {
            return false;
        };
        if *count == 0 {
            return false;
        }
        *count -= 1;
        true
    }

    /// Creates a reviewable escalation request unless the permission is already granted.
    pub fn request(
        &mut self,
        run_id: AgentRunId,
        permission: AgentPermission,
        reason: impl Into<String>,
    ) -> Option<PermissionRequestId> {
        if self.is_allowed(&run_id, permission) {
            return None;
        }

        let id = PermissionRequestId::generate();
        self.pending.insert(
            id.clone(),
            PermissionRequest {
                id: id.clone(),
                run_id,
                permission,
                reason: reason.into(),
            },
        );
        Some(id)
    }

    /// Applies the user's decision to one pending request.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] if the request no longer exists.
    pub fn resolve(
        &mut self,
        request_id: &PermissionRequestId,
        scope: ApprovalScope,
    ) -> Result<PermissionRequest, AgentHostError> {
        let request = self.pending.remove(request_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.permission_request_not_found",
                format!("permission request `{request_id}` does not exist"),
            )
        })?;

        match scope {
            ApprovalScope::AllowOnce => {
                *self
                    .once_grants
                    .entry(request.run_id.clone())
                    .or_default()
                    .entry(request.permission)
                    .or_default() += 1;
            }
            ApprovalScope::AllowForRun => {
                self.run_grants
                    .entry(request.run_id.clone())
                    .or_default()
                    .insert(request.permission);
            }
            ApprovalScope::AllowForProject => {
                self.project_grants.insert(request.permission);
            }
            ApprovalScope::Deny => {}
        }
        Ok(request)
    }

    /// Drops every transient decision associated with a terminal run.
    pub fn finish_run(&mut self, run_id: &AgentRunId) {
        self.run_grants.remove(run_id);
        self.once_grants.remove(run_id);
        self.pending.retain(|_, request| &request.run_id != run_id);
    }
}

/// Editable, conversation-produced proposal content before the host assigns a
/// monotonic version.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentProposalDraft {
    /// Human-visible objective for the requested work.
    pub goal: String,
    /// Requirements explicitly agreed in conversation.
    pub agreed_requirements: Vec<String>,
    /// Assumptions the run may rely on.
    pub assumptions: Vec<String>,
    /// Observable conditions required for completion.
    pub acceptance_criteria: Vec<String>,
    /// Planned project, source, and asset changes at a reviewable level.
    pub planned_changes: Vec<String>,
    /// Validation and playtest work expected before completion.
    pub validation_and_playtest_plan: Vec<String>,
    /// Capabilities expected to be required by the run.
    pub expected_capabilities: Vec<AgentPermission>,
}

/// Immutable versioned proposal stored in the session history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProposal {
    version: ProposalVersion,
    draft: AgentProposalDraft,
}

impl AgentProposal {
    /// Returns the immutable proposal revision.
    pub const fn version(&self) -> ProposalVersion {
        self.version
    }

    /// Returns the proposal body.
    pub fn draft(&self) -> &AgentProposalDraft {
        &self.draft
    }
}

/// Provider-independent host state for one agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunState {
    /// Reading the authoritative project and conversation context.
    Inspecting,
    /// Refining an implementation plan inside the approved proposal.
    Planning,
    /// Performing managed implementation work.
    Executing,
    /// Paused for a human design or permission decision.
    AwaitingUser,
    /// Running deterministic validation.
    Validating,
    /// Exercising the runtime or required interaction scenario.
    Playtesting,
    /// Interpreting deterministic and visual evidence.
    Evaluating,
    /// Repairing a recoverable implementation or validation failure.
    Repairing,
    /// All applicable completion criteria passed.
    Completed,
    /// The run ended because an unrecoverable failure was reported.
    Failed,
    /// The user or host cancelled further work.
    Cancelled,
}

impl AgentRunState {
    /// Returns whether no more run work may start from this state.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    fn can_transition_to(self, next: Self) -> bool {
        if self == next || self.is_terminal() {
            return false;
        }
        match self {
            Self::Inspecting => matches!(
                next,
                Self::Planning | Self::Executing | Self::AwaitingUser | Self::Failed | Self::Cancelled
            ),
            Self::Planning => matches!(
                next,
                Self::Executing | Self::AwaitingUser | Self::Failed | Self::Cancelled
            ),
            Self::Executing => matches!(
                next,
                Self::AwaitingUser
                    | Self::Validating
                    | Self::Playtesting
                    | Self::Evaluating
                    | Self::Repairing
                    | Self::Failed
                    | Self::Cancelled
            ),
            Self::AwaitingUser => matches!(
                next,
                Self::Inspecting
                    | Self::Planning
                    | Self::Executing
                    | Self::Validating
                    | Self::Playtesting
                    | Self::Evaluating
                    | Self::Repairing
                    | Self::Failed
                    | Self::Cancelled
            ),
            Self::Validating => matches!(
                next,
                Self::Playtesting
                    | Self::Evaluating
                    | Self::Repairing
                    | Self::Failed
                    | Self::Cancelled
            ),
            Self::Playtesting => matches!(
                next,
                Self::Evaluating | Self::Repairing | Self::Failed | Self::Cancelled
            ),
            Self::Evaluating => matches!(
                next,
                Self::Repairing | Self::Failed | Self::Cancelled
            ),
            Self::Repairing => matches!(
                next,
                Self::Inspecting
                    | Self::Planning
                    | Self::Executing
                    | Self::AwaitingUser
                    | Self::Validating
                    | Self::Playtesting
                    | Self::Evaluating
                    | Self::Failed
                    | Self::Cancelled
            ),
            Self::Completed | Self::Failed | Self::Cancelled => false,
        }
    }
}

/// Immutable input snapshot supplied when Go starts one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunInput {
    session_id: AgentSessionId,
    run_id: AgentRunId,
    provider_id: String,
    proposal: AgentProposal,
}

impl AgentRunInput {
    /// Returns the owning session.
    pub fn session_id(&self) -> &AgentSessionId {
        &self.session_id
    }

    /// Returns the new run identifier.
    pub fn run_id(&self) -> &AgentRunId {
        &self.run_id
    }

    /// Returns the provider selected when Go created this run.
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    /// Returns the exact proposal snapshot used by this run.
    pub fn proposal(&self) -> &AgentProposal {
        &self.proposal
    }
}

/// Auditable counts that distinguish managed work from elevated escape hatches.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentRunAuditSummary {
    /// Structured authoring mutations committed through MCP/authoring services.
    pub authoring_operations: u64,
    /// Source files changed through the managed code workspace.
    pub code_changes: u64,
    /// External assets acquired through typed provider services.
    pub external_acquisitions: u64,
    /// Raw filesystem operations performed under an elevated capability.
    pub raw_filesystem_accesses: u64,
    /// Custom commands executed outside the managed validation allow-list.
    pub custom_commands: u64,
    /// Permission escalations requested during the run.
    pub permission_escalations: u64,
    /// Deterministic validation results recorded during the run.
    pub validation_records: u64,
    /// Runtime playtest observations recorded during the run.
    pub playtest_records: u64,
}

/// Operation class used to update one run's audit summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAuditOperation {
    /// Structured project authoring mutation.
    AuthoringOperation,
    /// External asset acquisition.
    ExternalAcquisition,
    /// Elevated raw filesystem access.
    RawFilesystemAccess,
    /// Elevated custom command execution.
    CustomCommand,
}

/// One provider-independent execution record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRun {
    input: AgentRunInput,
    state: AgentRunState,
    changed_source_files: BTreeSet<PathBuf>,
    audit: AgentRunAuditSummary,
}

impl AgentRun {
    /// Returns the immutable run input.
    pub fn input(&self) -> &AgentRunInput {
        &self.input
    }

    /// Returns the current host lifecycle state.
    pub const fn state(&self) -> AgentRunState {
        self.state
    }

    /// Returns source files attributed to this run.
    pub fn changed_source_files(&self) -> &BTreeSet<PathBuf> {
        &self.changed_source_files
    }

    /// Returns the current audit counters for managed and elevated operations.
    pub fn audit(&self) -> &AgentRunAuditSummary {
        &self.audit
    }
}

/// Provider-independent structured event shown in the AI Studio audit timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "detail")]
pub enum AgentEventKind {
    /// A new proposal version became current.
    ProposalUpdated(ProposalVersion),
    /// Go created a run from the specified proposal version.
    RunStarted {
        /// New run identifier.
        run_id: AgentRunId,
        /// Proposal revision frozen into the run input.
        proposal_version: ProposalVersion,
    },
    /// Host lifecycle state changed.
    StateChanged {
        /// Affected run.
        run_id: AgentRunId,
        /// Previous state.
        from: AgentRunState,
        /// New state.
        to: AgentRunState,
    },
    /// One semantic step started.
    StepStarted(String),
    /// One semantic step completed.
    StepCompleted(String),
    /// A managed or provider tool call was issued.
    ToolCall(String),
    /// Reviewable project or source changes were prepared.
    ChangePreview(String),
    /// Reviewable changes were applied through their owning service.
    ChangeCommitted(String),
    /// A permission escalation requires a human response.
    PermissionRequested(PermissionRequestId),
    /// A permission escalation was resolved.
    PermissionResolved {
        /// Request identifier.
        request_id: PermissionRequestId,
        /// User-selected approval scope.
        scope: ApprovalScope,
    },
    /// Deterministic validation result.
    Validation {
        /// Validator or validation group.
        name: String,
        /// Whether the validator passed.
        passed: bool,
    },
    /// Runtime playtest observation.
    Playtest {
        /// Scenario description.
        scenario: String,
        /// Whether the scenario passed.
        passed: bool,
    },
    /// A recoverable problem triggered another repair cycle.
    Repair(String),
    /// The run was cancelled and no further work may start.
    Cancelled(AgentRunId),
    /// The run failed irrecoverably.
    Failed {
        /// Failed run.
        run_id: AgentRunId,
        /// Human-readable failure summary.
        message: String,
    },
    /// The run completed after all applicable completion gates passed.
    Completed(AgentRunId),
}

/// Sequenced event stored in one session's portable audit history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEvent {
    sequence: u64,
    kind: AgentEventKind,
}

impl AgentEvent {
    /// Returns the monotonic session event number.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the structured event payload.
    pub fn kind(&self) -> &AgentEventKind {
        &self.kind
    }
}

/// Whether one AI session remains user-private or is deliberately shared with
/// the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionVisibility {
    /// Store outside the project working tree in user-local application data.
    LocalPrivate,
    /// Store portable history beneath `.gameengine/ai/sessions`.
    ProjectShared,
}

impl Default for SessionVisibility {
    fn default() -> Self {
        Self::LocalPrivate
    }
}

/// Portable project-scoped conversation, proposal, run, and audit history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    id: AgentSessionId,
    title: String,
    visibility: SessionVisibility,
    conversation: Vec<ConversationMessage>,
    proposals: Vec<AgentProposal>,
    runs: Vec<AgentRun>,
    events: Vec<AgentEvent>,
}

impl AgentSession {
    /// Returns the session identifier.
    pub fn id(&self) -> &AgentSessionId {
        &self.id
    }

    /// Returns the current display title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the persistence visibility.
    pub const fn visibility(&self) -> SessionVisibility {
        self.visibility
    }

    /// Returns the portable conversation history.
    pub fn conversation(&self) -> &[ConversationMessage] {
        &self.conversation
    }

    /// Returns every proposal revision in chronological order.
    pub fn proposals(&self) -> &[AgentProposal] {
        &self.proposals
    }

    /// Returns the current proposal revision, if conversation has produced one.
    pub fn current_proposal(&self) -> Option<&AgentProposal> {
        self.proposals.last()
    }

    /// Returns every run in creation order.
    pub fn runs(&self) -> &[AgentRun] {
        &self.runs
    }

    /// Returns the provider-independent event timeline.
    pub fn events(&self) -> &[AgentEvent] {
        &self.events
    }

    /// Finds one run by stable identifier.
    pub fn run(&self, run_id: &AgentRunId) -> Option<&AgentRun> {
        self.runs.iter().find(|run| run.input.run_id() == run_id)
    }

    fn run_mut(&mut self, run_id: &AgentRunId) -> Option<&mut AgentRun> {
        self.runs
            .iter_mut()
            .find(|run| run.input.run_id() == run_id)
    }

    fn emit(&mut self, kind: AgentEventKind) {
        let sequence = self.events.last().map_or(1, |event| event.sequence + 1);
        self.events.push(AgentEvent { sequence, kind });
    }
}

/// Distinguishes an external agent process from a GameEngine-owned model loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRuntimeKind {
    /// Provider-managed process already owns its agent loop.
    ExternalAgentRuntime,
    /// GameEngine owns the agent loop and delegates inference to a model backend.
    NativeAgentRuntime,
}

/// Authentication lifecycle owned by one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationClass {
    /// Provider CLI or application owns its authenticated account session.
    ProviderManagedSession,
    /// GameEngine must obtain an API credential from a secure user secret store.
    ApiCredential,
    /// Local backend requires no remote authentication.
    LocalNoAuth,
    /// Enterprise environment injects or manages authentication externally.
    EnterpriseManaged,
}

/// Current connection state presented by AI Studio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderConnectionState {
    /// No connection attempt has completed.
    Disconnected,
    /// Provider login or credential setup is required.
    AuthenticationRequired,
    /// Provider is available for a run.
    Ready,
    /// Provider cannot currently be used.
    Error(String),
}

/// Provider metadata exposed without leaking credential material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    /// Stable provider identifier used in settings and run provenance.
    pub id: String,
    /// Human-readable provider name.
    pub display_name: String,
    /// Whether this provider is an external agent or native model-backed runtime.
    pub runtime_kind: ProviderRuntimeKind,
    /// Authentication lifecycle owned by the provider.
    pub authentication: AuthenticationClass,
    /// Current connection status.
    pub connection_state: ProviderConnectionState,
}

/// Structured runtime error independent of one provider's stdout/stderr format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRuntimeError {
    code: String,
    message: String,
}

impl AgentRuntimeError {
    /// Creates a provider-independent runtime error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Returns the stable error code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the human-readable explanation.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentRuntimeError {}

/// Provider-independent sink used by an agent runtime to publish semantic host events.
pub trait AgentEventSink {
    /// Publishes one semantic runtime event.
    fn emit(&mut self, event: AgentEventKind);
}

/// External or native agent-loop abstraction.
pub trait AgentRuntime {
    /// Returns provider metadata without credential contents.
    fn descriptor(&self) -> ProviderDescriptor;

    /// Starts work on one immutable run input.
    ///
    /// # Errors
    ///
    /// Returns [`AgentRuntimeError`] when the provider cannot start or execute the run.
    fn start(
        &mut self,
        input: &AgentRunInput,
        events: &mut dyn AgentEventSink,
    ) -> Result<(), AgentRuntimeError>;

    /// Requests cancellation of provider work associated with `run_id`.
    ///
    /// # Errors
    ///
    /// Returns [`AgentRuntimeError`] if cancellation cannot be requested.
    fn cancel(&mut self, run_id: &AgentRunId) -> Result<(), AgentRuntimeError>;
}

/// One native-runtime model request. Tool execution remains owned by the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    /// System or policy context supplied by the native agent loop.
    pub system: String,
    /// Conversation/context messages supplied to the model backend.
    pub messages: Vec<ConversationMessage>,
}

/// One native-runtime model response before the host interprets tool intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelResponse {
    /// Provider-produced assistant text.
    pub text: String,
}

/// Inference-only backend used by a GameEngine-owned native agent loop.
pub trait ModelBackend {
    /// Returns a stable backend identifier for settings and diagnostics.
    fn backend_id(&self) -> &str;

    /// Performs one model inference request without owning the outer tool loop.
    ///
    /// # Errors
    ///
    /// Returns [`AgentRuntimeError`] for backend connection or inference failures.
    fn infer(&mut self, request: &ModelRequest) -> Result<ModelResponse, AgentRuntimeError>;
}

/// Typed host failure for invalid session/run lifecycle operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHostError {
    code: String,
    message: String,
}

impl AgentHostError {
    /// Creates one host error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic-style code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the human-readable error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AgentHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentHostError {}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedAgentSession {
    schema_version: u32,
    session: AgentSession,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedProjectPolicy {
    schema_version: u32,
    project_grants: BTreeSet<AgentPermission>,
}

/// Project-scoped session/run host that keeps one AI writer active at a time.
#[derive(Debug)]
pub struct AgentHost {
    project_root: PathBuf,
    local_store_root: PathBuf,
    sessions: BTreeMap<AgentSessionId, AgentSession>,
    providers: BTreeMap<String, ProviderDescriptor>,
    active_writer: Option<(AgentSessionId, AgentRunId)>,
    permissions: AgentPermissionBroker,
}

impl AgentHost {
    /// Creates a host for one Editor-open project.
    pub fn new(project_root: PathBuf, local_store_root: PathBuf) -> Self {
        Self {
            project_root,
            local_store_root,
            sessions: BTreeMap::new(),
            providers: BTreeMap::new(),
            active_writer: None,
            permissions: AgentPermissionBroker::default(),
        }
    }

    /// Returns the project root governed by this host.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// Returns the layered application-level permission broker.
    pub fn permissions(&self) -> &AgentPermissionBroker {
        &self.permissions
    }

    /// Returns mutable access to the permission broker for explicit UI decisions.
    pub fn permissions_mut(&mut self) -> &mut AgentPermissionBroker {
        &mut self.permissions
    }

    /// Registers or refreshes provider metadata without storing credentials.
    pub fn register_provider(&mut self, provider: ProviderDescriptor) {
        self.providers.insert(provider.id.clone(), provider);
    }

    /// Returns registered provider descriptors in stable identifier order.
    pub fn providers(&self) -> impl Iterator<Item = &ProviderDescriptor> {
        self.providers.values()
    }

    /// Returns one registered provider descriptor.
    pub fn provider(&self, provider_id: &str) -> Option<&ProviderDescriptor> {
        self.providers.get(provider_id)
    }

    /// Returns whether the selected runtime adapter is currently ready for Go.
    pub fn provider_is_ready(&self, provider_id: &str) -> bool {
        self.provider(provider_id).is_some_and(|provider| {
            provider.connection_state == ProviderConnectionState::Ready
        })
    }

    /// Creates a local-private conversation for this project.
    pub fn create_session(&mut self, title: impl Into<String>) -> AgentSessionId {
        let id = AgentSessionId::generate();
        self.sessions.insert(
            id.clone(),
            AgentSession {
                id: id.clone(),
                title: title.into(),
                visibility: SessionVisibility::LocalPrivate,
                conversation: Vec::new(),
                proposals: Vec::new(),
                runs: Vec::new(),
                events: Vec::new(),
            },
        );
        id
    }

    /// Returns one session by identifier.
    pub fn session(&self, session_id: &AgentSessionId) -> Option<&AgentSession> {
        self.sessions.get(session_id)
    }

    /// Returns all sessions currently loaded in the host.
    pub fn sessions(&self) -> impl Iterator<Item = &AgentSession> {
        self.sessions.values()
    }

    fn session_mut(
        &mut self,
        session_id: &AgentSessionId,
    ) -> Result<&mut AgentSession, AgentHostError> {
        self.sessions.get_mut(session_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.session_not_found",
                format!("AI session `{session_id}` is not loaded"),
            )
        })
    }

    /// Appends a portable conversation message without changing an active run's
    /// immutable proposal snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when `session_id` is unknown.
    pub fn add_message(
        &mut self,
        session_id: &AgentSessionId,
        role: ConversationRole,
        text: impl Into<String>,
    ) -> Result<(), AgentHostError> {
        self.session_mut(session_id)?
            .conversation
            .push(ConversationMessage::new(role, text));
        Ok(())
    }

    /// Creates the next immutable proposal revision from collaborative conversation.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when `session_id` is unknown.
    pub fn revise_proposal(
        &mut self,
        session_id: &AgentSessionId,
        draft: AgentProposalDraft,
    ) -> Result<ProposalVersion, AgentHostError> {
        let session = self.session_mut(session_id)?;
        let version = ProposalVersion::new(
            session
                .current_proposal()
                .map_or(1, |proposal| proposal.version().get() + 1),
        );
        session.proposals.push(AgentProposal { version, draft });
        session.emit(AgentEventKind::ProposalUpdated(version));
        Ok(version)
    }

    /// Implements Go by snapshotting the current proposal into one immutable run input.
    ///
    /// At most one AI writer run may be active for the project. Human Editor
    /// mutations remain outside this writer role and use their normal revision checks.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when no proposal exists, the selected provider
    /// is missing or not ready, or another run already owns the project AI writer role.
    pub fn start_run(
        &mut self,
        session_id: &AgentSessionId,
        provider_id: &str,
    ) -> Result<AgentRunId, AgentHostError> {
        if let Some((active_session, active_run)) = &self.active_writer {
            return Err(AgentHostError::new(
                "agent.writer_busy",
                format!(
                    "AI writer is already held by session `{active_session}` run `{active_run}`"
                ),
            ));
        }

        let provider = self.providers.get(provider_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.provider_not_found",
                format!("agent provider `{provider_id}` is not registered"),
            )
        })?;
        if provider.connection_state != ProviderConnectionState::Ready {
            return Err(AgentHostError::new(
                "agent.provider_not_ready",
                format!(
                    "agent provider `{provider_id}` is not ready: {:?}",
                    provider.connection_state
                ),
            ));
        }

        let proposal = self
            .sessions
            .get(session_id)
            .and_then(AgentSession::current_proposal)
            .cloned()
            .ok_or_else(|| {
                AgentHostError::new(
                    "agent.proposal_required",
                    "Go requires a current structured proposal",
                )
            })?;

        let run_id = AgentRunId::generate();
        let input = AgentRunInput {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
            provider_id: provider_id.to_owned(),
            proposal: proposal.clone(),
        };
        let session = self.session_mut(session_id)?;
        session.runs.push(AgentRun {
            input,
            state: AgentRunState::Inspecting,
            changed_source_files: BTreeSet::new(),
            audit: AgentRunAuditSummary::default(),
        });
        session.emit(AgentEventKind::RunStarted {
            run_id: run_id.clone(),
            proposal_version: proposal.version(),
        });
        self.active_writer = Some((session_id.clone(), run_id.clone()));
        Ok(run_id)
    }

    /// Changes one run's resumable host state while preserving legal repair and
    /// AwaitingUser loops.
    ///
    /// Completion is intentionally not available through this method; use
    /// [`Self::complete_run`] so observed completion evidence is mandatory.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] for an unknown run or invalid transition.
    pub fn transition_run(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
        next: AgentRunState,
    ) -> Result<(), AgentHostError> {
        if next == AgentRunState::Completed {
            return Err(AgentHostError::new(
                "agent.completion_evidence_required",
                "use complete_run so completion evidence is evaluated",
            ));
        }

        let session = self.session_mut(session_id)?;
        let run = session.run_mut(run_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            )
        })?;
        let from = run.state;
        if !from.can_transition_to(next) {
            return Err(AgentHostError::new(
                "agent.invalid_run_transition",
                format!("cannot transition run `{run_id}` from {from:?} to {next:?}"),
            ));
        }
        run.state = next;
        session.emit(AgentEventKind::StateChanged {
            run_id: run_id.clone(),
            from,
            to: next,
        });
        if next.is_terminal() {
            self.finish_writer(session_id, run_id);
        }
        Ok(())
    }

    /// Records a source path as run provenance after a managed code service applies it.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the session or run is unknown.
    pub fn record_changed_source_file(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
        path: PathBuf,
    ) -> Result<(), AgentHostError> {
        let path = validated_relative_path(&path).map_err(|error| {
            AgentHostError::new("agent.code_path_invalid", error.to_string())
        })?;
        let session = self.session_mut(session_id)?;
        let run = session.run_mut(run_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            )
        })?;
        if run.changed_source_files.insert(path) {
            run.audit.code_changes += 1;
        }
        Ok(())
    }

    /// Records one managed or elevated operation in the run audit summary.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the session or run is unknown.
    pub fn record_audit_operation(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
        operation: AgentAuditOperation,
    ) -> Result<(), AgentHostError> {
        let session = self.session_mut(session_id)?;
        let run = session.run_mut(run_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            )
        })?;
        match operation {
            AgentAuditOperation::AuthoringOperation => run.audit.authoring_operations += 1,
            AgentAuditOperation::ExternalAcquisition => run.audit.external_acquisitions += 1,
            AgentAuditOperation::RawFilesystemAccess => run.audit.raw_filesystem_accesses += 1,
            AgentAuditOperation::CustomCommand => run.audit.custom_commands += 1,
        }
        Ok(())
    }

    /// Records a deterministic validation result and publishes a structured event.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the session or run is unknown.
    pub fn record_validation(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
        name: impl Into<String>,
        passed: bool,
    ) -> Result<(), AgentHostError> {
        let name = name.into();
        let session = self.session_mut(session_id)?;
        let run = session.run_mut(run_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            )
        })?;
        run.audit.validation_records += 1;
        session.emit(AgentEventKind::Validation { name, passed });
        Ok(())
    }

    /// Records a runtime playtest result and publishes a structured event.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the session or run is unknown.
    pub fn record_playtest(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
        scenario: impl Into<String>,
        passed: bool,
    ) -> Result<(), AgentHostError> {
        let scenario = scenario.into();
        let session = self.session_mut(session_id)?;
        let run = session.run_mut(run_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            )
        })?;
        run.audit.playtest_records += 1;
        session.emit(AgentEventKind::Playtest { scenario, passed });
        Ok(())
    }

    /// Requests an unavailable permission and records it in the semantic audit history.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the session or run is unknown.
    pub fn request_permission(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
        permission: AgentPermission,
        reason: impl Into<String>,
    ) -> Result<Option<PermissionRequestId>, AgentHostError> {
        if self
            .session(session_id)
            .and_then(|session| session.run(run_id))
            .is_none()
        {
            return Err(AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            ));
        }
        let request = self
            .permissions
            .request(run_id.clone(), permission, reason.into());
        if let Some(request_id) = &request {
            let session = self.session_mut(session_id)?;
            if let Some(run) = session.run_mut(run_id) {
                run.audit.permission_escalations += 1;
            }
            session.emit(AgentEventKind::PermissionRequested(request_id.clone()));
        }
        Ok(request)
    }

    /// Applies a human permission decision and records it in the audit timeline.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the request or session is unknown.
    pub fn resolve_permission(
        &mut self,
        session_id: &AgentSessionId,
        request_id: &PermissionRequestId,
        scope: ApprovalScope,
    ) -> Result<(), AgentHostError> {
        self.permissions.resolve(request_id, scope)?;
        self.session_mut(session_id)?
            .emit(AgentEventKind::PermissionResolved {
                request_id: request_id.clone(),
                scope,
            });
        Ok(())
    }

    /// Cancels one non-terminal run without pretending external side effects were rolled back.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the run is unknown or already terminal.
    pub fn cancel_run(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
    ) -> Result<(), AgentHostError> {
        let session = self.session_mut(session_id)?;
        let run = session.run_mut(run_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            )
        })?;
        if run.state.is_terminal() {
            return Err(AgentHostError::new(
                "agent.run_already_terminal",
                format!("agent run `{run_id}` is already terminal"),
            ));
        }
        let from = run.state;
        run.state = AgentRunState::Cancelled;
        session.emit(AgentEventKind::StateChanged {
            run_id: run_id.clone(),
            from,
            to: AgentRunState::Cancelled,
        });
        session.emit(AgentEventKind::Cancelled(run_id.clone()));
        self.finish_writer(session_id, run_id);
        Ok(())
    }

    /// Fails one non-terminal run with a structured semantic event.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the run is unknown or already terminal.
    pub fn fail_run(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
        message: impl Into<String>,
    ) -> Result<(), AgentHostError> {
        let message = message.into();
        let session = self.session_mut(session_id)?;
        let run = session.run_mut(run_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            )
        })?;
        if run.state.is_terminal() {
            return Err(AgentHostError::new(
                "agent.run_already_terminal",
                format!("agent run `{run_id}` is already terminal"),
            ));
        }
        let from = run.state;
        run.state = AgentRunState::Failed;
        session.emit(AgentEventKind::StateChanged {
            run_id: run_id.clone(),
            from,
            to: AgentRunState::Failed,
        });
        session.emit(AgentEventKind::Failed {
            run_id: run_id.clone(),
            message,
        });
        self.finish_writer(session_id, run_id);
        Ok(())
    }

    /// Marks a run complete only when every applicable acceptance, validation,
    /// runtime, frame, visual, and interaction check passes.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when evidence is incomplete or the run is unknown.
    pub fn complete_run(
        &mut self,
        session_id: &AgentSessionId,
        run_id: &AgentRunId,
        evidence: &CompletionReport,
    ) -> Result<(), AgentHostError> {
        if !evidence.is_complete() {
            return Err(AgentHostError::new(
                "agent.completion_incomplete",
                format!(
                    "run `{run_id}` cannot complete; unresolved checks: {}",
                    evidence.unresolved_summary()
                ),
            ));
        }
        let session = self.session_mut(session_id)?;
        let run = session.run_mut(run_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.run_not_found",
                format!("agent run `{run_id}` does not exist"),
            )
        })?;
        if run.state.is_terminal() {
            return Err(AgentHostError::new(
                "agent.run_already_terminal",
                format!("agent run `{run_id}` is already terminal"),
            ));
        }
        let from = run.state;
        run.state = AgentRunState::Completed;
        session.emit(AgentEventKind::StateChanged {
            run_id: run_id.clone(),
            from,
            to: AgentRunState::Completed,
        });
        session.emit(AgentEventKind::Completed(run_id.clone()));
        self.finish_writer(session_id, run_id);
        Ok(())
    }

    fn finish_writer(&mut self, session_id: &AgentSessionId, run_id: &AgentRunId) {
        if self
            .active_writer
            .as_ref()
            .is_some_and(|active| &active.0 == session_id && &active.1 == run_id)
        {
            self.active_writer = None;
        }
        self.permissions.finish_run(run_id);
    }

    /// Returns the currently active AI writer role, if any.
    pub fn active_writer(&self) -> Option<(&AgentSessionId, &AgentRunId)> {
        self.active_writer
            .as_ref()
            .map(|(session, run)| (session, run))
    }

    /// Loads durable project-level capability toggles from user-local application data.
    ///
    /// A missing policy file is treated as the secure default of no project grants.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when an existing policy file is malformed or unsupported.
    pub fn load_project_policy(&mut self) -> Result<(), AgentHostError> {
        let path = self.project_policy_path();
        if !path.exists() {
            return Ok(());
        }
        let bytes = fs::read(&path)
            .map_err(|error| AgentHostError::new("agent.policy_load_failed", error.to_string()))?;
        let policy: PersistedProjectPolicy = serde_json::from_slice(&bytes)
            .map_err(|error| AgentHostError::new("agent.policy_parse_failed", error.to_string()))?;
        if policy.schema_version != AGENT_POLICY_SCHEMA_VERSION {
            return Err(AgentHostError::new(
                "agent.policy_schema_unsupported",
                format!(
                    "agent policy schema {} is unsupported; expected {}",
                    policy.schema_version, AGENT_POLICY_SCHEMA_VERSION
                ),
            ));
        }
        self.permissions = AgentPermissionBroker::with_project_grants(policy.project_grants);
        Ok(())
    }

    /// Persists durable project-level capability toggles without credentials or command strings.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when serialization or atomic persistence fails.
    pub fn save_project_policy(&self) -> Result<PathBuf, AgentHostError> {
        let path = self.project_policy_path();
        let bytes = serde_json::to_vec_pretty(&PersistedProjectPolicy {
            schema_version: AGENT_POLICY_SCHEMA_VERSION,
            project_grants: self.permissions.project_grants().clone(),
        })
        .map_err(|error| AgentHostError::new("agent.policy_serialize_failed", error.to_string()))?;
        atomic_write(&path, &bytes)
            .map_err(|error| AgentHostError::new("agent.policy_save_failed", error.to_string()))?;
        Ok(path)
    }

    fn project_policy_path(&self) -> PathBuf {
        self.local_store_root
            .join(project_storage_key(&self.project_root))
            .join("policy.json")
    }

    /// Changes whether one session is local-private or project-shared.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the session is unknown.
    pub fn set_visibility(
        &mut self,
        session_id: &AgentSessionId,
        visibility: SessionVisibility,
    ) -> Result<(), AgentHostError> {
        self.session_mut(session_id)?.visibility = visibility;
        Ok(())
    }

    /// Persists a loaded session atomically to its configured local/private or
    /// project-shared location.
    ///
    /// Credentials are not represented by any persisted AI session type. The
    /// caller remains responsible for not inserting secret material into free-form
    /// user conversation text before deliberately sharing a session.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] for an unknown session or persistence failure.
    pub fn save_session(&self, session_id: &AgentSessionId) -> Result<PathBuf, AgentHostError> {
        let session = self.session(session_id).ok_or_else(|| {
            AgentHostError::new(
                "agent.session_not_found",
                format!("AI session `{session_id}` is not loaded"),
            )
        })?;
        let path = self.session_path(session);
        let contents = serde_json::to_string_pretty(&PersistedAgentSession {
            schema_version: AI_SESSION_SCHEMA_VERSION,
            session: session.clone(),
        })
        .map_err(|error| AgentHostError::new("agent.session_serialize_failed", error.to_string()))?;
        atomic_write(&path, contents.as_bytes())
            .map_err(|error| AgentHostError::new("agent.session_save_failed", error.to_string()))?;
        Ok(path)
    }

    /// Restores the most recently modified local-private or project-shared session.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when session discovery or loading fails.
    pub fn restore_latest_session(&mut self) -> Result<Option<AgentSessionId>, AgentHostError> {
        let mut candidates = Vec::new();
        let local_sessions = self
            .local_store_root
            .join(project_storage_key(&self.project_root))
            .join("sessions");
        collect_session_candidates(&local_sessions, false, &mut candidates)?;
        let shared_sessions = self
            .project_root
            .join(".gameengine")
            .join("ai")
            .join("sessions");
        collect_session_candidates(&shared_sessions, true, &mut candidates)?;

        let Some((_, path)) = candidates.into_iter().max_by_key(|candidate| candidate.0) else {
            return Ok(None);
        };
        self.load_session(&path).map(Some)
    }

    /// Loads one persisted portable session and inserts it into this project host.
    ///
    /// # Errors
    ///
    /// Returns [`AgentHostError`] when the file cannot be read or uses a different schema.
    pub fn load_session(&mut self, path: &Path) -> Result<AgentSessionId, AgentHostError> {
        let bytes = fs::read(path)
            .map_err(|error| AgentHostError::new("agent.session_load_failed", error.to_string()))?;
        let record: PersistedAgentSession = serde_json::from_slice(&bytes).map_err(|error| {
            AgentHostError::new("agent.session_parse_failed", error.to_string())
        })?;
        if record.schema_version != AI_SESSION_SCHEMA_VERSION {
            return Err(AgentHostError::new(
                "agent.session_schema_unsupported",
                format!(
                    "AI session schema {} is unsupported; expected {}",
                    record.schema_version, AI_SESSION_SCHEMA_VERSION
                ),
            ));
        }
        let id = record.session.id.clone();
        self.sessions.insert(id.clone(), record.session);
        Ok(id)
    }

    fn session_path(&self, session: &AgentSession) -> PathBuf {
        match session.visibility {
            SessionVisibility::LocalPrivate => self
                .local_store_root
                .join(project_storage_key(&self.project_root))
                .join("sessions")
                .join(format!("{}.json", session.id)),
            SessionVisibility::ProjectShared => self
                .project_root
                .join(".gameengine")
                .join("ai")
                .join("sessions")
                .join(session.id.as_str())
                .join("session.json"),
        }
    }
}

fn collect_session_candidates(
    root: &Path,
    nested: bool,
    output: &mut Vec<(std::time::SystemTime, PathBuf)>,
) -> Result<(), AgentHostError> {
    if !root.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(root)
        .map_err(|error| AgentHostError::new("agent.session_discovery_failed", error.to_string()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| AgentHostError::new("agent.session_discovery_failed", error.to_string()))?;
        let path = if nested {
            entry.path().join("session.json")
        } else {
            entry.path()
        };
        if !path.is_file()
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('.'))
            || path.extension().and_then(|extension| extension.to_str()) != Some("json")
        {
            continue;
        }
        let modified = path
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        output.push((modified, path));
    }
    Ok(())
}

fn project_storage_key(path: &Path) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("project-{hash:016x}")
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "target path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("agent-session"),
        generated_id("write")
    ));
    fs::write(&temporary, bytes)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if path.exists() => {
            fs::remove_file(path)?;
            fs::rename(&temporary, path).map_err(|_| error)
        }
        Err(error) => Err(error),
    }
}

/// One managed source change prepared in a session-scoped code workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeChange {
    /// Project-relative source path.
    pub path: PathBuf,
    /// Baseline contents, or `None` for a newly created file.
    pub before: Option<String>,
    /// Workspace contents, or `None` for a deletion.
    pub after: Option<String>,
}

/// Run-level source checkpoint stored independently from Git history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeCheckpoint {
    /// Checkpoint schema version.
    pub schema_version: u32,
    /// Run that owns this logical checkpoint.
    pub run_id: AgentRunId,
    /// Reviewable changes relative to the session workspace baseline.
    pub changes: Vec<CodeChange>,
}

/// Managed, session-scoped isolated source workspace.
///
/// The initial implementation snapshots text source paths explicitly selected by
/// the host. New files may be created inside the workspace, and all live-project
/// application remains an explicit separate operation.
#[derive(Debug)]
pub struct AgentCodeWorkspace {
    project_root: PathBuf,
    workspace_root: PathBuf,
    baseline: BTreeMap<PathBuf, String>,
}

impl AgentCodeWorkspace {
    /// Creates an isolated source workspace from project-relative text paths.
    ///
    /// # Errors
    ///
    /// Returns [`CodeWorkspaceError`] for invalid paths, symlink escapes, I/O,
    /// or non-UTF-8 source files.
    pub fn capture(
        project_root: &Path,
        workspace_root: &Path,
        paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, CodeWorkspaceError> {
        fs::create_dir_all(workspace_root).map_err(CodeWorkspaceError::io)?;
        let project_root = project_root
            .canonicalize()
            .map_err(CodeWorkspaceError::io)?;
        let workspace_root = workspace_root
            .canonicalize()
            .map_err(CodeWorkspaceError::io)?;
        let mut baseline = BTreeMap::new();

        for relative in paths {
            let relative = validated_relative_path(&relative)?;
            let source = project_root.join(&relative);
            let source_canonical = source.canonicalize().map_err(CodeWorkspaceError::io)?;
            if !source_canonical.starts_with(&project_root) {
                return Err(CodeWorkspaceError::new(
                    "agent.code_path_escape",
                    format!("source path `{}` escapes the project root", relative.display()),
                ));
            }
            let contents = fs::read_to_string(&source_canonical).map_err(CodeWorkspaceError::io)?;
            let destination = workspace_root.join(&relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(CodeWorkspaceError::io)?;
            }
            fs::write(&destination, contents.as_bytes()).map_err(CodeWorkspaceError::io)?;
            baseline.insert(relative, contents);
        }

        Ok(Self {
            project_root,
            workspace_root,
            baseline,
        })
    }

    /// Returns the local isolated workspace root, never a canonical project path.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Writes or creates one UTF-8 source file inside the isolated workspace.
    ///
    /// # Errors
    ///
    /// Returns [`CodeWorkspaceError`] if the path escapes the workspace or I/O fails.
    pub fn write_file(
        &self,
        relative: &Path,
        contents: &str,
    ) -> Result<(), CodeWorkspaceError> {
        let relative = validated_relative_path(relative)?;
        let target = self.workspace_root.join(&relative);
        let parent = target.parent().ok_or_else(|| {
            CodeWorkspaceError::new("agent.code_path_invalid", "source path has no parent")
        })?;
        fs::create_dir_all(parent).map_err(CodeWorkspaceError::io)?;
        ensure_under_root(parent, &self.workspace_root)?;
        fs::write(target, contents.as_bytes()).map_err(CodeWorkspaceError::io)
    }

    /// Removes one source file from the isolated workspace without touching the live project.
    ///
    /// # Errors
    ///
    /// Returns [`CodeWorkspaceError`] when the path is invalid or removal fails.
    pub fn remove_file(&self, relative: &Path) -> Result<(), CodeWorkspaceError> {
        let relative = validated_relative_path(relative)?;
        let target = self.workspace_root.join(relative);
        if target.exists() {
            fs::remove_file(target).map_err(CodeWorkspaceError::io)?;
        }
        Ok(())
    }

    /// Returns a reviewable source diff represented as before/after UTF-8 content.
    ///
    /// # Errors
    ///
    /// Returns [`CodeWorkspaceError`] if the isolated workspace cannot be inspected safely.
    pub fn preview_changes(&self) -> Result<Vec<CodeChange>, CodeWorkspaceError> {
        let current = collect_workspace_text(&self.workspace_root)?;
        let mut paths = self
            .baseline
            .keys()
            .chain(current.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut changes = Vec::new();
        for path in std::mem::take(&mut paths) {
            let before = self.baseline.get(&path).cloned();
            let after = current.get(&path).cloned();
            if before != after {
                changes.push(CodeChange { path, before, after });
            }
        }
        Ok(changes)
    }

    /// Writes one run checkpoint beneath the local workspace history directory.
    ///
    /// # Errors
    ///
    /// Returns [`CodeWorkspaceError`] if changes cannot be inspected or persisted.
    pub fn checkpoint(&self, run_id: &AgentRunId) -> Result<PathBuf, CodeWorkspaceError> {
        let checkpoint = CodeCheckpoint {
            schema_version: CODE_CHECKPOINT_SCHEMA_VERSION,
            run_id: run_id.clone(),
            changes: self.preview_changes()?,
        };
        let path = self
            .workspace_root
            .join(".agent-history")
            .join(format!("{}.json", run_id));
        let bytes = serde_json::to_vec_pretty(&checkpoint).map_err(|error| {
            CodeWorkspaceError::new("agent.code_checkpoint_serialize_failed", error.to_string())
        })?;
        atomic_write(&path, &bytes).map_err(CodeWorkspaceError::io)?;
        Ok(path)
    }

    /// Applies the currently previewed source changes to the live project through
    /// the managed code service boundary.
    ///
    /// This operation requires `AuthoringPermission::CodeWrite` but does not grant
    /// raw filesystem or arbitrary shell capabilities.
    ///
    /// # Errors
    ///
    /// Returns [`CodeWorkspaceError`] for missing permission, path escapes, or I/O failures.
    pub fn apply_to_project(
        &self,
        permissions: &AuthoringPermissions,
    ) -> Result<Vec<PathBuf>, CodeWorkspaceError> {
        permissions
            .require(AuthoringPermission::CodeWrite)
            .map_err(|error| {
                CodeWorkspaceError::new("agent.code_write_denied", error.to_string())
            })?;
        let changes = self.preview_changes()?;
        let mut applied = Vec::new();
        for change in changes {
            let relative = validated_relative_path(&change.path)?;
            let destination = self.project_root.join(&relative);
            let parent = destination.parent().ok_or_else(|| {
                CodeWorkspaceError::new("agent.code_path_invalid", "source path has no parent")
            })?;
            fs::create_dir_all(parent).map_err(CodeWorkspaceError::io)?;
            ensure_under_root(parent, &self.project_root)?;
            match change.after {
                Some(contents) => {
                    atomic_write(&destination, contents.as_bytes()).map_err(CodeWorkspaceError::io)?;
                }
                None if destination.exists() => {
                    let canonical = destination.canonicalize().map_err(CodeWorkspaceError::io)?;
                    if !canonical.starts_with(&self.project_root) {
                        return Err(CodeWorkspaceError::new(
                            "agent.code_path_escape",
                            format!("destination `{}` escapes project root", relative.display()),
                        ));
                    }
                    fs::remove_file(canonical).map_err(CodeWorkspaceError::io)?;
                }
                None => {}
            }
            applied.push(relative);
        }
        Ok(applied)
    }
}

fn validated_relative_path(path: &Path) -> Result<PathBuf, CodeWorkspaceError> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(CodeWorkspaceError::new(
            "agent.code_path_invalid",
            format!("source path `{}` must be project-relative", path.display()),
        ));
    }
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => clean.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CodeWorkspaceError::new(
                    "agent.code_path_escape",
                    format!("source path `{}` escapes its managed root", path.display()),
                ));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(CodeWorkspaceError::new(
            "agent.code_path_invalid",
            "source path must contain a file name",
        ));
    }
    Ok(clean)
}

fn ensure_under_root(path: &Path, root: &Path) -> Result<(), CodeWorkspaceError> {
    let canonical = path.canonicalize().map_err(CodeWorkspaceError::io)?;
    if canonical.starts_with(root) {
        Ok(())
    } else {
        Err(CodeWorkspaceError::new(
            "agent.code_path_escape",
            format!("path `{}` escapes managed root `{}`", path.display(), root.display()),
        ))
    }
}

fn collect_workspace_text(root: &Path) -> Result<BTreeMap<PathBuf, String>, CodeWorkspaceError> {
    fn visit(
        root: &Path,
        directory: &Path,
        output: &mut BTreeMap<PathBuf, String>,
    ) -> Result<(), CodeWorkspaceError> {
        for entry in fs::read_dir(directory).map_err(CodeWorkspaceError::io)? {
            let entry = entry.map_err(CodeWorkspaceError::io)?;
            let file_type = entry.file_type().map_err(CodeWorkspaceError::io)?;
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(|error| {
                CodeWorkspaceError::new("agent.code_path_escape", error.to_string())
            })?;
            if relative
                .components()
                .next()
                .is_some_and(|component| component.as_os_str() == ".agent-history")
            {
                continue;
            }
            if file_type.is_symlink() {
                return Err(CodeWorkspaceError::new(
                    "agent.code_symlink_rejected",
                    format!("workspace symlink `{}` is not managed", relative.display()),
                ));
            }
            if file_type.is_dir() {
                visit(root, &path, output)?;
            } else if file_type.is_file() {
                output.insert(
                    relative.to_path_buf(),
                    fs::read_to_string(&path).map_err(CodeWorkspaceError::io)?,
                );
            }
        }
        Ok(())
    }

    let mut output = BTreeMap::new();
    visit(root, root, &mut output)?;
    Ok(output)
}

/// Managed source-workspace failure with a stable diagnostic-style code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeWorkspaceError {
    code: String,
    message: String,
}

impl CodeWorkspaceError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn io(error: io::Error) -> Self {
        Self::new("agent.code_workspace_io", error.to_string())
    }

    /// Returns the stable diagnostic-style code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the human-readable explanation.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for CodeWorkspaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CodeWorkspaceError {}

/// Engine-managed validation operation that does not imply arbitrary shell access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedValidationCommand {
    /// `cargo fmt --all --check`.
    Format,
    /// `cargo metadata --format-version 1 --locked`.
    Metadata,
    /// `cargo check --workspace`.
    Check,
    /// `cargo clippy --workspace --all-targets -- -D warnings`.
    Clippy,
    /// `cargo test --workspace`.
    Tests,
    /// `cargo doc --workspace --no-deps`.
    Documentation,
}

/// Program and argument vector for one host-owned managed validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedCommandSpec {
    /// Program executable selected by the engine allow-list.
    pub program: &'static str,
    /// Arguments passed directly without shell parsing.
    pub arguments: Vec<&'static str>,
}

impl ManagedValidationCommand {
    /// Returns the exact allow-listed process specification for this validator.
    pub fn command_spec(self) -> ManagedCommandSpec {
        match self {
            Self::Format => ManagedCommandSpec {
                program: "cargo",
                arguments: vec!["fmt", "--all", "--check"],
            },
            Self::Metadata => ManagedCommandSpec {
                program: "cargo",
                arguments: vec!["metadata", "--format-version", "1", "--locked"],
            },
            Self::Check => ManagedCommandSpec {
                program: "cargo",
                arguments: vec!["check", "--workspace"],
            },
            Self::Clippy => ManagedCommandSpec {
                program: "cargo",
                arguments: vec![
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            },
            Self::Tests => ManagedCommandSpec {
                program: "cargo",
                arguments: vec!["test", "--workspace"],
            },
            Self::Documentation => ManagedCommandSpec {
                program: "cargo",
                arguments: vec!["doc", "--workspace", "--no-deps"],
            },
        }
    }
}

/// Search request understood by a typed external asset provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetSearchQuery {
    /// Human-readable search text.
    pub query: String,
}

/// Reviewable search result before any external content is acquired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetCandidate {
    /// Provider-stable candidate identifier.
    pub provider_asset_id: String,
    /// Human-readable candidate name.
    pub display_name: String,
    /// Optional source/license explanation supplied by the provider.
    pub provenance: Option<String>,
}

/// Acquired external bytes and provenance before normal GameEngine import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquiredAsset {
    /// Suggested project-relative file name.
    pub suggested_file_name: PathBuf,
    /// Downloaded or generated bytes passed to the existing import pipeline.
    pub bytes: Vec<u8>,
    /// Reviewable source/license information when available.
    pub provenance: Option<String>,
}

/// GUI-free external asset acquisition provider.
pub trait AssetAcquisitionProvider {
    /// Stable provider identifier.
    fn provider_id(&self) -> &str;

    /// Searches provider metadata without mutating project assets.
    ///
    /// # Errors
    ///
    /// Returns [`AgentRuntimeError`] for provider connection or query failures.
    fn search(&mut self, query: &AssetSearchQuery) -> Result<Vec<AssetCandidate>, AgentRuntimeError>;

    /// Acquires one chosen candidate for the normal import/manifest pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`AgentRuntimeError`] when external acquisition fails.
    fn acquire(&mut self, candidate: &AssetCandidate) -> Result<AcquiredAsset, AgentRuntimeError>;
}

/// One required completion evidence category from ADR 0131.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionCheckKind {
    /// Proposal acceptance criteria.
    AcceptanceCriteria,
    /// Blocking authoring validation.
    AuthoringValidation,
    /// Required source-code validation.
    SourceValidation,
    /// Successful runtime launch.
    PlayLaunch,
    /// At least one relevant captured frame.
    FrameCapture,
    /// Agent inspection of the captured visual result.
    VisualEvaluation,
    /// Required interactive scenarios.
    InteractionScenarios,
}

/// Pass/fail/not-applicable evidence for one completion category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionCheck {
    /// Evidence category.
    pub kind: CompletionCheckKind,
    /// Whether the proposal requires this check.
    pub applicable: bool,
    /// Whether the applicable check passed.
    pub passed: bool,
    /// Human-readable evidence or reason it is not applicable.
    pub detail: String,
}

/// Completion evidence that prevents compile-only success claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionReport {
    checks: BTreeMap<CompletionCheckKind, CompletionCheck>,
}

impl CompletionReport {
    /// Creates a report from explicit evidence records.
    pub fn new(checks: impl IntoIterator<Item = CompletionCheck>) -> Self {
        Self {
            checks: checks
                .into_iter()
                .map(|check| (check.kind, check))
                .collect(),
        }
    }

    /// Returns one evidence category when it was reported.
    pub fn check(&self, kind: CompletionCheckKind) -> Option<&CompletionCheck> {
        self.checks.get(&kind)
    }

    /// Returns whether all seven categories were explicitly reported and every
    /// applicable category passed.
    pub fn is_complete(&self) -> bool {
        let required = [
            CompletionCheckKind::AcceptanceCriteria,
            CompletionCheckKind::AuthoringValidation,
            CompletionCheckKind::SourceValidation,
            CompletionCheckKind::PlayLaunch,
            CompletionCheckKind::FrameCapture,
            CompletionCheckKind::VisualEvaluation,
            CompletionCheckKind::InteractionScenarios,
        ];
        required.into_iter().all(|kind| {
            self.check(kind)
                .is_some_and(|check| !check.applicable || check.passed)
        })
    }

    /// Returns a compact explanation of missing or failing completion evidence.
    pub fn unresolved_summary(&self) -> String {
        let required = [
            CompletionCheckKind::AcceptanceCriteria,
            CompletionCheckKind::AuthoringValidation,
            CompletionCheckKind::SourceValidation,
            CompletionCheckKind::PlayLaunch,
            CompletionCheckKind::FrameCapture,
            CompletionCheckKind::VisualEvaluation,
            CompletionCheckKind::InteractionScenarios,
        ];
        let unresolved = required
            .into_iter()
            .filter_map(|kind| match self.check(kind) {
                None => Some(format!("{kind:?}:missing")),
                Some(check) if check.applicable && !check.passed => Some(format!("{kind:?}:failed")),
                Some(_) => None,
            })
            .collect::<Vec<_>>();
        if unresolved.is_empty() {
            "none".to_owned()
        } else {
            unresolved.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "gameengine-agent-test-{name}-{}",
            generated_id("tmp")
        ));
        fs::create_dir_all(&root).expect("test root must be creatable");
        root
    }

    fn draft(goal: &str) -> AgentProposalDraft {
        AgentProposalDraft {
            goal: goal.to_owned(),
            acceptance_criteria: vec!["it works".to_owned()],
            ..AgentProposalDraft::default()
        }
    }

    fn register_ready_provider(host: &mut AgentHost) {
        host.register_provider(ProviderDescriptor {
            id: "test-provider".to_owned(),
            display_name: "Test Provider".to_owned(),
            runtime_kind: ProviderRuntimeKind::ExternalAgentRuntime,
            authentication: AuthenticationClass::LocalNoAuth,
            connection_state: ProviderConnectionState::Ready,
        });
    }

    #[test]
    fn go_snapshots_exact_proposal_version() {
        let project = temp_root("proposal-project");
        let local = temp_root("proposal-local");
        let mut host = AgentHost::new(project, local);
        register_ready_provider(&mut host);
        let session = host.create_session("test");
        host.revise_proposal(&session, draft("first"))
            .expect("proposal must revise");
        let run = host
            .start_run(&session, "test-provider")
            .expect("Go must start");
        host.revise_proposal(&session, draft("second"))
            .expect("later conversation may revise proposal");

        let session = host.session(&session).expect("session must remain loaded");
        assert_eq!(session.current_proposal().unwrap().version().get(), 2);
        assert_eq!(session.run(&run).unwrap().input().provider_id(), "test-provider");
        assert_eq!(
            session.run(&run).unwrap().input().proposal().version().get(),
            1
        );
        assert_eq!(
            session.run(&run).unwrap().input().proposal().draft().goal,
            "first"
        );
    }

    #[test]
    fn only_one_ai_writer_run_is_active() {
        let project = temp_root("writer-project");
        let local = temp_root("writer-local");
        let mut host = AgentHost::new(project, local);
        register_ready_provider(&mut host);
        let first = host.create_session("first");
        let second = host.create_session("second");
        host.revise_proposal(&first, draft("one")).unwrap();
        host.revise_proposal(&second, draft("two")).unwrap();
        let run = host.start_run(&first, "test-provider").unwrap();

        let error = host
            .start_run(&second, "test-provider")
            .expect_err("second writer must wait");
        assert_eq!(error.code(), "agent.writer_busy");
        host.cancel_run(&first, &run).unwrap();
        assert!(host.start_run(&second, "test-provider").is_ok());
    }

    #[test]
    fn repair_and_awaiting_user_transitions_are_resumable() {
        let project = temp_root("state-project");
        let local = temp_root("state-local");
        let mut host = AgentHost::new(project, local);
        register_ready_provider(&mut host);
        let session = host.create_session("state");
        host.revise_proposal(&session, draft("state")).unwrap();
        let run = host.start_run(&session, "test-provider").unwrap();

        host.transition_run(&session, &run, AgentRunState::Planning)
            .unwrap();
        host.transition_run(&session, &run, AgentRunState::Executing)
            .unwrap();
        host.transition_run(&session, &run, AgentRunState::Repairing)
            .unwrap();
        host.transition_run(&session, &run, AgentRunState::AwaitingUser)
            .unwrap();
        host.transition_run(&session, &run, AgentRunState::Executing)
            .unwrap();
    }

    #[test]
    fn permission_scopes_are_distinct() {
        let run = AgentRunId::generate();
        let permission = AgentPermission::Agent(AgentCapability::NetworkAccess);
        let mut broker = AgentPermissionBroker::default();
        let request = broker
            .request(run.clone(), permission, "search provider")
            .unwrap();
        broker.resolve(&request, ApprovalScope::AllowOnce).unwrap();
        assert!(broker.authorize(&run, permission));
        assert!(!broker.authorize(&run, permission));

        let request = broker
            .request(run.clone(), permission, "search again")
            .unwrap();
        broker
            .resolve(&request, ApprovalScope::AllowForRun)
            .unwrap();
        assert!(broker.authorize(&run, permission));
        broker.finish_run(&run);
        assert!(!broker.authorize(&run, permission));
    }

    #[test]
    fn shared_session_is_written_under_reserved_project_metadata() {
        let project = temp_root("shared-project");
        let local = temp_root("shared-local");
        let mut host = AgentHost::new(project.clone(), local);
        let session = host.create_session("shared");
        host.add_message(&session, ConversationRole::User, "build a game")
            .unwrap();
        host.set_visibility(&session, SessionVisibility::ProjectShared)
            .unwrap();
        let path = host.save_session(&session).unwrap();

        assert!(path.starts_with(project.join(".gameengine/ai/sessions")));
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("build a game"));
        assert!(!text.contains("authorization_token"));
    }

    #[test]
    fn local_session_path_does_not_enter_project_tree() {
        let project = temp_root("private-project");
        let local = temp_root("private-local");
        let mut host = AgentHost::new(project.clone(), local.clone());
        let session = host.create_session("private");
        let path = host.save_session(&session).unwrap();

        assert!(path.starts_with(local));
        assert!(!path.starts_with(project));
    }

    #[test]
    fn latest_local_session_restores_after_restart() {
        let project = temp_root("restore-project");
        let local = temp_root("restore-local");
        let session = {
            let mut host = AgentHost::new(project.clone(), local.clone());
            let session = host.create_session("restored");
            host.add_message(&session, ConversationRole::User, "continue this")
                .unwrap();
            host.save_session(&session).unwrap();
            session
        };

        let mut restarted = AgentHost::new(project, local);
        let restored = restarted
            .restore_latest_session()
            .unwrap()
            .expect("saved session must be discovered");
        assert_eq!(restored, session);
        assert_eq!(
            restarted.session(&restored).unwrap().conversation()[0].text(),
            "continue this"
        );
    }

    #[test]
    fn run_audit_distinguishes_managed_and_escape_hatch_work() {
        let project = temp_root("audit-project");
        let local = temp_root("audit-local");
        let mut host = AgentHost::new(project, local);
        register_ready_provider(&mut host);
        let session = host.create_session("audit");
        host.revise_proposal(&session, draft("audit")).unwrap();
        let run = host.start_run(&session, "test-provider").unwrap();
        host.record_audit_operation(&session, &run, AgentAuditOperation::AuthoringOperation)
            .unwrap();
        host.record_audit_operation(&session, &run, AgentAuditOperation::CustomCommand)
            .unwrap();
        host.record_changed_source_file(&session, &run, PathBuf::from("src/lib.rs"))
            .unwrap();
        host.record_validation(&session, &run, "clippy", true)
            .unwrap();
        host.record_playtest(&session, &run, "launch", true)
            .unwrap();

        let audit = host.session(&session).unwrap().run(&run).unwrap().audit();
        assert_eq!(audit.authoring_operations, 1);
        assert_eq!(audit.code_changes, 1);
        assert_eq!(audit.custom_commands, 1);
        assert_eq!(audit.validation_records, 1);
        assert_eq!(audit.playtest_records, 1);
    }

    #[test]
    fn code_workspace_isolated_changes_require_explicit_apply() {
        let project = temp_root("code-project");
        let workspace = temp_root("code-workspace");
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src/lib.rs"), "pub fn answer() -> u32 { 1 }\n").unwrap();
        let code = AgentCodeWorkspace::capture(
            &project,
            &workspace,
            [PathBuf::from("src/lib.rs")],
        )
        .unwrap();
        code.write_file("src/lib.rs".as_ref(), "pub fn answer() -> u32 { 2 }\n")
            .unwrap();

        assert_eq!(
            fs::read_to_string(project.join("src/lib.rs")).unwrap(),
            "pub fn answer() -> u32 { 1 }\n"
        );
        assert_eq!(code.preview_changes().unwrap().len(), 1);
        let denied = code
            .apply_to_project(&AuthoringPermissions::read_only())
            .expect_err("CodeWrite is mandatory");
        assert_eq!(denied.code(), "agent.code_write_denied");

        let permissions = AuthoringPermissions::read_only().with(AuthoringPermission::CodeWrite);
        code.apply_to_project(&permissions).unwrap();
        assert_eq!(
            fs::read_to_string(project.join("src/lib.rs")).unwrap(),
            "pub fn answer() -> u32 { 2 }\n"
        );
    }

    #[test]
    fn code_workspace_rejects_parent_traversal() {
        let project = temp_root("escape-project");
        let workspace = temp_root("escape-workspace");
        fs::write(project.join("safe.rs"), "fn safe() {}\n").unwrap();
        let code = AgentCodeWorkspace::capture(&project, &workspace, [PathBuf::from("safe.rs")])
            .unwrap();
        let error = code
            .write_file("../escape.rs".as_ref(), "bad")
            .expect_err("parent traversal must be rejected");
        assert_eq!(error.code(), "agent.code_path_escape");
    }

    #[test]
    fn completion_requires_explicit_visual_and_runtime_evidence() {
        let complete = CompletionReport::new([
            CompletionCheck {
                kind: CompletionCheckKind::AcceptanceCriteria,
                applicable: true,
                passed: true,
                detail: "accepted".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::AuthoringValidation,
                applicable: true,
                passed: true,
                detail: "validated".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::SourceValidation,
                applicable: true,
                passed: true,
                detail: "tests passed".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::PlayLaunch,
                applicable: true,
                passed: true,
                detail: "launched".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::FrameCapture,
                applicable: true,
                passed: true,
                detail: "frame.png".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::VisualEvaluation,
                applicable: true,
                passed: true,
                detail: "image inspected".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::InteractionScenarios,
                applicable: false,
                passed: false,
                detail: "not applicable".into(),
            },
        ]);
        assert!(complete.is_complete());

        let incomplete = CompletionReport::new([
            CompletionCheck {
                kind: CompletionCheckKind::AcceptanceCriteria,
                applicable: true,
                passed: true,
                detail: String::new(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::AuthoringValidation,
                applicable: false,
                passed: false,
                detail: "not applicable".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::SourceValidation,
                applicable: false,
                passed: false,
                detail: "not applicable".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::PlayLaunch,
                applicable: true,
                passed: true,
                detail: String::new(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::FrameCapture,
                applicable: true,
                passed: false,
                detail: "not captured".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::VisualEvaluation,
                applicable: true,
                passed: false,
                detail: "not inspected".into(),
            },
            CompletionCheck {
                kind: CompletionCheckKind::InteractionScenarios,
                applicable: false,
                passed: false,
                detail: "not applicable".into(),
            },
        ]);
        assert!(!incomplete.is_complete());
        assert!(incomplete.unresolved_summary().contains("FrameCapture"));
    }

    #[test]
    fn managed_validators_never_require_shell_parsing() {
        let clippy = ManagedValidationCommand::Clippy.command_spec();
        assert_eq!(clippy.program, "cargo");
        assert_eq!(
            clippy.arguments,
            vec!["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]
        );
    }

    #[test]
    fn generated_ids_change_even_within_same_clock_tick() {
        let first = AgentSessionId::generate();
        let second = AgentSessionId::generate();
        assert_ne!(first, second);
        std::thread::sleep(Duration::from_millis(1));
        assert_ne!(second, AgentSessionId::generate());
    }
}
