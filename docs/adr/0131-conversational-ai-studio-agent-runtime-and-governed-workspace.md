# ADR 0131: Conversational AI Studio Agent Runtime and Governed Workspace

Status: Accepted
Date: 2026-08-16
Relates to: ADR 0035, ADR 0117, ADR 0121

## Context

ADR 0121 establishes MCP as the standard structured AI authoring interface for
an Editor-open project and keeps the live Editor authoring session authoritative.
ADR 0035 separately defines runtime input and frame observation. Those decisions
make external AI authoring possible, but they do not define a first-class
product experience for asking an AI to design, build, test, and iteratively
refine a game from inside GameEngine.

A production-quality AI creation workflow needs more than a prompt box or a
single model API integration. Different users may have access to external agent
runtimes such as Claude Code or Codex through provider-managed accounts, direct
model APIs through separate credentials, or local models through runtimes such
as Ollama. External coding agents already own tool loops, context management,
process execution, and provider authentication, while a native in-engine agent
must own those concerns itself. Treating both shapes as one undifferentiated
"LLM provider" would produce a weak abstraction and tie the Editor to one
vendor's authentication and process model.

The interaction model also needs to remain collaborative. A user may begin with
an incomplete idea, expect the AI to ask design questions, refine the proposal
through conversation, and only then authorize implementation. During execution,
new design choices may appear that require another human decision. A one-way
Plan -> Build pipeline would make this workflow unnecessarily rigid.

Game creation may require authoring mutations, project Rust source changes,
asset acquisition, validation commands, runtime control, frame capture, and in
some cases lower-level filesystem or shell access. GameEngine should keep normal
operations inside typed and reviewable services, while retaining a controlled
escape hatch for workflows that cannot be expressed through those services.
Application-level permission policy must not be misrepresented as an OS-level
sandbox guarantee.

Finally, AI conversation and run history is useful across Editor restarts and
machines. Keeping all history only in local user state prevents deliberate team
or multi-PC sharing, while writing credentials, temporary workspaces, or all
private conversation into canonical project data would be unsafe and noisy.

A project-wide decision is therefore required for the AI Studio product model,
agent runtime and model-backend boundaries, execution state machine, permission
broker, code and asset side effects, session persistence, human/agent
concurrency, and completion criteria.

## Decision

### 1. AI Studio is the project-scoped conversational creation surface

The Editor owns a project-scoped **AI Studio** surface above the MCP authoring
interface. AI Studio is conversation-first rather than mode-first. The normal
workflow is:

```text
User intent
  -> conversation and design questions
  -> structured proposal revisions
  -> explicit Go
  -> autonomous or interactive run
  -> validation and playtest
  -> completion or further conversation
```

The user and agent MAY continue the same conversation before, during, and after
a run. AI Studio MUST NOT require the user to choose a one-way Ask, Plan, Build,
or Autopilot pipeline before discussing the desired result.

The UI SHOULD make the current provider, connection/authentication state,
proposal, active run, permissions, changes, validation state, and stop action
visible without requiring raw terminal inspection.

### 2. Conversation produces a versioned proposal; Go snapshots it into a run

Each AI session maintains a versioned structured proposal in addition to the
human-readable conversation. A proposal contains at least:

- goal;
- agreed requirements;
- assumptions;
- acceptance criteria;
- planned project, code, and asset changes at an appropriate level;
- validation and playtest plan; and
- capabilities expected to be required.

Proposal revisions are mutable while the user and agent are discussing the
work. Pressing or invoking **Go** snapshots one exact proposal version into a
new immutable `AgentRun` input. Later conversation may create a new proposal
version, but it MUST NOT silently change the objective or acceptance criteria of
an already-running run.

A session therefore has the conceptual structure:

```text
AgentSession
  conversation
  proposal v1
  proposal v2
  ...
  run 1 -> proposal snapshot v2
  run 2 -> proposal snapshot v5
```

A run may request clarification, but a material change to the agreed goal or
acceptance criteria requires an explicit proposal revision and a new or resumed
run decision rather than silent scope expansion.

### 3. Runs are resumable state machines with structured events

