//! Transcript projection over Agent Host state (ADR 0158).
//!
//! The studio's primary surface is one conversation transcript. This module
//! derives that transcript from `AgentSession` messages and `AgentRun` event
//! logs so every presentation — embedded, detached, and the remote companion —
//! renders the same entries. Drawing code decides how an entry looks; it does
//! not decide what an entry is.
//!
//! The projection holds no authoritative state and never reorders or summarizes
//! host events in a way that changes their meaning.

use crate::agent_host::{
    AgentEvent, AgentEventEvidence, AgentEventKind, AgentRunState, AgentSession, CompletionStatus,
    ConversationRole,
};

/// Longest detail text an entry shows before it is collapsed by default.
const COLLAPSE_DETAIL_CHARS: usize = 220;

/// What one transcript entry represents.
///
/// Every `AgentEventKind` maps to exactly one entry kind. A kind with no
/// mapping is a defect rather than a reason to drop the event, so unmapped
/// kinds render as [`TranscriptEntryKind::Note`] carrying their message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptEntryKind {
    /// A message the user sent.
    UserMessage,
    /// A message the agent produced.
    AgentMessage,
    /// A system note in the conversation.
    SystemMessage,
    /// A run opened by an explicit Go.
    RunOpened,
    /// The run's state machine advanced.
    RunState,
    /// A proposal snapshot or revision.
    Proposal,
    /// A permission request or its resolution.
    Permission,
    /// Semantic progress reported by the runtime.
    Progress,
    /// A tool action and its outcome.
    ToolAction,
    /// Work claim coordination between runs.
    WorkCoordination,
    /// Source workspace preparation, detection, or application.
    CodeChange,
    /// A validation result.
    Validation,
    /// A playtest result.
    Playtest,
    /// A captured frame.
    CapturedFrame,
    /// A recorded model exchange (ADR 0159).
    ModelExchange,
    /// A resource or confinement policy decision.
    ResourcePolicy,
    /// Editing interruption or resumption.
    EditingState,
    /// A cancellation.
    Cancellation,
    /// A failure.
    Failure,
    /// A completion report.
    Completion,
    /// An event this build has no specific presentation for.
    Note,
}

impl TranscriptEntryKind {
    /// Short label a presentation may show as the entry's heading.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::UserMessage => "You",
            Self::AgentMessage => "Agent",
            Self::SystemMessage => "System",
            Self::RunOpened => "Run started",
            Self::RunState => "State",
            Self::Proposal => "Proposal",
            Self::Permission => "Permission",
            Self::Progress => "Progress",
            Self::ToolAction => "Tool",
            Self::WorkCoordination => "Work ownership",
            Self::CodeChange => "Code",
            Self::Validation => "Validation",
            Self::Playtest => "Playtest",
            Self::CapturedFrame => "Frame",
            Self::ModelExchange => "Model turn",
            Self::ResourcePolicy => "Policy",
            Self::EditingState => "Editing",
            Self::Cancellation => "Cancelled",
            Self::Failure => "Failure",
            Self::Completion => "Completion",
            Self::Note => "Note",
        }
    }
}

/// An Editor context one entry can navigate to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TranscriptNavigation {
    /// Open the captured frame belonging to this run.
    CapturedFrame {
        /// Stable artifact identity inside the run.
        artifact_id: String,
    },
    /// Reveal the managed code workspace for this run.
    CodeWorkspace,
}

/// One ordered transcript entry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TranscriptEntry {
    /// What this entry represents.
    pub(crate) kind: TranscriptEntryKind,
    /// Host timestamp used for ordering and display.
    pub(crate) created_unix_ms: u64,
    /// Run this entry belongs to, when it came from a run event log.
    pub(crate) run_id: Option<String>,
    /// Host event sequence inside its run, used only for tie-breaking.
    pub(crate) sequence: u64,
    /// One-line summary that is always visible.
    pub(crate) summary: String,
    /// Longer text shown when the entry is expanded.
    pub(crate) detail: String,
    /// Whether a presentation may collapse the detail by default.
    ///
    /// A permission escalation, an escape hatch, a failed gate, and an
    /// unperformed completion criterion are never collapsible.
    pub(crate) collapsible: bool,
    /// Outcome that must stay visible without expanding, when the entry has one.
    pub(crate) outcome: Option<TranscriptOutcome>,
    /// Editor context this entry can navigate to.
    pub(crate) navigation: Option<TranscriptNavigation>,
}

