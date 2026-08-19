# ADR 0158: Conversation-First AI Studio Presentation and Transcript Projection

Status: Proposed
Date: 2026-08-19
Builds on: ADR 0131
Relates to: ADR 0133, ADR 0135, ADR 0139, ADR 0142, ADR 0147, ADR 0150, ADR 0155

## Context

ADR 0131 defines AI Studio as a conversation-first surface: conversation
produces a versioned proposal, an explicit **Go** snapshots that proposal into a
run, and the studio presents semantic progress, changes, and audit history in a
human-readable timeline. The current implementation satisfies the semantics but
not the presentation.

The embedded and detached presentations draw one vertical stack of independent
cards: a standing explanation, Session, Proposal, Provider, Code changes, Run
timeline, and a status line. Every card is present at all times regardless of
whether the session has a run. Within the Session card the conversation is
confined to a fixed 180-point scroll area, and the model-backend configuration
is drawn between the message list and the message composer. Managed Local AI
setup, execution environment, GGUF registration, benchmark selection, hosted
credential entry, and the remote companion gateway are all drawn inline in the
same column as the conversation.

Two consequences follow. First, the surface reads as a settings dialog that
contains a chat box rather than a conversation with the project's agent, which
is what ADR 0131 specifies and what users expect from the tools they compare it
to. Second, the elements ADR 0131 §1 requires to be visible — provider and
connection state, proposal, active run, permissions, changes, validation state,
and the stop action — compete for the same column as the conversation instead of
being reachable when they matter.

The material the studio must show already exists as ordered host state. An
`AgentRun` carries a sequenced `AgentEvent` log whose kinds already cover
proposals, permission requests and resolutions, tool actions, semantic progress,
code workspace preparation and application, validation, playtest, captured
frames, resource policy, cancellation, failure, and completion. That log is
already projected for the ADR 0133 companion through an ordered cursor. What is
missing is a decision that this log — not a set of hand-placed cards — is the
studio's primary surface.

## Decision

### 1. One transcript is the studio's primary surface

Every AI Studio presentation MUST draw a single conversation transcript as its
primary surface, occupying the presentation's available height, with a message
composer pinned to its lower edge. No other section may be permanently stacked
above or below the transcript.

The transcript MUST scroll independently of the composer, and MUST NOT be given
a fixed height that is smaller than the space the presentation can offer it.

### 2. The transcript is a projection of Agent Host state, not a second model

Transcript entries MUST be derived from `AgentSession` conversation messages and
`AgentRun` event logs. The transcript MUST NOT hold authoritative state, MUST
NOT be the only record of anything, and MUST NOT reorder or summarize host
events in a way that changes their meaning.

Ordering MUST be deterministic. `ConversationMessage` and `AgentEvent` both
carry `created_unix_ms`; entries are ordered by that timestamp, with ties broken
by run start order and then by event sequence, so the same session renders
identically in every presentation and on every reopen.

The projection MUST live with the Agent Host rather than inside drawing code, so
that the embedded presentation, the ADR 0147 detached window, a later ADR 0147
tier-2 frontend, and the ADR 0133 companion render the same entries. GUI code
selects how an entry looks; it does not decide what an entry is.

Every `AgentEventKind` MUST map to exactly one entry kind. An event kind with no
mapping is a defect, not a reason to drop the event: unmapped kinds MUST render
as a generic entry carrying their message rather than disappearing from the
transcript.

### 3. Run structure is expressed inside the transcript

A run appears in the transcript as a bounded span opened by its Go and closed by
its completion, failure, or cancellation, carrying the immutable proposal
snapshot it was started from. Proposal revisions, permission requests and their
resolutions, authoring mutations, code change sets, asset acquisitions,
validation results, playtest results, captured frames, and the audit summary
appear as entries within that span, in host order.

An entry MAY be collapsed by default when its detail is long, but the fact that
it occurred, and its outcome, MUST be visible without expanding it. Collapsing
MUST NOT be used to hide a permission escalation, an escape-hatch operation, a
failed validation gate, or an unperformed completion criterion.

Where the Editor can resolve a referenced entity, asset, graph node, material,
or document, the entry MUST offer navigation to that Editor context, as ADR 0131
§14 requires.

### 4. Decisions stay reachable without scrolling

Conversation scrolls; decisions must not scroll away. The studio MUST pin the
following outside the transcript for as long as they apply:

- the active run's state and its **Stop** action;
- any pending permission request and its decision controls;
- any pending question the agent is waiting on; and
- the current proposal version, with **Go**.

These pinned affordances sit between the transcript and the composer and MUST be
absent when they do not apply, so a session with no run shows a transcript and a
composer and nothing else.

**Go** MUST remain an explicit affirmative action. Sending a message MUST NOT
start, resume, or extend a run, and MUST NOT be presented as if it might.
Likewise, a permission decision MUST NOT be inferable from ordinary message
sending.

### 5. Configuration is a separate surface

Model backend selection, quality preference, execution environment, managed
runtime setup and removal, GGUF registration, model discovery, benchmark task
and campaign controls, hosted and enterprise endpoint and credential entry,
confinement requirement, external provider program and arguments, and the remote
companion gateway MUST move out of the transcript column into an AI Studio
settings surface reached from the studio header.