An `AgentRun` is not represented as an opaque child process. The host tracks a
state machine whose implementation may refine the following conceptual phases:

```text
Inspecting
Planning
Executing
AwaitingUser
Validating
Playtesting
Evaluating
Repairing
Completed
Failed
Cancelled
```

Transitions are not one-way. Validation or playtest failure may return to
`Repairing` and then to execution or validation. A design ambiguity, permission
escalation, or user-dependent choice may transition to `AwaitingUser` and resume
when the user responds.

Ordinary recoverable implementation problems such as compiler errors,
authoring validation failures, import failures, or minor layout adjustments
SHOULD be repaired autonomously within the active proposal and permission
budget. The agent SHOULD ask the user when a meaningful product decision,
permission escalation, destructive operation, or material scope deviation is
required.

The host emits provider-independent structured `AgentEvent` values for lifecycle
and UX. Events SHOULD cover at least plan/proposal updates, step start and
completion, tool calls, change previews and commits, permission requests,
validation, playtest, repair, cancellation, failure, and completion. Provider
stdout/stderr MAY be retained for diagnostics, but raw terminal text MUST NOT be
the only source of truth for the AI Studio UI.

### 4. Agent orchestration is an application layer above authoring semantics

GameEngine introduces a GUI-free agent orchestration boundary conceptually split
into these responsibilities:

- **agent core contracts**: session/run identifiers, proposals, events,
  capabilities, permissions, provider descriptors, and run state;
- **agent host**: project-scoped session/run lifecycle, provider registry,
  permission broker, approvals, cancellation, persistence, and orchestration;
- **external agent runtime adapters**: launching and communicating with coding
  agents that already own their own agent loop;
- **native agent runtime**: an engine-owned model/tool loop for direct model
  backends;
- **agent code workspace**: source snapshot/checkpoint, diff, patch application,
  and engine-managed validation; and
- **asset acquisition service**: search/acquire providers above the normal asset
  import and manifest pipeline.

The implementation MAY place these responsibilities in separate workspace
crates as they become substantial. Regardless of package count, they MUST NOT
move agent lifecycle, provider authentication, network access, or shell policy
into `engine-authoring`, runtime ECS crates, or `engine-mcp`.

The Editor AI Studio is a frontend over the agent host. It MUST NOT duplicate
provider-specific orchestration or authoring business rules in GUI code.

### 5. External agent runtimes and model backends are distinct abstractions

GameEngine distinguishes an **agent runtime** from a **model backend**.

An external agent runtime already owns its own model interaction and agent loop.
Examples include coding-agent CLIs. A native agent runtime is owned by
GameEngine and delegates model inference to a `ModelBackend` abstraction.
Conceptually:

```text
AgentRuntime
  ExternalAgentRuntime
    provider-managed coding agent
    custom compatible agent process

  NativeAgentRuntime
    ModelBackend
      local model runtime
      hosted model API
      future enterprise backend
```

Claude Code, Codex, local model runtimes, hosted APIs, and future providers MUST
NOT be forced into one API-key-shaped provider interface.

Authentication is a provider capability. Supported authentication classes MAY
include provider-managed session/login, API credential, local/no-auth, and
enterprise-managed authentication. GameEngine MUST NOT require an API key when
an external provider legitimately manages its own authenticated session.

Provider-managed credentials remain owned by that provider. API credentials
owned by GameEngine MUST be stored in an OS credential facility or equivalent
secure user secret store. Credentials, tokens, and provider login material MUST
NOT be written to canonical project data or shared AI session records.

### 6. Structured project authoring continues to use the Editor MCP endpoint

ADR 0121 remains authoritative for project authoring. Both external and native
agent runtimes use the project-scoped Editor MCP interface for semantic
inspection and authoring mutation. They MUST NOT introduce a second authoring
writer or bypass the live Editor session by replacing Scene, Graph, Prefab, UI,
Material, settings, animation, or other authoring files directly.

The AI Agent Bridge from ADR 0035 remains the runtime input, frame observation,
and visual interaction path. It is used for playtest interaction and visual
evaluation, not as a substitute for typed authoring operations.

