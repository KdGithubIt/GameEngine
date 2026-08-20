# ADR 0162: Intent-Driven AI Studio Execution and Progressive Configuration Disclosure

Status: Accepted
Date: 2026-08-20
Amends: ADR 0131, ADR 0158
Relates to: ADR 0133, ADR 0135, ADR 0142, ADR 0147, ADR 0150, ADR 0155, ADR 0156

## Context

ADR 0131 established AI Studio as a conversation-first surface in which
conversation produces a versioned proposal and an explicit **Go** snapshots that
proposal into a run. ADR 0158 kept that rule and hardened it: sending a message
MUST NOT start, resume, or extend a run, and **Go** MUST remain an explicit
affirmative action drawn among the pinned affordances.

Both records optimized for auditability of the start decision. Neither examined
what the rule costs the user who is trying to get work done, and the shipped
surface now shows three separate problems that all trace back to those two
records rather than to implementation defects.

**Stating an intent does not perform it.** The user types what they want, sends
it, and nothing happens; the text becomes conversation, and the work only starts
after locating and pressing a second control. Every comparable tool the user
evaluates AI Studio against — including the assistant CLIs this project itself
is developed with — treats submission of an instruction as the instruction. The
studio's read-only behavior is desirable and must be kept, but ADR 0158 §4
delivers it by changing what *submission* means rather than by offering a mode,
so the user cannot express "just do it" at all.

**Decisions crowd out the conversation.** ADR 0158 §4 requires the active run's
state, pending permissions, pending questions, and the current proposal with
**Go** to be pinned between transcript and composer. In the shipped surface that
dock also carries the proposal editor, the completion contract, the run control
row, the backend row, and the status card. On a normal embedded presentation the
transcript is left with roughly half the height, which reproduces the exact
complaint ADR 0158 was written to fix.

**Configuration has one tier where it needs two.** ADR 0158 §5 correctly moved
configuration out of the transcript column, but specified only that it move to
"an AI Studio settings surface." The result is one scrolling window in which
choosing which registered model to use sits beside registering a GGUF file,
installing a managed runtime, entering hosted credentials, configuring an
external provider program, and running benchmark campaigns. Selecting a model is
a per-message decision; registering one is a rare machine-local setup task.
Presenting them at the same depth makes the frequent action expensive.

The governance ADR 0131 actually requires — an immutable proposal snapshot per
run, no silent objective change inside a running run, explicit permission
escalation for capabilities, and a completion contract that reports unperformed
checks — does not depend on a distinct **Go** control. It depends on the *commit
point* being unambiguous and recorded. That commit point can be the send action
of an explicitly selected mode.

## Decision

### 1. Two conversation modes replace the Go control

Every AI Studio presentation MUST expose exactly two conversation modes on the
composer:

```text
Ask    read-only; the agent may inspect and answer, and may not write
Build  write-capable; the agent performs the work the message describes
```

The selected mode MUST be visible on the composer at all times, MUST be
persisted per session, and MUST be changeable in one interaction without opening
configuration. No third pipeline mode may be added by this record; a plan-only
workflow is expressed by conversing in Ask mode, which already revises the
proposal without starting a run.

The **Go** control is removed. Submitting a message is the affirmative action:

- in Ask mode, submission runs the read-only harness and MUST NOT create a run
  that can write;
- in Build mode, submission commits the intent and starts a run.

Mode selection is the explicit act that ADR 0158 §4 assigned to **Go**. Because
the mode is displayed on the control that submits, and because write capability
follows the displayed mode rather than an invisible default, submission cannot
be mistaken for a message that does nothing, and a message cannot silently
acquire write capability.

### 2. Intent commit preserves proposal immutability

Build-mode submission MUST perform, in order and atomically from the user's
point of view:

1. derive a new proposal version from the current proposal plus the submitted
   message;
2. record that version in the session's proposal history; and
3. snapshot exactly that version as the immutable input of a new `AgentRun`.

The recorded artifacts are therefore identical to what pressing **Go** produced.
`AgentRun` MUST continue to carry the exact proposal version it started from,
and a running run's goal and acceptance criteria MUST NOT change after the
snapshot, as ADR 0131 §2 requires.