/// Outcome shown beside an entry summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptOutcome {
    /// The step succeeded.
    Succeeded,
    /// The step failed.
    Failed,
    /// The step did not apply.
    NotApplicable,
    /// The step is still in flight or its outcome was not reported.
    Pending,
}

impl TranscriptOutcome {
    /// Short label a presentation shows beside the summary.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "ok",
            Self::Failed => "failed",
            Self::NotApplicable => "n/a",
            Self::Pending => "pending",
        }
    }
}

/// One run span in the transcript.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TranscriptRunSpan {
    /// Run identity.
    pub(crate) run_id: String,
    /// Immutable proposal snapshot the run was started from.
    pub(crate) proposal_summary: String,
    /// Current run state.
    pub(crate) state: AgentRunState,
    /// When the run opened.
    pub(crate) started_unix_ms: u64,
    /// When the run closed, when it has.
    pub(crate) finished_unix_ms: Option<u64>,
}

/// The full projected transcript of one session.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct SessionTranscript {
    /// Ordered entries.
    pub(crate) entries: Vec<TranscriptEntry>,
    /// Run spans in run start order.
    pub(crate) runs: Vec<TranscriptRunSpan>,
}

/// Projects one session into its transcript.
///
/// Ordering is deterministic: entries sort by host timestamp, then by run start
/// order, then by event sequence, so the same session renders identically in
/// every presentation and on every reopen.
pub(crate) fn project_session(session: &AgentSession) -> SessionTranscript {
    let mut ordered: Vec<(u64, usize, u64, TranscriptEntry)> = Vec::new();
    for message in &session.messages {
        let kind = match message.role {
            ConversationRole::User => TranscriptEntryKind::UserMessage,
            ConversationRole::Assistant => TranscriptEntryKind::AgentMessage,
            ConversationRole::System => TranscriptEntryKind::SystemMessage,
        };
        ordered.push((
            message.created_unix_ms,
            0,
            0,
            TranscriptEntry {
                kind,
                created_unix_ms: message.created_unix_ms,
                run_id: None,
                sequence: 0,
                summary: first_line(&message.text),
                detail: message.text.clone(),
                collapsible: message.text.chars().count() > COLLAPSE_DETAIL_CHARS,
                outcome: None,
                navigation: None,
            },
        ));
    }

    let mut runs = Vec::new();
    for (run_order, run) in session.runs.iter().enumerate() {
        runs.push(TranscriptRunSpan {
            run_id: run.id.clone(),
            proposal_summary: proposal_summary(&run.proposal_snapshot),
            state: run.state,
            started_unix_ms: run.started_unix_ms,
            finished_unix_ms: run.finished_unix_ms,
        });
        for event in &run.events {
            let entry = project_event(&run.id, event);
            ordered.push((event.created_unix_ms, run_order + 1, event.sequence, entry));
        }
    }

    ordered.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
    });
    SessionTranscript {
        entries: ordered.into_iter().map(|(_, _, _, entry)| entry).collect(),
        runs,
    }
}