Source-code changes and asset acquisition are separate domains from authoring
transactions and use the governed services described below.

### 7. GameEngine owns a layered agent permission broker

The existing authoring permission vocabulary remains the shared authorization
boundary for authoring operations. Agent orchestration adds application-level
capabilities for concerns that do not belong in `engine-authoring`, including:

- network access;
- external asset acquisition;
- runtime launch;
- runtime input/control;
- frame capture;
- raw workspace filesystem access; and
- arbitrary command execution when it is broader than an engine-managed tool.

AI Studio exposes these capabilities as understandable controls, such as
checkboxes or equivalent policy toggles. Safe project authoring writes approved
by pressing Go do not require a confirmation dialog for every transaction.

Permission escalation supports at least the following approval scopes when the
operation permits them:

```text
Allow once
Allow for this run
Allow for this project
Deny
```

Persistent project policy stores capability decisions, not brittle literal
command strings. Credentials are never part of the policy record.

### 8. Managed services are the default; low-level access is an explicit escape hatch

The default agent path is **Managed**:

```text
project authoring -> MCP
source code       -> agent code workspace / patch service
assets            -> asset acquisition + import pipeline
validation        -> engine-managed validation commands
runtime           -> normal Play / AI Agent Bridge
```

Raw filesystem writes and arbitrary shell execution are disabled by default.
When a workflow cannot be completed through managed services, the agent may
request scoped low-level access with a reason and the requested capability.
AI Studio SHOULD distinguish workspace-scoped access from unrestricted process
execution and make elevated access visible in the run audit history.

GameEngine permission policy is an application-level authorization boundary.
When an external agent process runs with the user's normal operating-system
identity, GameEngine MUST NOT claim that application policy alone prevents that
process from accessing every path or capability available to the user. Strong
confinement requires a provider or OS sandbox, container, restricted token, VM,
or equivalent mechanism. Implementations SHOULD use available provider/OS
sandbox features, but the initial architecture does not require GameEngine to
provide a universal OS sandbox.

### 9. Code changes use a session-scoped agent workspace with run checkpoints

Normal AI source-code mutation does not write directly into the live project
working tree. The agent code workspace maintains a session-scoped working copy
or equivalent isolated source state that the coding agent can inspect and edit.
Each run creates logical before/after checkpoints and a reviewable diff.

The workspace persists for the lifetime of the AI session so later requests can
build on earlier code changes. Checkpoints are GameEngine agent history, not Git
commits, and MUST NOT silently modify Git history.

Applying code changes to the live project goes through the code workspace
service, which owns diff review, policy checks, project-root confinement, and
integration with validation. A run MUST retain enough provenance to report
which source files it changed and to support run-level comparison or revert
when technically possible.

Engine-managed validation commands such as formatting, metadata/check, Clippy,
tests, documentation, and other repository-defined validators may be allowed
without granting arbitrary shell execution. Custom scripts or commands outside
the managed allow-list require the corresponding elevated capability.

### 10. External asset acquisition is provider-based and explicitly enabled

Network asset acquisition is optional and disabled unless the user enables the
corresponding AI Studio capability or approves it for the relevant scope.

Asset search and acquisition use a GUI-free provider service rather than
unstructured `curl` or arbitrary filesystem writes. Providers may represent
web asset catalogs, local libraries, generated images, generated audio,
generated 3D content, or future sources. Acquired content enters the existing
import/manifest pipeline and remains subject to normal path and asset-write
rules.

Where a provider exposes source or license metadata, the acquisition service
SHOULD retain reviewable provenance sufficient for the user to understand where
an asset came from. Provider credentials and temporary download credentials are
never project assets.

### 11. Human editing remains available while an agent runs

AI execution MUST NOT globally lock the Editor. The human user may continue to
edit the Scene, graphs, assets, or source while an agent is working.

Structured authoring retains ADR 0121 revision/generation checks. If a human or
another operation changes the authoritative document after the agent inspected
it, a stale apply is rejected. The agent SHOULD re-read the current state and
repair or re-plan rather than force-applying stale commands.