The derived proposal version MUST be visible in the transcript at the head of
the run span it started, and MUST be inspectable and editable before the next
submission. Where the agent could not derive a coherent proposal from the
message, it MUST ask rather than start a run on a guess.

### 3. Messages sent during an active run become the next run's intent

While a run is active, submission MUST NOT mutate that run's snapshot. The
submitted message MUST be appended to the conversation and MUST be carried into
the next proposal revision.

The studio MUST tell the user which run their message applies to at the moment
they submit it, and MUST offer to stop the active run and commit the new intent
instead. Deferral is the default; replacing the active run is an explicit
choice.

Answering a question the run is waiting on is not covered by this section. When
a run is in `AwaitingUser`, submission answers that question and the run
continues, exactly as it does today.

### 4. Pinned affordances are limited to blocking decisions

ADR 0158 §4's pinned set is reduced. Only the following MUST be pinned outside
the transcript, and only while they apply:

- a pending permission request and its decision controls; and
- a pending question the agent is waiting on.

The active run MUST be represented outside the transcript by a single-line
status strip carrying its state and its **Stop** action, and that strip MUST be
absent when no run is active. Stop MUST remain reachable without scrolling
whenever a run can be stopped.

The proposal, the completion contract, code change sets, validation results, and
audit summaries MUST be presented inside the run span in the transcript, as ADR
0158 §3 already specifies for run content. Moving them there does not weaken ADR
0131 §13: a completion contract with an unperformed or failed criterion MUST
still be expanded by default and MUST NOT be collapsed away.

A session with no run and no pending decision MUST show a transcript, a mode-
bearing composer, and nothing else.

### 5. Configuration is disclosed in two tiers

Configuration MUST be split by frequency of use, not by subsystem.

**Selection tier.** Reachable from the composer in one interaction, and limited
to choosing among things that are already configured:

- conversation mode;
- which registered model or backend to use, presented as a list of already
  registered entries with their readiness state; and
- the ADR 0150 quality/effort preference.

The selection tier MUST NOT register, install, remove, download, authenticate,
or benchmark anything, and MUST NOT be the only place a selected entry can be
inspected.

**Configuration tier.** A separate surface reached from the studio header,
organized as named sections rather than one scrolling column:

```text
Models       GGUF registration, discovery, managed runtime setup and removal,
             residency and resource controls
Providers    external agent program and arguments, hosted and enterprise
             endpoints and credentials, confinement requirement
Environment  execution environment and WSL placement
Benchmarks   benchmark tasks, campaigns, and results
Remote       companion gateway
```

A control that changes what exists on the machine, what the project may reach,
or what credentials are stored MUST live in the configuration tier. A control
that only chooses among existing entries MAY live in the selection tier.

Code changes are run output, not configuration, and MUST NOT be presented in
either tier; they belong to the run span under §4.

### 6. What this record does not relax

The following remain exactly as their originating records specify, and no part
of §1–§5 may be read as weakening them:

- ADR 0131 §8 capability escalation. Network access, external asset acquisition,
  runtime launch and control, frame capture, raw workspace filesystem access,
  and arbitrary command execution still require explicit approval with the
  documented scopes. Build mode grants no capability by itself.
- ADR 0158 §6. Network egress, credential storage, elevation, restart,
  environment removal, and code application MUST still state their consequence
  at the point of the action.
- ADR 0131 §13 completion reporting, including unperformed criteria.
- ADR 0158 §2 transcript projection from host state, and its ordering rules.
- Stop semantics and the interrupt-for-editing path.

### 7. Presentation parity

The mode control, the selection tier, the run status strip, the reduced pinned
set, and the transcript-resident run content MUST be equivalent in the embedded
and detached presentations, as ADR 0147 and ADR 0158 §7 require. The ADR 0133
companion MUST NOT present a **Go** affordance that the local presentations no
longer have.

## Consequences

The studio becomes usable the way the tools it is compared to are usable: the
user writes what they want and it happens, and the read-only guarantee is a
visible mode rather than a change in what sending means.

