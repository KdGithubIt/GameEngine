# ADR 0157: AI Runtime Debugging, Deterministic Playtest Control, and Host-Owned Observation

Status: Accepted
Date: 2026-08-18
Builds on: ADR 0026, ADR 0064, ADR 0103, ADR 0131, ADR 0141
Relates to: ADR 0138, ADR 0142, ADR 0149, ADR 0156

## Context

GameEngine already has most of the low-level pieces required for an AI agent to
debug a running game. ADR 0026 defines engine-level virtual input and explicitly
forbids OS-level input synthesis. ADR 0064 defines deterministic fixed-tick input
replay. Play mode can already pause, resume, advance one fixed/frame step, record
and replay input, capture the Game View, and expose runtime debugger snapshots.
ADR 0141's Native Agent Runtime can request `runtime_input`, and AI Studio can
start Play, queue an `InputCommand`, capture a frame, and stop Play through
Editor-owned operations.

Those pieces are not yet a complete AI debugging contract. A model can request a
key-down action and later request key-up, but the time between those two model
turns is LLM inference wall-clock time, not simulation time. A slow model could
therefore hold a gameplay input for several seconds when it intended a short
press. The model also does not have one governed application surface for pause,
resume, deterministic multi-tick input sequences, single-step inspection,
condition waits, assertions, replay-driven reproduction, and typed runtime
observation.

That gap matters beyond benchmarking. The useful product behavior is an AI that
can reproduce a gameplay bug, pause at the interesting state, inspect runtime
values, step the simulation, form a diagnosis, modify code or authoring data
through the existing governed paths, and then replay the same interaction to
check the repair. ADR 0156 also needs Runtime Interaction and Visual Evaluation
tasks to measure this real production capability rather than a benchmark-only
shortcut.

A project-wide decision is therefore required for deterministic AI playtest
control, host-owned timing, pause/step semantics, input-sequence ownership,
observation and assertion surfaces, replay integration, visual capability,
permissions, interference handling, and benchmark reuse.

## Decision

### 1. AI runtime debugging is a first-class application capability

GameEngine introduces a governed AI runtime-debugging / automated-playtest
surface above `RuntimePlayState` and the engine virtual-input layer.

Conceptually:

```text
AI Studio / Native Agent Runtime
              |
       Agent Host permissions
              |
      RuntimeDebugController
       /       |        \
   control   observe   assertions
       \       |        /
        RuntimePlayState
              |
       VirtualInputQueue
```

The model chooses semantic debugging actions. The host owns simulation timing,
Play lifecycle, input scheduling, observation truth, assertion evaluation,
timeouts, and cleanup. The model never gains direct mutable access to the
runtime ECS or the operating system.

This surface is product functionality. ADR 0156 benchmark tasks reuse it; the
benchmark does not receive a more privileged runtime-control API.

### 2. Simulation time, not LLM wall-clock time, controls gameplay input

A model turn is not a clock. Input duration MUST NOT be defined by waiting for a
future model response.

The authoritative scheduling unit for deterministic playtest actions is the
runtime fixed tick from ADR 0064. The controller accepts a bounded plan that can
conceptually contain actions such as:

```text
tick 0   -> KeyW down
tick 48  -> KeyW up
tick 54  -> Space down
tick 57  -> Space up
tick 90  -> pause and observe
```

The host freezes the resolved schedule before execution and injects commands at
the defined runtime boundaries. Model inference may take milliseconds or
seconds without changing the intended hold duration.

A convenience duration such as `hold W for 0.8 seconds` MAY be accepted by an
outer application API, but it MUST be resolved to the session's fixed-tick
schedule before execution. The resolved tick plan, not wall-clock delay, is the
reproducibility identity. Non-finite, negative, unbounded, or overflowing
durations/ticks are rejected.

### 3. Input plans use the existing virtual-input and replay vocabulary

AI debugging does not invent OS automation or a second gameplay input model.
Scheduled commands use the `InputCommand` vocabulary from ADR 0026 and enter the
runtime through `InputSource::AiAgent` and the normal `VirtualInputQueue` drain
boundary.

A first-release debug plan SHOULD support at least:

- key down/up;
- mouse button down/up;
- mouse move/delta/scroll;
- gamepad commands when the underlying runtime command is supported;
- bounded hold as host-expanded down/up commands;
- ordered multi-command sequences; and
- explicit release/cleanup of input state owned by the debug session.

The controller tracks the pressed/active state it injected so abort, stop,
timeout, permission revocation, or failed Play cannot leave AI-owned controls
stuck. Cleanup MUST NOT synthesize desktop input.