Multiple read-oriented sessions MAY coexist. At most one AI run per Editor
project may hold the agent-host writer role at a time in the initial
implementation. Human editing is not counted as an AI writer and remains
allowed. A future multi-writer agent model requires an explicit conflict and
ownership contract rather than implicit concurrent writes.

### 12. AI sessions support local-private and project-shared persistence

AI sessions survive Editor restarts. By default, session state is private user
application data outside the project working tree, keyed by project identity and
location consistently with ADR 0117.

A user may explicitly mark a session as **project-shared**. Shared sessions are
stored as collaboration metadata under a reserved project-local AI metadata
area, conceptually:

```text
.gameengine/ai/sessions/<session-id>/
```

The shared representation contains portable conversation, proposal, run
summary, and audit information needed to continue the collaboration from
another machine. It MAY be tracked in Git or moved with the project directory.
It is not canonical authoring data and MUST NOT be included in packaged game
content.

Shared records MUST NOT contain:

- API keys, OAuth/session tokens, MCP bearer credentials, or other secrets;
- provider credential-store contents;
- machine-specific absolute paths when a project-relative identity is possible;
- transient process identifiers or ports;
- full agent workspace copies, build outputs, or caches; or
- other data that cannot safely or portably cross machines.

The session-scoped code workspace is local reconstructable state. Project-shared
history records checkpoints and diffs or references needed for history, not a
second checked-in copy of the complete working tree.

AI Studio SHOULD make the local/private versus project-shared state obvious and
allow the user to change it deliberately.

### 13. Completion requires validation and observed playable behavior

An autonomous run MUST NOT report the game or requested feature as completed
solely because source compilation or one authoring mutation succeeded.

When applicable to the proposal, completion requires all of the following:

1. the proposal acceptance criteria are satisfied;
2. blocking authoring validation passes;
3. required source-code validation passes;
4. Play launches successfully;
5. at least one relevant frame is captured;
6. the agent performs visual evaluation of the captured result; and
7. required interaction scenarios are exercised and pass.

A proposal may explicitly mark a criterion as not applicable, but the run must
report that fact rather than presenting an unperformed check as a pass.

Visual evaluation supplements rather than replaces deterministic authoring,
compiler, test, and runtime checks. If the agent cannot capture or inspect the
required result, the run is incomplete or partially validated rather than
completed.

### 14. AI Studio presents semantic progress, changes, and audit history

AI Studio SHOULD present structured work such as inspected documents, proposal
steps, authoring mutations, code diffs, asset acquisitions, validation,
playtest, and repair attempts in a human-readable timeline.

Where the Editor can resolve a referenced entity, asset, graph node, material,
UI document, or other object, the UI SHOULD provide navigation from the agent
activity to the corresponding Editor context.

Each run records an audit summary including at least the applicable counts or
records for authoring operations, code changes, external acquisitions,
low-level filesystem access, custom commands, permission escalations, and
validation/playtest outcomes. Audit history MUST distinguish managed operations
from escape-hatch operations.

Cancellation is a first-class action. Stopping a run prevents new work from
starting, requests provider/process cancellation, and leaves committed
transactional authoring changes and already-applied code changes visible and
reviewable. Cancellation MUST NOT imply an unsafe automatic rollback of
external side effects that the host cannot prove reversible.

### 15. Implementation is capability-sliced, but the contracts are designed together

Implementation SHOULD proceed in vertical slices while preserving the full
architecture:

1. agent core/session/run/event contracts and AI Studio conversation UX;
2. external agent runtime process integration and MCP connection injection;
3. permission broker and managed validation;
4. session-scoped code workspace, checkpoints, diff, and apply;
5. native agent runtime with local-model backend support first where practical;
6. hosted model backends and provider-managed authentication adapters;
7. asset acquisition providers and network permission UX;
8. Play, frame observation, evaluation, and repair loop; and
9. project-shared portable session history and audit tooling.

A partial implementation MUST NOT bypass the architecture by placing unique
editing rules in one provider adapter or the AI Studio UI.

### 16. Local-model setup is catalog-assisted without model-family lock-in