fn project_event(run_id: &str, event: &AgentEvent) -> TranscriptEntry {
    let kind = entry_kind(event.kind);
    let mut outcome = None;
    let mut navigation = None;
    let mut detail = event.message.clone();
    let mut collapsible = event.message.chars().count() > COLLAPSE_DETAIL_CHARS;

    match event.evidence.as_ref() {
        Some(AgentEventEvidence::Progress { step, detail: text }) => {
            detail = format!("{step}: {text}");
        }
        Some(AgentEventEvidence::ToolAction {
            tool,
            action,
            success,
        }) => {
            detail = format!("{tool} · {action}");
            outcome = Some(match success {
                Some(true) => TranscriptOutcome::Succeeded,
                Some(false) => TranscriptOutcome::Failed,
                None => TranscriptOutcome::Pending,
            });
        }
        Some(AgentEventEvidence::Playtest {
            launched,
            interactions_passed,
        }) => {
            detail = format!(
                "launched={launched} interactions_passed={}",
                match interactions_passed {
                    Some(value) => value.to_string(),
                    None => "unreported".to_owned(),
                }
            );
            outcome = Some(match (launched, interactions_passed) {
                (true, Some(true)) => TranscriptOutcome::Succeeded,
                (_, Some(false)) => TranscriptOutcome::Failed,
                _ => TranscriptOutcome::Pending,
            });
        }
        Some(AgentEventEvidence::CapturedFrame {
            artifact_id,
            width,
            height,
        }) => {
            detail = format!("{artifact_id} ({width}x{height})");
            navigation = Some(TranscriptNavigation::CapturedFrame {
                artifact_id: artifact_id.clone(),
            });
        }
        Some(AgentEventEvidence::CompletionGate { gate, status }) => {
            detail = format!("{gate}: {status:?}");
            outcome = Some(match status {
                CompletionStatus::Passed => TranscriptOutcome::Succeeded,
                CompletionStatus::Failed => TranscriptOutcome::Failed,
                CompletionStatus::NotApplicable => TranscriptOutcome::NotApplicable,
                CompletionStatus::Pending => TranscriptOutcome::Pending,
            });
        }
        Some(AgentEventEvidence::ModelExchange {
            turn,
            prompt_tokens,
            response_tokens,
            finish_reason,
            response_excerpt,
            ..
        }) => {
            detail = format!(
                "turn {turn} · finish {finish_reason} · prompt {} · response {}\n{response_excerpt}",
                token_label(*prompt_tokens),
                token_label(*response_tokens),
            );
            collapsible = true;
        }
        None => {}
    }

    if let Some(validation) = event.validation.as_ref() {
        detail = format!("{detail}\n{}", validation_detail(validation));
    }

    if matches!(
        kind,
        TranscriptEntryKind::Permission
            | TranscriptEntryKind::Failure
            | TranscriptEntryKind::Cancellation
    ) {
        collapsible = false;
    }
    if outcome == Some(TranscriptOutcome::Failed) {
        collapsible = false;
    }
    if kind == TranscriptEntryKind::CodeChange {
        navigation = Some(TranscriptNavigation::CodeWorkspace);
    }

    TranscriptEntry {
        kind,
        created_unix_ms: event.created_unix_ms,
        run_id: Some(run_id.to_owned()),
        sequence: event.sequence,
        summary: first_line(&event.message),
        detail,
        collapsible,
        outcome,
        navigation,
    }
}

fn entry_kind(kind: AgentEventKind) -> TranscriptEntryKind {
    match kind {
        AgentEventKind::RunStarted => TranscriptEntryKind::RunOpened,
        AgentEventKind::StateChanged => TranscriptEntryKind::RunState,
        AgentEventKind::UserMessage => TranscriptEntryKind::UserMessage,
        AgentEventKind::AssistantMessage => TranscriptEntryKind::AgentMessage,
        AgentEventKind::Proposal => TranscriptEntryKind::Proposal,
        AgentEventKind::WorkClaimAcquired
        | AgentEventKind::WorkClaimReleased
        | AgentEventKind::WorkConflict
        | AgentEventKind::WorkWait
        | AgentEventKind::CrossRunDependency
        | AgentEventKind::Reconciliation => TranscriptEntryKind::WorkCoordination,
        AgentEventKind::SemanticProgress => TranscriptEntryKind::Progress,
        AgentEventKind::ToolAction => TranscriptEntryKind::ToolAction,
        AgentEventKind::PermissionRequested | AgentEventKind::PermissionResolved => {
            TranscriptEntryKind::Permission
        }
        AgentEventKind::ProviderOutput => TranscriptEntryKind::ModelExchange,
        AgentEventKind::CodeWorkspacePrepared
        | AgentEventKind::CodeChangesDetected
        | AgentEventKind::CodeChangesApplied => TranscriptEntryKind::CodeChange,
        AgentEventKind::Validation => TranscriptEntryKind::Validation,
        AgentEventKind::Playtest => TranscriptEntryKind::Playtest,
        AgentEventKind::CapturedFrame => TranscriptEntryKind::CapturedFrame,
        AgentEventKind::EditingInterrupted | AgentEventKind::EditingResumed => {
            TranscriptEntryKind::EditingState
        }
        AgentEventKind::ResourcePolicy => TranscriptEntryKind::ResourcePolicy,
        AgentEventKind::Cancellation => TranscriptEntryKind::Cancellation,
        AgentEventKind::Failure => TranscriptEntryKind::Failure,
        AgentEventKind::Completion => TranscriptEntryKind::Completion,
        AgentEventKind::Unknown => TranscriptEntryKind::Note,
    }
}