When an interaction must be persisted or reproduced, the controller SHOULD reuse
ADR 0064 `InputReplay` rather than define another serialized input format. A new
persisted debug-plan schema, if later required, needs its own versioned contract
and cannot silently change `*.replay.json` semantics.

### 4. Pause, resume, and deterministic step are host-owned debug operations

The governed surface exposes Play-state controls equivalent to:

```text
pause
resume
step 1
step N
```

Pause and resume delegate to the existing runtime pause state. While paused, one
step uses the existing single-step contract: exactly one fixed and frame schedule
pass at the runtime fixed delta. `step N` is a bounded host loop over that same
primitive; it is not implemented by repeatedly asking the model to continue.

The model may request these operations only inside an authorized playtest/debug
plan. Host-reported Play/paused/tick state is authoritative; a model message that
claims a pause or step occurred does not make it true.

### 5. Runtime observation is typed, read-only, bounded, and truthful

The controller exposes bounded read-only observations from the running world. It
reuses existing runtime-debug evidence where available instead of requiring the
model to infer all state from pixels.

First-release observation SHOULD include, where supported:

- Play/running/paused state and fixed tick count;
- runtime entity identity mapped to stable authoring identity when available;
- selected readable component values such as transform and velocity;
- input state and resolved input actions;
- runtime diagnostics and relevant failure state;
- performance/tick counters;
- animation / Animation Graph debug state;
- Behavior Tree or other registered domain-debug snapshots; and
- host-owned Game View frame capture.

Unavailable values remain explicitly unavailable. Observation MUST NOT grant
arbitrary ECS mutation, arbitrary memory reads, hidden engine internals, project
credentials, or an unbounded dump of the runtime world. Domain-specific debug
providers may extend the typed observation surface under their existing
architecture contracts.

### 6. Waiting and assertions are evaluated by the host

Useful automated debugging needs more than `sleep`. The controller MAY expose
bounded conditions such as:

```text
wait_ticks(60)
wait_until(entity state predicate, timeout_ticks)
assert(component predicate)
assert(no blocking runtime diagnostic)
```

Conditions and assertions use a typed, allow-listed predicate vocabulary over
host-owned observations. They MUST NOT evaluate arbitrary model-provided Rust,
Rhai, shell, SQL, debugger expressions, or raw memory addresses.

Every wait has an explicit tick/action/time budget. The host reports whether the
condition passed, failed, timed out, or became unavailable. A model cannot mark
a host assertion successful merely by stating that the expected outcome
occurred.

Real wall-clock waiting may still exist for infrastructure operations such as a
game-code build or model startup, but it is separate from simulation-time input
and MUST NOT be used as the reproducibility clock for gameplay actions.

### 7. Replay is the normal reproduction primitive for before/after debugging

When an AI finds a reproducible gameplay issue, GameEngine SHOULD be able to
retain the deterministic input interaction as machine-local debug evidence or an
explicitly saved ADR 0064 replay.

A normal repair loop becomes:

```text
start Play from known state
  -> execute/replay deterministic input
  -> pause / inspect / capture
  -> diagnose
  -> stop Play
  -> repair through governed code/authoring paths
  -> rebuild/restart from the same known state
  -> replay the same input
  -> re-run host assertions / observations
```

The first release does not require arbitrary runtime rewind or full world
snapshots. Reproduction may restart Play from the known fixture/project state and
replay input from the beginning. Named replay checkpoints from ADR 0064 may be
used as evidence markers without implying snapshot restore.

### 8. Visual debugging is capability-gated and complements structured state

Frame capture remains host-owned. A captured frame may be attached to a model
turn only when the selected `ModelBackend` truthfully reports image-input
capability and the provider adapter implements the required image transport.

A text-only model can still perform Runtime Interaction debugging through typed
state, diagnostics, replay, and assertions. It MUST NOT be told that it visually
inspected a frame it could not receive. Visual completion remains a separate
capability/evidence dimension for ADR 0142/0156.

Image bytes, resize/encoding policy, and provider-specific multimodal transport
belong outside the engine ECS. The engine/runtime remains responsible only for
the authoritative captured pixels and observation identity.

### 9. Permissions, budgets, and human interference remain explicit

AI runtime debugging remains governed by Agent Host and the immutable playtest
plan. Starting Play, sending input, capturing frames, observing protected state,
and any later mutation capability require the same permission model as normal AI
Studio work.

A debug plan records bounded limits such as maximum model turns, maximum runtime
ticks or interaction actions, maximum captures/observations, and condition
timeouts. Exceeding a budget fails or pauses the plan rather than silently
continuing indefinitely.