Native local-model support is runtime-oriented rather than model-family-oriented.
GameEngine SHOULD integrate a small number of local runtime or compatible
backend shapes and MUST NOT require one engine adapter per model family.
Compatible models exposed by a supported runtime may therefore be used without
adding model-specific authoring logic to GameEngine.

AI Studio SHOULD provide a curated local-model setup path for users who do not
already manage local models. The recommended catalog SHOULD present a small
number of understandable profiles such as lightweight, balanced, and
high-quality rather than exposing an unfiltered model registry as the default
experience. A catalog entry SHOULD record enough metadata to make the choice
reviewable, including:

- runtime/backend identity;
- exact model and version identity;
- download source and license/provenance information;
- expected download/storage size;
- recommended system memory and GPU memory where known;
- context and modality capabilities;
- structured-output and tool-use capabilities; and
- the GameEngine benchmark version and result that justified recommendation.

Model weights are not bundled merely because a model appears in the catalog.
Downloading a recommended model is an explicit user action, and AI Studio SHOULD
show the source and expected transfer/storage size before acquisition.

Where reliable, AI Studio SHOULD detect already-installed compatible local
runtimes and models and allow the user to select them. Advanced users MUST be
able to configure a compatible custom local backend without requiring that its
model appear in the curated catalog.

The native runtime maintains a provider-independent `ModelCapabilityProfile` or
equivalent description. Capabilities SHOULD include, when discoverable,
structured output, tool use, image input, reasoning support, context limits, and
other features required by the active harness. A model that can connect but has
not passed the relevant GameEngine benchmark may be shown as compatible but
unverified rather than being presented as officially recommended.

Specific model families and model versions are deliberately not pinned by this
ADR. Recommendations are product data derived from current compatibility,
hardware requirements, licensing constraints, and GameEngine-specific measured
results. Updating that catalog MUST NOT require changing the native agent
architecture.

### 17. The native runtime starts from one measurable baseline harness

GameEngine owns one provider-independent baseline native-agent harness. The
initial harness SHOULD use the same orchestration policy across compatible
models so model comparisons do not accidentally compare different agents.
Model-specific behavior MUST NOT fork the semantic authoring, workspace,
permission, validation, or completion architecture defined by this ADR.

The architecture boundaries are stable, while performance-sensitive harness
policy is expected to evolve. Tunable policy MAY include:

- whether planning is required or may be skipped for simple work;
- which tools are exposed in each run state;
- retrieval, context selection, compaction, and pinned working-set policy;
- validation cadence and validator ordering;
- repair budgets and re-plan thresholds;
- model reasoning-mode selection when supported;
- frame-observation and visual-evaluation cadence; and
- stop and completion-decision policy above the deterministic completion gates.

The host SHOULD instrument native runs so these decisions can be measured.
Metrics SHOULD include, when available, task and acceptance-criteria success,
model turns, tool calls, invalid or failed tool calls, context compactions,
token usage, code-edit counts, validation attempts, repair loops, Play and
visual-evaluation attempts, human interventions, elapsed time, and peak memory
or GPU-memory use. Missing backend-specific metrics MUST be represented as
unavailable rather than fabricated.

GameEngine SHOULD maintain a representative **GameEngine Agent Benchmark** for
native-agent evaluation. Candidate models MUST be compared with the same harness
version, tool and permission budget, task corpus, backend configuration, and
completion criteria when the comparison is intended to choose a recommended
model. Results SHOULD retain the exact model version, quantization or equivalent
runtime representation, backend version, and relevant hardware configuration.
Third-party benchmark scores MAY inform candidate selection, but MUST NOT be the
sole basis for the default GameEngine recommendation.

Harness changes SHOULD be evaluated against recorded failures and benchmark
results. When practical, change one policy dimension at a time and retain a
change only when it improves the relevant task set without unacceptable
regressions. If a model demonstrates a repeatable need for a different planning,
context, tool, or reasoning policy, that difference SHOULD be represented by a
small `HarnessPolicy` or model profile rather than by a separate agent
implementation.

### 18. Questions and learning use the same conversation with a read-oriented harness