fn token_label(tokens: Option<u64>) -> String {
    tokens.map_or_else(|| "unreported".to_owned(), |value| value.to_string())
}

fn validation_detail(validation: &crate::agent_host::ManagedValidationEvent) -> String {
    serde_json::to_string(validation).unwrap_or_else(|_| "validation detail unavailable".to_owned())
}

fn proposal_summary(proposal: &crate::agent_host::AgentProposal) -> String {
    let goal = first_line(&proposal.goal);
    if goal.trim().is_empty() {
        format!("proposal v{}", proposal.version)
    } else {
        format!("v{} · {goal}", proposal.version)
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= COLLAPSE_DETAIL_CHARS {
        return line.to_owned();
    }
    format!(
        "{}…",
        line.chars().take(COLLAPSE_DETAIL_CHARS).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_host::{AgentHost, ModelExchangeRecord};
    use std::fs;
    use std::path::PathBuf;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gameengine-transcript-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_nanos())
                .unwrap_or_default()
        ))
    }

    fn host(label: &str) -> (AgentHost, PathBuf, PathBuf) {
        let project = temp_path(&format!("{label}-project"));
        let storage = temp_path(&format!("{label}-storage"));
        fs::create_dir_all(&project).expect("test project directory");
        let host = AgentHost::open(project.clone(), storage.clone()).expect("host");
        (host, project, storage)
    }

    #[test]
    fn every_event_kind_maps_to_exactly_one_entry_kind() {
        // A kind with no mapping is a defect; this is the guard that says so
        // before an event can disappear from the transcript.
        for kind in [
            AgentEventKind::RunStarted,
            AgentEventKind::StateChanged,
            AgentEventKind::UserMessage,
            AgentEventKind::AssistantMessage,
            AgentEventKind::Proposal,
            AgentEventKind::WorkClaimAcquired,
            AgentEventKind::WorkClaimReleased,
            AgentEventKind::WorkConflict,
            AgentEventKind::WorkWait,
            AgentEventKind::CrossRunDependency,
            AgentEventKind::Reconciliation,
            AgentEventKind::SemanticProgress,
            AgentEventKind::ToolAction,
            AgentEventKind::PermissionRequested,
            AgentEventKind::PermissionResolved,
            AgentEventKind::ProviderOutput,
            AgentEventKind::CodeWorkspacePrepared,
            AgentEventKind::CodeChangesDetected,
            AgentEventKind::CodeChangesApplied,
            AgentEventKind::Validation,
            AgentEventKind::Playtest,
            AgentEventKind::CapturedFrame,
            AgentEventKind::EditingInterrupted,
            AgentEventKind::EditingResumed,
            AgentEventKind::ResourcePolicy,
            AgentEventKind::Cancellation,
            AgentEventKind::Failure,
            AgentEventKind::Completion,
            AgentEventKind::Unknown,
        ] {
            let _ = entry_kind(kind);
        }
    }

    #[test]
    fn an_event_kind_from_a_newer_build_keeps_its_message_in_the_transcript() {
        let event: AgentEvent = serde_json::from_value(serde_json::json!({
            "sequence": 3,
            "created_unix_ms": 7,
            "kind": "some_future_kind",
            "message": "written by a newer build",
        }))
        .expect("event written by a newer build stays readable");
        let entry = project_event("run", &event);
        assert_eq!(entry.kind, TranscriptEntryKind::Note);
        assert_eq!(entry.summary, "written by a newer build");
    }

    #[test]
    fn conversation_and_run_events_interleave_in_host_order() {
        let (mut host, project, storage) = host("ordering");
        let session = host.create_session("Ordering").expect("session");
        host.append_message(&session, ConversationRole::User, "build a thing")
            .expect("message");
        let version = host.session(&session).expect("session").proposal.version;
        let run = host
            .start_run_authorized(&session, version, "test")
            .expect("run");
        host.record_semantic_progress(&run, "step", "did a thing")
            .expect("progress");
        host.append_message(&session, ConversationRole::Assistant, "done")
            .expect("message");

        let transcript = project_session(host.session(&session).expect("session"));
        let kinds = transcript
            .entries
            .iter()
            .map(|entry| entry.kind)
            .collect::<Vec<_>>();
        assert_eq!(kinds.first(), Some(&TranscriptEntryKind::UserMessage));
        assert!(kinds.contains(&TranscriptEntryKind::RunOpened));
        assert!(kinds.contains(&TranscriptEntryKind::Progress));
        assert_eq!(kinds.last(), Some(&TranscriptEntryKind::AgentMessage));
        assert_eq!(transcript.runs.len(), 1);
        assert_eq!(transcript.runs[0].run_id, run);

        // Projecting the same session twice produces the same transcript.
        assert_eq!(
            transcript,
            project_session(host.session(&session).expect("session"))
        );
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn a_failed_tool_action_and_a_permission_entry_are_never_collapsible() {
        let (mut host, project, storage) = host("collapse");
        let session = host.create_session("Collapse").expect("session");
        let version = host.session(&session).expect("session").proposal.version;
        let run = host
            .start_run_authorized(&session, version, "test")
            .expect("run");
        host.record_tool_action(&run, "authoring.apply", "rejected", Some(false))
            .expect("tool action");

        let transcript = project_session(host.session(&session).expect("session"));
        let failed = transcript
            .entries
            .iter()
            .find(|entry| entry.outcome == Some(TranscriptOutcome::Failed))
            .expect("failed entry");
        assert!(!failed.collapsible);
        assert_eq!(failed.kind, TranscriptEntryKind::ToolAction);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn a_recorded_model_exchange_becomes_one_collapsible_entry() {
        let (mut host, project, storage) = host("exchange");
        let session = host.create_session("Exchange").expect("session");
        let version = host.session(&session).expect("session").proposal.version;
        let run = host
            .start_run_authorized(&session, version, "test")
            .expect("run");
        host.record_model_exchange(
            &run,
            ModelExchangeRecord {
                turn: 2,
                prompt: "prompt",
                response: "response",
                prompt_tokens: Some(12),
                response_tokens: None,
                finish_reason: "length",
                response_digest: "digest",
                response_excerpt: "response",
            },
        )
        .expect("exchange");

        let transcript = project_session(host.session(&session).expect("session"));
        let entry = transcript
            .entries
            .iter()
            .find(|entry| entry.kind == TranscriptEntryKind::ModelExchange)
            .expect("model exchange entry");
        assert!(entry.detail.contains("turn 2"));
        assert!(entry.detail.contains("finish length"));
        assert!(entry.detail.contains("response unreported"));
        assert!(entry.collapsible);
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn a_captured_frame_entry_offers_navigation_to_its_artifact() {
        let (mut host, project, storage) = host("frame");
        let session = host.create_session("Frame").expect("session");
        let version = host.session(&session).expect("session").proposal.version;
        let run = host
            .start_run_authorized(&session, version, "test")
            .expect("run");
        host.transition_run(&run, AgentRunState::Executing, "execute")
            .expect("executing");
        host.transition_run(&run, AgentRunState::Validating, "validate")
            .expect("validating");
        host.transition_run(&run, AgentRunState::Playtesting, "playtest")
            .expect("playtesting");
        let (artifact_id, _) = host
            .store_captured_frame_artifact(&run, 2, 2, b"png")
            .expect("artifact");

        let transcript = project_session(host.session(&session).expect("session"));
        let entry = transcript
            .entries
            .iter()
            .find(|entry| entry.kind == TranscriptEntryKind::CapturedFrame)
            .expect("captured frame entry");
        assert_eq!(
            entry.navigation,
            Some(TranscriptNavigation::CapturedFrame { artifact_id })
        );
        let _ = fs::remove_dir_all(project);
        let _ = fs::remove_dir_all(storage);
    }
}