The settings surface is machine-local configuration, not conversation. It MAY
reuse the ADR 0147 detached-viewport mechanism or draw as a modal within the
presentation; either way it MUST NOT own Agent Host state, and closing it MUST
NOT affect an active run.

The composer MAY carry one compact control showing the selected model backend
and its connection state, because ADR 0131 §1 requires provider and
authentication state to be visible. Selecting a different backend from that
control is permitted; configuring one is not.

### 6. UI text is a label; specification text is on demand

Control labels MUST be short enough to read as labels. The normative sentences
that currently sit inline — processing posture, residency semantics, confinement
scope, removal scope, and similar — MUST be reachable from the control they
describe rather than drawn beside it by default.

Reducing a sentence to a label MUST NOT reduce what the user is told before an
irreversible or outward-facing action. Network egress, credential storage,
elevation, restart, environment removal, and code application MUST state their
consequence at the point of the action.

### 7. Presentation parity

The transcript, its entries, the pinned affordances, and the composer MUST be
equivalent in the embedded and detached presentations, as ADR 0147 UX continuity
already requires. Layout, scroll position, collapse state, and draft text remain
presentation-local.

## Consequences

The studio becomes legible as a conversation, and the ADR 0131 §14 timeline
stops being one card among several and becomes the surface itself.

Adding a host capability that emits a new `AgentEventKind` now requires deciding
how it reads in the transcript. This is a deliberate cost: it replaces the
current pattern where a new capability grows another always-visible card.

The ADR 0133 companion and any later ADR 0147 tier-2 frontend gain a shared
entry model instead of each re-deriving presentation from raw host state. The
companion's existing ordered event cursor is the transport for that model.

`ai_studio.rs` has grown past six thousand lines that mix host polling,
provider orchestration glue, configuration, and drawing. Extracting the settings surface
and the transcript projection separates those responsibilities. This ADR does
not require a new crate; it requires that the projection not live in drawing
code.

Users lose the ability to see conversation and full model configuration at the
same time. This is accepted: configuration is changed rarely and read rarely,
and it currently costs the conversation most of its height on every frame.

## Alternatives Considered

**Conversation with a persistent side rail.** Draw the transcript on the left
and keep Proposal, Run, and permissions in a permanent right-hand column. This
preserves more of the current structure and keeps decisions visible without the
pinned-affordance rule. It was rejected because the studio must remain usable in
the embedded presentation at its 460-point minimum width, where a side rail
leaves the conversation too narrow to read, and because a permanent rail
reproduces the present problem of showing run state for sessions that have no
run.

**Tidy the existing cards.** Collapse cards by default, shorten the prose, let
the conversation area grow, and move configuration below the fold. This is
substantially cheaper and needs no ADR. It was rejected as the target state
because it leaves the conversation as one card among several, which is the
structural complaint; it remains a reasonable partial step if the full change
cannot be scheduled.

**A conversation surface that hides governance detail.** Present only messages,
and move proposals, permissions, diffs, validation, and audit into secondary
views opened on demand. Rejected: ADR 0131 §1 and §14 require these to be
visible without raw inspection, and ADR 0131 §13 requires completion to report
unperformed checks rather than presenting a clean transcript.

**Rendering the transcript directly from `run.events` in GUI code.** Rejected
because the ADR 0133 companion and later frontends would each re-derive entry
meaning, and the three surfaces would drift.

## Compatibility and Migration

No serialized format changes. `AgentSession`, `AgentRun`, `AgentEvent`,
`ConversationMessage`, and `AgentProposal` are unchanged, and their schema
versions are unchanged. The transcript is derived from fields that already
exist, including the `created_unix_ms` values both message and event types
already carry. Sessions written before this change render without migration.

`AiStudioPreferences` gains presentation-local state only if the settings
surface needs it; any such field is added with `serde(default)` in the manner of
the existing fields, so older preference files continue to load.

The ADR 0133 companion protocol is unchanged by this ADR. If the entry model is
later exposed over that protocol, it is a versioned addition to the existing
ordered event projection, not a replacement for it.

No public API across crate boundaries changes. `AiStudioPanel` remains the
Editor's entry point.

## Verification

Implementation must show:

- one presentation-independent projection producing identical ordered entries
  for the same session in the embedded and detached presentations;
- deterministic ordering across a session that interleaves conversation messages
  with events from more than one run;
- every `AgentEventKind` reaching the transcript, including kinds with no
  specific rendering;
- pinned Stop, permission decision, pending question, and Go present exactly
  when they apply and absent otherwise;
- no run started or permission granted as a side effect of sending a message;
- the transcript growing with the presentation rather than a fixed height; and
- an active run surviving the settings surface being opened and closed.

Presentation work under this ADR requires Visual Validation for both the
embedded and detached presentations, per `docs/EDITOR_VISUAL_VALIDATION.md`.

## Non-Goals

This ADR does not change Agent Host ownership, proposal immutability, Go
semantics, permission semantics, confinement policy, ADR 0135 resource
arbitration, ADR 0139 working-copy coherency, or the authoring boundary.

It does not make AI Studio remotely reachable, does not define a mobile client,
does not add token streaming or any provider-facing behavior, and does not
change the palette or type scale shared with the Launcher.

It does not remove the reattach operation, which ADR 0147 defines as a
presentation operation.