AI Studio is also the first-class surface for asking questions about GameEngine
and about game development. This does not introduce a mandatory user-visible
Ask/Build mode pipeline. The same conversation MAY move naturally between
questions, design discussion, learning, debugging, and an authorized build run.

The host SHOULD classify or otherwise route the current intent to an appropriate
harness policy. Read-oriented question and learning work normally follows a
retrieve -> reason -> answer flow and MUST NOT acquire mutation permissions
merely because the same session could later create a run. Useful read sources
include:

- accepted ADRs and canonical GameEngine documentation;
- current source code and generated/public API documentation;
- read-only Editor MCP inspection of the open project and diagnostics; and
- general model knowledge for game-development concepts not specific to the
  current GameEngine implementation.

For GameEngine-specific questions, current repository and Editor evidence is
authoritative over stale model memory. The answer flow SHOULD retain source
provenance sufficient for the UI to distinguish repository-derived facts,
current project observations, general model knowledge, and optional external
research. Network-backed research remains subject to the normal network
permission policy.

GameEngine-specific knowledge SHOULD primarily be supplied through retrieval
rather than by baking one repository snapshot into model weights. This keeps
answers aligned with changing ADRs, documentation, APIs, and project state
without requiring a model fine-tune after ordinary engine changes.

If a conversation moves from explanation to mutation, the host creates or
revises the structured proposal and still requires the normal explicit **Go**
snapshot before starting the corresponding write-capable `AgentRun`. Asking a
question MUST NOT silently escalate into project mutation.

### 19. One selected model is the baseline; multi-model routing is an optimization

The initial native experience SHOULD use one user-selected model for
conversation, questions, planning, and build work whenever that model satisfies
the required capabilities. This keeps conversation continuity, evaluation, and
failure diagnosis understandable while the native harness is being established.

A later `ModelRouter` MAY use different models for different workloads, for
example a smaller model for simple questions, a stronger coding model for
implementation, or a vision-capable model for frame evaluation. Such routing is
an optimization rather than a correctness dependency.

Routing policy MUST preserve the active session and proposal semantics, source
provenance, permission boundaries, immutable run input, and completion gates.
The host MUST NOT hide a capability loss caused by switching models. Context
handoff and routing choices SHOULD be measurable, and a multi-model policy
SHOULD be adopted only after GameEngine-specific evaluation demonstrates a
meaningful quality, latency, memory, or power benefit over the single-model
baseline.

## Initial implementation status

The first accepted implementation ships the architectural foundation without
pretending that every provider or autonomous repair capability already exists.
The Editor application layer contains a GUI-free agent host module and a thin
AI Studio frontend. The host owns versioned sessions and proposals, immutable
Go-time proposal snapshots, one active project writer run, structured run
events, approval scopes, restartable local-private persistence, sanitized
project-shared history, a generic external-process runtime boundary with
ephemeral Editor MCP injection, and an isolated code workspace with stale-file
checks before reviewed code is applied. The GUI does not own those rules.

This initial slice intentionally leaves provider-specific Claude/Codex adapters,
the native model/tool loop, hosted model authentication, asset acquisition
providers, automatic managed-validation execution, and the Play/frame/evaluate/
repair loop for the later capability slices listed above. The completion model
already records those gates explicitly, so an unimplemented or unperformed gate
cannot be presented as a pass. Moving the GUI-free host from the Editor
application package into a dedicated workspace crate later does not change the
contract.

## Consequences

- GameEngine gains a first-class conversational AI creation workflow rather
  than a vendor-specific prompt launcher.
- Users can use external subscription/account-backed coding agents without
  requiring GameEngine to possess a model API key, while direct API and local
  model backends remain possible through the native runtime.
- The proposal snapshot gives Go a precise meaning and prevents a running agent
  from silently drifting as the conversation continues.
- Human editing stays responsive while agent work proceeds; stale authoring
  mutations fail safely and are re-read instead of force-applied.
- Managed authoring, code, asset, validation, and runtime services provide a
  consistent audit and permission surface across providers.
- The escape hatch preserves practical coding-agent flexibility, but external
  agents are not falsely described as OS-sandboxed when they are not.