For ordinary interactive debugging, human and AI input may coexist according to
the runtime's input policy. For deterministic replay, benchmark, or a run marked
reproducible, unplanned human input that can affect the same runtime MUST be
blocked for that session or mark the evidence contaminated/non-comparable. The
product must never pretend a human-modified run is deterministic.

### 10. The controller owns safe failure and cleanup boundaries

Runtime crashes, Play stop, build failure, invalid input, unavailable
observation, model interruption, permission denial, and assertion failure are
observable debug outcomes. They are not converted into success by retrying until
the model happens to pass.

On termination the controller cancels pending scheduled actions, releases
AI-owned input state, preserves already collected evidence, and returns control
to normal Editor operation. A restart is allowed only when the caller's plan
explicitly permits a new attempt and the evidence identity makes that attempt
distinct.

### 11. ADR 0156 benchmarks the same production debugging capability

ADR 0156 Runtime Interaction tasks use this controller to launch Play, execute a
frozen input/replay plan, observe state, pause/step when required, and evaluate
host-owned task assertions. Visual Evaluation adds frame input only for a
capable backend.

Benchmark hidden tests, golden state, scoring thresholds, and evaluation oracle
remain host-only under ADR 0156. This controller returns production observations
and assertion outcomes; it does not expose benchmark answer keys to the model.

A benchmark result that bypasses this governed path is not equivalent evidence
for production AI debugging capability.

### 12. Debug evidence is application/session data, not canonical authoring data

Transient debug plans, observation history, captures, and benchmark-owned replay
artifacts are machine-local application/session data by default. They MUST NOT be
written into canonical project authoring files merely because an AI generated
them.

A user may explicitly save or export a replay/capture through the existing
product workflow. Persisted formats retain their own versioning/provenance
contracts. Debug evidence MUST NOT contain credentials or silently serialize the
model conversation.

## Implementation

The first-release implementation should add an application-layer controller over
existing `RuntimePlayState` capabilities rather than moving AI policy into the
engine crate. Existing primitives such as `queue_input`, `set_paused`,
`request_single_step`, replay recording/playback, frame capture, input debug
snapshots, entity/component debug snapshots, animation/domain snapshots, and
AI Studio runtime actions remain the implementation foundation.

The Native Agent structured-action vocabulary may gain typed runtime-debug
actions, but Agent Host remains authoritative for validation and execution. The
model produces a bounded semantic plan; AI Studio/application code translates it
into exact runtime actions and records their host-observed outcomes.

New cross-crate public API, serialization, stable identifiers, or replay-format
changes are not implied by this ADR. If implementation cannot stay additive
behind the existing boundaries, the affected contract must be reviewed before
changing it.

## Verification

Implementation must prove at least:

- the same fixed-tick input plan produces the same injected command/tick order
  regardless of model-turn wall-clock latency;
- bounded hold expands to deterministic down/up commands and abort cleanup does
  not leave AI-owned input pressed;
- AI input remains engine-level and never synthesizes OS keyboard/mouse input;
- pause/resume reflect host state and `step 1` executes one fixed/frame pass
  while paused;
- multi-step execution uses the same primitive and respects explicit budgets;
- typed waits/assertions are host-evaluated and reject arbitrary code/expression
  execution;
- runtime entity/component/input/diagnostic/domain observations are read-only,
  bounded, and truthful about unavailable values;
- replay-driven reproduction uses the ADR 0064 fixed-step/input path;
- unplanned human input is blocked or marks deterministic evidence contaminated;
- frame capture reaches only backends with implemented image-input capability;
- a text-only backend can still complete non-visual runtime debugging without a
  false visual claim;
- failure/abort/permission revocation cancels pending actions and returns the
  Editor to a usable state; and
- ADR 0156 Runtime Interaction/Visual Evaluation use this production surface and
  cannot retrieve the benchmark's host-only evaluator.

Any new AI Studio runtime-debug timeline, control, observation, replay, capture,
or assertion UI requires Editor Visual Validation.

## Non-goals

This ADR does not:

- synthesize desktop/OS input or control applications outside the GameEngine Play
  world;
- let LLM response latency define gameplay input duration;
- grant arbitrary mutable ECS access, raw memory debugging, or unrestricted code
  evaluation to the model;
- require arbitrary runtime rewind or full world-state snapshots in the first
  release;
- replace the existing human Runtime Debugger, Game View controls, or ADR 0064
  replay format;
- make image input mandatory for text-only debugging tasks;
- let a model self-award host-owned completion or benchmark gates; or
- create a benchmark-only runtime control path that is more capable than normal
  AI Studio debugging.