The transcript regains the height ADR 0158 §1 intended for it, because the dock
below it shrinks to a composer plus, at most, one blocking decision and one
status line.

Choosing a model becomes a one-interaction action, and machine setup stops
competing with it for the same window.

The start decision is no longer a separate control, so the audit story now rests
on the mode indicator and the recorded proposal version. Any change that lets
write capability differ from the displayed mode is a defect of this record, not
a detail: mode display and effective capability MUST be derived from one value.

Build mode makes it possible to start a run by typing and pressing send, which
is the point, and which means an accidental send starts work. Stop remains
immediate, capability escalation remains gated, and the run span records the
exact snapshot, so the recovery path is the same one a mistaken **Go** already
had.

Removing **Go** invalidates the affordance ADR 0158 §4 named, so any
documentation, companion surface, or test that asserts the presence of a **Go**
control is updated by the implementing change.

`ai_studio.rs` has grown past seven thousand five hundred lines. Splitting the
configuration tier into sections is the natural point to extract configuration
drawing out of that module; this record does not require a new crate, and does
not require the extraction to happen in the same change as §1.

## Alternatives Considered

**Keep Go and press it automatically on send.** Preserves ADR 0158 §4's literal
text. Rejected: the control remains on screen while no longer being the thing
that starts work, which is more confusing than either rule alone, and it leaves
the user unable to tell whether a given send will run.

**Keep Go and reduce it to a keyboard shortcut.** Rejected for the same reason
in weaker form: the user still expresses one intent through two actions, and the
shortcut is undiscoverable.

**Three modes: Ask, Plan, Build.** A Plan mode that revises the proposal without
running is expressible, but ADR 0131 §1 explicitly refuses to make the user
choose a pipeline before discussing the work, and Ask mode already produces
proposal revisions. Rejected as a third state that must be explained without
enabling anything new.

**Let a message sent during an active run steer that run.** Rejected: it is the
silent scope expansion ADR 0131 §2 exists to prevent. Deferring to the next
proposal revision, with an explicit option to stop and recommit, keeps the
running snapshot immutable while still letting the user redirect.

**Keep one settings window and merely collapse its sections.** Cheaper and needs
no record. Rejected as the target state because model *selection* would remain
at the same depth as model *installation*, which is the actual complaint; it
remains an acceptable partial step for the configuration tier only.

**A persistent right-hand rail for run state.** Already rejected by ADR 0158 for
the 460-point embedded minimum width, and this record does not revisit it.

## Compatibility and Migration

No serialized project data changes. `AgentSession`, `AgentRun`, `AgentEvent`,
`ConversationMessage`, and `AgentProposal` keep their schemas and their schema
versions. A run started by Build-mode submission is byte-compatible with a run
started by **Go**, because it is the same snapshot of the same proposal type.

`AiStudioPreferences` gains a conversation-mode field added with
`serde(default)` in the manner of its existing fields, so preference files
written before this change continue to load. The default MUST be Ask, so an
existing installation does not gain write-on-send without the user selecting it
once.

No public API across crate boundaries changes. `AiStudioPanel` remains the
Editor's entry point.

The ADR 0133 companion presents the same modes and the same reduced pinned set
over the existing ordered event projection. Because the companion cannot edit a
proposal, presenting Build there requires one backward-compatible addition to
its HTTP surface: `POST /api/sessions/{id}/intent`, which records the submitted
instruction, derives the proposal version that instruction commits, and starts a
run from exactly that version. Every existing operation is unchanged, including
`POST /api/sessions/{id}/go`, so a client written before this record keeps
working against a host that implements it. The addition obeys the ADR 0133
reconnect rules: it carries a `request_id` and a base proposal version, a
repeated `request_id` returns the first response rather than committing a second
version or starting a second run, and a stale base version is rejected rather
than silently rebased.

ADR 0131 and ADR 0158 remain Accepted. This record amends ADR 0131 §2's
description of the commit point, replaces ADR 0158 §4, and refines ADR 0158 §5;
all other sections of both records stand unchanged.