- Session-scoped code workspaces and run checkpoints add storage, diff, and
  reconciliation complexity, but make code changes reviewable and reversible
  without abusing Git commits as internal agent state.
- Project-shared AI history improves multi-PC and team continuity while keeping
  credentials, caches, and transient state out of Git and packaged games.
- Native agent support requires GameEngine to own a real model/tool loop,
  context management, cancellation, and backend abstraction in addition to the
  simpler external-agent process adapters.
- Completion becomes stricter and more meaningful because playable and visual
  behavior is observed rather than inferred from compilation alone.

## Alternatives Considered

### Make AI Studio a thin terminal wrapper around Claude Code or Codex

Rejected. It would expose provider-specific output and capabilities directly,
provide no stable proposal/run/audit model, and make future native or local
agents second-class.

### Treat every integration as an LLM API provider

Rejected. External coding agents and direct model backends own different
responsibilities and authentication lifecycles. An API-key-shaped abstraction
would not represent provider-managed subscription/account sessions or local
models cleanly.

### Use a one-way Ask -> Plan -> Build -> Autopilot mode pipeline

Rejected. The primary workflow is collaborative conversation. The agent must be
able to ask questions before Go and return to the user during execution when a
meaningful decision is required.

### Let agents directly edit all project files and run unrestricted shell commands

Rejected as the default. It bypasses the semantic authoring model, weakens
reviewability, and makes provider behavior inconsistent. Low-level access
remains an explicit escape hatch for operations that managed services cannot
express.

### Prohibit all raw filesystem and shell access

Rejected. Coding agents sometimes require build products, generated files,
custom tools, or diagnostics that are impractical to model immediately through
a typed service. The safer long-term design is managed-by-default with explicit
permission escalation and auditable low-level access.

### Use Git commits as AgentRun checkpoints

Rejected. Internal run lifecycle must not silently rewrite or pollute the user's
source-control history. Git remains a user/team collaboration tool; agent
checkpoints are independent session metadata.

### Store all AI sessions only in user-local application data

Rejected. It prevents deliberate continuation across machines and team
collaboration. Local-private remains the default, with an explicit project-shared
option that is safe to version in Git.

### Store all AI conversation and workspace state in canonical project data

Rejected. Conversation, credentials, provider state, caches, and agent process
metadata are not gameplay authoring semantics and must not affect packaged game
content or canonical project validation.

### Lock the Editor while an AI run is active

Rejected. It would make the AI feature hostile to normal interactive editing.
Existing stale-revision checks already provide the correct semantic conflict
boundary for authoring data.

### Claim application permissions provide a universal sandbox

Rejected. An external process running under the user's OS identity may retain
capabilities outside GameEngine's application policy. Strong confinement must
come from a real provider or operating-system isolation mechanism.

## Compatibility and Migration

This ADR does not change existing persisted authoring document schemas, stable
IDs, MCP tool names, CLI command meanings, or runtime ECS contracts.

If accepted, ADR 0121 remains authoritative for the Editor-scoped MCP writer
and shared authoring transaction semantics. This decision extends the
application layer above that interface with conversational agent orchestration,
provider/runtime abstractions, permission policy, code workspace management,
asset acquisition, and AI session persistence.

Implementing project-shared AI history introduces a reserved collaboration
metadata area under the project tree. That metadata is explicitly not canonical
authoring data and is excluded from packaged game content. Its own session
schema MUST be versioned before cross-machine persistence is shipped.

Implementing the native agent runtime, code workspace, asset acquisition
service, and any new workspace crates adds public internal architecture but does
not authorize runtime crates to depend on Editor, MCP transport, provider SDKs,
or AI session types.

Provider credentials and login sessions remain user-private. Migration from
local-only sessions to project-shared sessions is an explicit user action and
must sanitize non-portable or secret fields before writing the shared record.

If this ADR is accepted, `docs/AI_FRIENDLY_AUTHORING_SPEC.md` sections 16-18
MUST be updated before or with implementation so the canonical specification
reflects the accepted agent-runtime, permission, and collaboration contracts.
