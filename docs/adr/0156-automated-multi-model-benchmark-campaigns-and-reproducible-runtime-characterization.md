# ADR 0156: Automated Multi-Model Benchmark Campaigns and Reproducible Runtime Characterization

Status: Accepted
Date: 2026-08-18
Builds on: ADR 0142, ADR 0155
Relates to: ADR 0135, ADR 0141, ADR 0143, ADR 0150, ADR 0157

## Context

ADR 0142 defines the versioned GameEngine Agent Benchmark, seven representative
task classes, strict comparison identity, machine-local evidence, and a curated
model catalog. The initial UI intentionally requires explicit model selection,
task selection, execution, and `Record current evidence`.

That manual path proves the benchmark contract, but it does not scale to real
model selection. Comparing four models across seven tasks already requires 28
task runs for one repetition. Repeating each task three times requires 84 runs,
before accounting for model acquisition, load/unload, fixture reset, runtime
recovery, or comparison reporting.

Manual repetition also creates avoidable experiment bias. A user can
accidentally change runtime version, quality preference, task identity, hardware
state, model representation, or execution order between candidates. The
resulting records remain individually valid, but a human-driven workflow makes
it too easy to compare evidence that ADR 0142 correctly labels non-equivalent.

ADR 0155 introduces a GameEngine-managed local runtime capable of acquiring,
starting, loading, unloading, and identifying local model representations. That
makes it practical for the benchmark to orchestrate complete campaigns instead
of treating every model run as an isolated manual action.

The benchmark must still measure the GameEngine product, not only tokens per
second. Code implementation, authoring mutation, repair, Play/runtime
interaction, and visual evaluation must continue to exercise the real Agent
Host, governed tools, validation, and completion gates.

A second use case is runtime characterization. For the Windows first release,
the same GGUF may be executed through Windows-native `llama.cpp` or WSL2 Linux.
That comparison is useful for choosing the managed Windows runtime environment,
but ADR 0142 correctly forbids presenting it as a model-only ranking. Native
Linux is not required for this decision because a true native-Linux comparison
would also require a validated Linux-hosted GameEngine product path. The
campaign system therefore needs an explicit distinction between model comparison
and Windows runtime/platform characterization, while reserving future
`NativeLinux` evidence as a separate environment if Linux-hosted GameEngine is
later supported.

A project-wide decision is required for campaign identity, user consent,
fixtures, repetitions, execution ordering, lifecycle automation, automatic
evidence recording, failure classification, resume behavior, aggregation,
runtime characterization, and integration with the curated catalog and ADR 0150
router.

## Decision

### 1. GameEngine introduces an explicit benchmark campaign

A **benchmark campaign** is a machine-local orchestration object that freezes the
experiment plan before measured work starts.

Conceptually:

```text
BenchmarkCampaign
  plan
    models[]
    tasks[]
    repetitions
    comparison class
    runtime/execution profile
    quality/resource policy
    fixture identity
  schedule[]
  run records[]
  aggregate report
```

Starting a campaign is an explicit user action. That action authorizes the
benchmark runner to execute the selected task plan and automatically record the
resulting sanitized evidence. It does not authorize silent future benchmarks,
background uploads, unrelated model downloads, or use of private projects.

The existing manual `Record current evidence` path remains available for focused
development and diagnostics.

### 2. Campaign plans are versioned and immutable after measured execution starts

A campaign plan records enough information to reproduce or reject comparison.

At minimum it contains:

- campaign schema/version;
- ADR 0142 corpus and benchmark harness versions;
- selected task identities;
- ordered model candidates and exact representation identities when known;
- comparison class;
- requested repetition count;
- GameEngine quality/workload policy;
- selected local/hosted backend posture;
- managed runtime preference or frozen execution environment;
- benchmark fixture/project identity;
- tool/permission/work-claim budget policy;
- inference/sampling profile where provider-independent control exists;
- deterministic seed policy where supported; and
- hardware identity captured before measured execution.

Once the first measured task begins, the plan is immutable. Changing a model,
task set, runtime environment, repetition count, quality policy, or fixture
creates a new campaign or explicit derived campaign rather than rewriting the
identity of existing results.

### 3. The seven ADR 0142 task classes remain authoritative

This ADR automates ADR 0142; it does not weaken its corpus.

The first campaign runner covers the versioned task classes:

```text
Read question
Project inspection
Code implementation
Typed authoring mutation
Validation and repair
Runtime interaction
Visual evaluation
```

Each task runner maps one frozen task descriptor to the real production harness:

- read questions use the native read/provenance harness;
- write-capable tasks use the native Agent Runtime and Agent Host;
- code tasks use the governed code workspace and managed validation;
- authoring tasks use the authoritative Editor/MCP boundary;
- runtime tasks use the production AI Runtime Debugging / Automated Playtest
  surface defined by ADR 0157, including normal Play, deterministic managed
  input, pause/step, observation, and host-owned assertions;
- visual tasks use the same ADR 0157 observation path plus host-owned frame
  capture and visual completion evidence when the selected ModelBackend supports
  image input.

The benchmark MUST NOT introduce a benchmark-only privileged runtime-control
path. A model qualifies by using the same governed debugging surface available
to normal AI Studio work.

A campaign may run a subset for diagnostics, but a full catalog-qualification
campaign requires every task required by the current ADR 0142 recommendation
policy.

### 4. Benchmarks use controlled repository-owned fixtures by default

Automated campaigns MUST NOT silently benchmark against the user's active
project.

The normal runner uses repository-owned or versioned benchmark fixture projects
whose initial state, expected task contract, and reset procedure are known.

Before every measured repetition the runner restores the required fixture state
so one model's mutations do not become another model's input.

Fixture reset MUST cover all state that can affect task semantics, including
when applicable:

- canonical project authoring data;
- managed code-workspace source state;
- generated benchmark-local outputs;
- AI session/run state;
- managed Play state; and
- benchmark-specific caches that are defined as cold for the selected profile.

Global runtime/model caches are reset only when the campaign profile explicitly
measures a cold boundary.

Private projects may be benchmarked only through an explicit separate user
action and remain ineligible for silent upload or public catalog evidence unless
the user deliberately contributes sanitized results through a future policy.

### 4a. Candidate-visible task contracts are separated from host-only evaluation

A benchmark is invalid if the candidate model can retrieve the answer key or the
mechanics used to award success. The benchmark therefore separates what the
Agent is legitimately asked to solve from what only the host may know.

The candidate-visible side MAY contain:

- the task prompt;
- user-visible acceptance criteria that a real user would reasonably provide;
- the project/source/assets required to perform the task;
- the normal production tool descriptions and permission boundaries; and
- runtime observations that ADR 0157 would expose during ordinary debugging.

The host-only evaluation side contains, as applicable:

- pristine fixture/reset metadata not needed by the candidate;
- hidden validation cases and expected state;
- golden outputs or reference invariants;
- scoring/qualification thresholds;
- hidden visual or runtime assertions; and
- variant-generation state that would reveal the specific held-out answer.

Host-only evaluator data MUST NOT be placed inside the candidate's code
workspace, project tree, retrieval corpus, MCP-visible files, tool results,
conversation context, or other model-readable surface. If a benchmark task must
let the Agent inspect GameEngine source, that access MUST expose only the
candidate-visible source needed for the task or otherwise keep the evaluator in
a separate host-owned boundary. Merely naming a file `hidden` inside a readable
repository is not isolation.

The host, not the model, decides hidden completion. A model may report progress
or visible acceptance criteria, but it cannot self-award a hidden benchmark
gate.

### 4b. Parameterized variants and holdouts reduce benchmark contamination

A permanently fixed public problem can eventually become part of model training
data or be solved through benchmark-specific memorization. The corpus SHOULD
therefore define task families with multiple equivalent fixtures or
parameterized instances in addition to stable public smoke cases.

For a measured campaign, the host freezes the exact instantiated task identity
and any reproducibility seed before execution. The candidate receives the
instantiated user-facing task, not hidden generator state or the evaluation
oracle. Results record enough instance identity to reproduce or explicitly reject
a comparison without requiring the answer key to become model-visible.

Open-source generator logic alone is not treated as secret. Holdout value comes
from candidate-inaccessible instantiated data, hidden assertions, and variation
that prevents a model from succeeding merely by recalling one published fixture.

### 5. Campaign execution automates acquisition, model switching, and runtime lifecycle

Before measured execution, the campaign preflight resolves every selected
candidate to an exact representation. A candidate may already be installed, may
refer to an existing compatible GGUF, or may be introduced from a supported
source repository / exact model-file URL. GameEngine SHOULD discover available
metadata and GGUF representations from the source where possible, but the exact
file and quantization used for comparison remain explicit campaign identity.

If selected candidates are missing, the preflight presents one review containing
per-candidate and aggregate transfer/storage requirements plus known
license/provenance. One explicit **Download & Run** approval authorizes acquisition
of exactly those frozen missing candidates. The campaign MUST NOT use that
approval to download unrelated candidates later. Interrupted downloads may use
the bounded pre-measurement retry policy from this ADR.

For a managed local campaign the orchestrator may then automatically:

1. acquire and content-verify the approved missing model representations;
2. verify all selected model identities and the frozen runtime/backend identity;
3. start the selected managed runtime;
4. load the required model;
5. confirm backend health and applicable telemetry;
6. restore the selected candidate-visible fixture instance;
7. run the frozen benchmark task through the production Agent/ADR 0157 surfaces;
8. evaluate host-only gates without exposing their oracle to the candidate;
9. record sanitized evidence;
10. reset the benchmark fixture;
11. unload or switch the model at the defined boundary; and
12. continue to the next scheduled run.

The user does not manually download each GGUF in a browser, create an Ollama
`Modelfile`, select `Evidence task`, press Go, press `Record current evidence`,
switch models, or relaunch the managed server for every campaign item.

External compatible runtimes MAY participate when their identity and lifecycle
can be measured truthfully, but the orchestrator MUST NOT fake controls that the
backend does not expose.

### 6. Only one measured local inference run executes at a time by default

Model candidates compete for the same hardware, so concurrent benchmark
execution would create uncontrolled resource contention.

The default campaign scheduler is sequential for measured local model work.
Other Editor/Agent activity that would materially consume the same resource
budget is suspended, rejected, or marks the run non-comparable according to the
campaign policy.

The scheduler records exact execution order.

To reduce time-order and thermal bias, the first-release full-comparison
scheduler SHOULD interleave candidates by task and repetition rather than run
every repetition of model A before model B. The exact schedule is deterministic
for the frozen campaign and is included in campaign evidence.

A future scheduler may adopt another measured ordering policy, but changing that
policy changes campaign identity.

### 7. Repetition is first-class; no single lucky run becomes a ranking

The UI exposes a measured repetition count.

The first-release default is three measured repetitions per selected task and
model. One repetition remains available for smoke testing and development, but
the UI MUST distinguish it from a repeated comparison campaign.

The benchmark store retains each underlying ADR 0142 run record. Aggregation
never replaces raw run evidence.

If a provider does not support deterministic seeding, the campaign records that
fact and uses repetitions to expose variability instead of pretending the run is
deterministic.

### 8. Cold-start and steady-state behavior are separate metrics

Model-load cost and task-execution cost answer different questions.

A campaign execution profile defines whether a measured run is:

- **cold**: model residency/runtime caches are released to the defined safe
  boundary before measurement; or
- **warm**: the model is already healthy/resident according to the backend
  contract.

Load/reload latency is recorded separately from task elapsed time when the
backend exposes enough evidence. The report MUST NOT silently add or remove
startup cost when comparing candidates.

Product-level recommendations may use both cold and warm evidence according to a
versioned recommendation policy.

### 9. Comparison class is explicit

Campaigns classify what dimension is being compared.

#### Model comparison

A model-comparison campaign freezes the execution environment and all ADR 0142
equivalence dimensions that must remain the same. Candidate model identity and
representation are the intended changed dimension.

For managed local inference this means the campaign MUST NOT let `Auto` choose
Windows for one model and WSL2 for another while calling the result a model-only
comparison. The runtime environment is resolved once before measured execution
and frozen for the campaign.

#### Runtime/platform characterization

A first-release Windows runtime-characterization campaign intentionally holds
the model representation constant while changing only the Windows execution
environment:

```text
same GGUF
  WindowsNative
  Wsl2Linux
```

This is an engineering/product-selection campaign for ADR 0155, not a normal
step every user must run before benchmarking models. It SHOULD be short and use
representative GPU-resident and, when relevant, CPU/GPU-hybrid workloads.

These results are labelled runtime/platform evidence and are never inserted into
a model-only ranking as if the environment matched. `NativeLinux` may be added
later only when a Linux-hosted GameEngine exists as a validated product path; it
is not required to choose the Windows first-release runtime.

#### End-to-end product profile

A future end-to-end profile MAY compare complete user-facing policies where each
candidate is allowed its own measured optimal resource plan. Such results must be
labelled product-profile evidence rather than strict model-only evidence.

### 10. Automatic recording is allowed only inside an explicitly started campaign

ADR 0142 requires recording to be explicit rather than silent.

Pressing **Run Benchmark** or an equivalent campaign-start action is the explicit
recording decision for all runs described by the frozen campaign plan.

After each task, the orchestrator automatically writes an ADR 0142 record only
when it can prove that:

- the executing model/runtime matches the frozen candidate;
- the task identity matches the frozen descriptor;
- the benchmark and runtime harness versions match;
- the fixture identity is current;
- the hardware/execution profile has not changed incompatibly; and
- the completion/failure evidence belongs to that exact run.

If any identity cannot be established, the result is rejected from comparable
evidence rather than guessed.

No campaign action uploads prompts, conversation history, retrieved source text,
credentials, private project paths, or raw work-claim target keys.

### 11. Model, backend, and task failures are evidence, not retry noise

The orchestrator must not retry a failed model turn until it happens to pass.

Once a measured task starts, failures such as:

- OOM;
- model/backend crash;
- invalid tool behavior;
- failed validation;
- exhausted repair budget;
- failed Play interaction;
- failed visual evaluation; or
- task timeout

remain benchmark evidence.

A bounded automatic retry is allowed only for a clearly classified
infrastructure failure that occurred before the measured task started, such as
an interrupted download or a managed server that failed its startup health
probe.

If the failure cannot be confidently classified as pre-measurement
infrastructure, it is not silently discarded.

### 12. Campaigns are pausable and resumable without rewriting prior evidence

A long campaign may be stopped or paused.

The orchestrator persists machine-local progress containing the frozen plan,
completed schedule entries, recorded run identities, and the next pending
entry.

On resume it revalidates:

- current hardware identity;
- benchmark corpus/harness versions;
- fixture version;
- runtime/backend version;
- selected model representations; and
- execution environment.

If an identity dimension changed incompatibly, GameEngine preserves the old
evidence and requires a new or derived campaign for the remaining work. It does
not rewrite earlier records to match the new environment.

### 13. Campaign reporting emphasizes task success before raw throughput

The aggregate report presents each candidate with at least:

- completed/attempted runs;
- task success rate;
- completion-gate failures;
- human interventions;
- median elapsed time for successful comparable runs;
- generation throughput when available;
- load/reload latency when available;
- peak backend/model GPU residency when available;
- OOM count;
- validation/repair behavior; and
- per-task breakdown.

Raw tokens per second is not the primary ranking metric.

The report MUST identify unavailable telemetry instead of treating it as zero.

A single scalar score MAY be added only behind a versioned scoring policy with
documented weighting. The first release does not require one.

### 14. Curated catalog and model routing consume only qualified campaign evidence

Campaign automation does not lower ADR 0142 qualification requirements.

The Curated Model Catalog may consume campaign records only when the current
recommendation policy finds complete comparable evidence.

ADR 0150 routing may use campaign-produced records exactly as it uses manual ADR
0142 records. Runtime-characterization results do not qualify a routing
specialist unless the relevant model-comparison evidence independently satisfies
ADR 0150.

A campaign report may recommend that more evidence is needed; it MUST NOT invent
a Lightweight, Balanced, or High Quality winner from an incomplete corpus.

### 15. The first-release UI is a campaign launcher and progress matrix

AI Studio SHOULD provide a benchmark surface that allows the user to:

- select multiple discovered or managed models;
- add a new candidate from a supported source repository / exact model-file URL;
- inspect discovered GGUF files and explicitly choose the representation /
  quantization used by the campaign;
- inspect exact model/quantization/runtime identity;
- select the runtime/agent harness before freeze, with Goose ACP Agent Harness
  recommended for Managed Local and Legacy Native available only by explicit
  compatibility/comparison selection;
- inspect the frozen lane, harness, agent/runtime, model, and execution
  environment after freeze;
- select full corpus or a task subset;
- choose repetition count;
- choose cold/warm execution profile where supported;
- choose model-comparison or runtime-characterization mode where that mode is
  relevant to the current product state;
- review missing model transfer/storage and license/provenance per candidate and
  in aggregate;
- explicitly approve the exact missing set through **Download & Run**;
- start the campaign automatically after approved acquisition succeeds;
- observe current model/task/repetition and aggregate progress;
- pause/stop and later resume; and
- inspect the final per-task comparison report.

The normal full-model-comparison path should therefore approach:

```text
select models
  -> Download & Run   # only when selected models are missing
  -> review results
```

rather than a manual download/import/task/record sequence for every model.

Months later, a newly released model SHOULD be addable without rebuilding the
benchmark system or re-running every historical candidate. If prior evidence
for the current baseline model is still strictly comparable, the new candidate
may be measured against that existing baseline. If benchmark corpus, harness,
runtime, execution environment, hardware, or another equivalence dimension
changed incompatibly, the UI SHOULD offer to re-run the current baseline plus
the new candidate. It MUST NOT imply strict comparability to stale evidence or
require all historical models to be downloaded and measured again.

### 16. Benchmark campaigns remain machine-local product data

Campaign plans, progress, and aggregate reports are machine-local application
data unless a future explicit export/contribution flow says otherwise.

They MUST NOT enter canonical project authoring files or packaged game content.

A sanitized report MAY be exportable for human review. Exported data must follow
ADR 0142 redaction and provenance rules.

## Implementation

The campaign implementation uses schema version 3 and the
`task-repetition-candidate-interleave-v2` schedule identity. The concrete order
is task, repetition, then candidate, so the recorded identity and actual
thermal/time-order policy agree. Schema version 2 froze the explicit quality
policy, hardware identity, and finite per-run timeout. Schema version 3 adds an
optional frozen ADR 0142 benchmark runtime/lane identity to the campaign plan and
plan digest. Existing schema-v2 campaign checkpoints deserialize with that field
absent and retain their legacy harness meaning; they are not rewritten into an
ACP lane. A schema-v2 checkpoint remains resumable only while all of its original
contract dimensions still match and no schema-v3 runtime identity has been
attached.

The campaign launcher exposes **Runtime / Harness** before freeze. Managed
Local selects **Goose ACP Agent Harness** as the normal/recommended choice when
the user moves into a managed execution environment; **Legacy Native Harness**
remains an explicit compatibility/comparison choice. Compatible-backend
campaigns retain Legacy Native. The selector is identity-bearing, not a display
preference: a Goose choice discovers the actual local Goose ACP runtime and
places the provider-neutral `BenchmarkRuntimeIdentity` directly in
`CampaignPolicy`, so `CampaignPolicy::freeze` atomically derives the runtime-aware
per-task authority and immutable `CampaignPlan`.

That identity is stamped into the per-task experiment execution identity and
therefore into every `BenchmarkChildRunSpec`. A Managed Local child with the
frozen ACP identity must select the Goose ACP route; a child without it is the
explicit Legacy Native lane. Failure to discover or negotiate the frozen ACP
runtime, or mismatch between the negotiated and frozen identities, is a failed
run with no ACP `BenchmarkRecord`; it never falls back to Native. Evidence whose
top-level benchmark runtime differs from the frozen plan is likewise rejected as
an identity mismatch.

The existing seven-task ADR 0156 campaign cannot be relabelled `raw_model`,
because every task executes through an Agent Host or production task harness.
Raw Model records must come from a model-only harness. The common ACP Agent
Harness currently cannot provide honest evidence for `read_question_v1` or
`visual_evaluation_v1`; the Goose selector disables/removes them before freeze
and runtime-aware freeze rejects them again. `validation_repair_v1` remains on
the governed failure -> ACP repair -> revalidation path. Agent-inclusive
campaign evidence is never treated as model-only routing/catalog evidence.

The same runtime-aware task policy derives both the immutable
`AgentProposal.requested_capabilities` and the headless permission budget. The
budget therefore cannot auto-approve a capability the proposal did not declare,
and ACP does not weaken Agent Host's existing authority boundary.

The headless coordinator records a timed-out child as one `Timeout` run failure
and continues the remaining schedule. It does not clear the queue. The campaign
state machine does not perform a UI-only pre-measurement retry; a future retry
may be added only together with a coordinator protocol that reruns the same
ordinal. Campaign Pause asks the headless parent to terminate the active child,
persists the completed prefix, and Resume validates and restores that prefix
before rerunning the interrupted ordinal. Machine-local checkpoints preserve
the frozen plan and progress across Editor restarts.

Warm and cold profiles are established through verified backend reload/release
operations before measurement. The deterministic sampling identity is connected
to temperature-zero, seed-zero requests for the supported local adapters.

The campaign orchestrator belongs above the benchmark record primitives and
Agent Host, in application-layer code. It coordinates existing services rather
than duplicating task semantics in the UI. Runtime-interaction and visual task
execution MUST call the production ADR 0157 debugging/observation API rather
than a benchmark-only control seam.

The ACP benchmark migration keeps the current seven task descriptors and uses
ADR 0142 benchmark record schema v4 to add optional runtime/lane identity.
Existing schema v1-v3 records remain readable as legacy-harness records without
that identity, so the migration does not reinterpret previously collected
evidence. Campaign-plan/progress storage, instantiated fixture identity, and
execution-profile identity remain separate frozen dimensions.

Candidate-visible fixture material and host-only evaluator material MUST be
separate application-layer resources with separate access policy. The Agent's
retrieval roots, project/code workspace, MCP surface, tool results, and prompt
context must be constructed so they cannot traverse into the host-only oracle.
Parameterized or holdout instance generation occurs before measured execution,
and the frozen instance identity is recorded without exposing hidden scoring
state to the model.

ADR 0155 managed-runtime lifecycle is the preferred local execution path, but
campaign orchestration remains backend-independent enough to use the existing
Ollama-compatible adapter or future hosted/enterprise backends when their
comparison identity is valid. Managed acquisition and campaign orchestration are
separate responsibilities: ADR 0155 owns verified model/runtime bytes, while
this ADR owns the exact candidate set and the explicit **Download & Run**
authorization for a campaign.

## Verification

Tests and benchmark fixtures must prove:

- campaign plans freeze before the first measured run;
- changing model/runtime/task/repetition policy creates new campaign identity;
- every one of the seven task descriptors maps to the intended production
  harness, with runtime/visual tasks using ADR 0157 rather than a privileged
  benchmark-only control path;
- fixture reset prevents cross-model source/authoring/session contamination;
- candidate retrieval, code workspace, project files, MCP tools, prompts, and
  tool results cannot expose the host-only evaluator/oracle;
- visible acceptance criteria remain usable while hidden tests, golden state,
  scoring thresholds, and hidden assertions remain host-owned;
- parameterized/holdout instances freeze reproducible identity without exposing
  their hidden answer state to the candidate;
- **Download & Run** authorizes only the exact missing candidate set shown in
  campaign preflight and content verification precedes measured execution;
- measured local runs are sequential by default;
- deterministic schedule order is stable for the same campaign plan;
- repetition records are preserved individually;
- cold and warm profiles do not silently mix startup cost;
- model-comparison campaigns freeze one execution environment;
- Windows-native/WSL2 comparisons are labelled runtime characterization and do
  not require native Linux for the Windows first release;
- campaign start is the explicit recording action;
- identity mismatch rejects automatic evidence recording;
- measured failures are retained rather than retried until success;
- only classified pre-measurement infrastructure failures receive bounded retry;
- pause/resume preserves prior evidence and rejects incompatible environment
  drift;
- a newly added model can reuse strictly comparable baseline evidence or prompts
  a baseline re-run when equivalence dimensions changed, without forcing every
  historical model to run again;
- aggregate reports treat unavailable telemetry truthfully;
- incomplete evidence produces no false curated-catalog recommendation; and
- ADR 0150 consumes only records that still satisfy its comparison policy.

Campaign selection, download review, progress, pause/resume, and result-report UI
require Editor Visual Validation.

## Non-goals

This ADR does not:

- replace the ADR 0142 benchmark corpus with synthetic tokens-per-second tests;
- silently benchmark private user projects;
- expose hidden tests, golden state, scoring thresholds, or evaluation-oracle
  internals to the candidate model;
- rely on one permanently fixed public fixture as the only evidence of a model's
  capability;
- run competing local models concurrently by default;
- hide failed runs through unlimited retries;
- declare three repetitions statistically sufficient for every future decision;
- make runtime/platform comparisons equivalent to model-only comparisons;
- require native-Linux GameEngine support for the Windows first-release runtime
  decision;
- require every historical model to be re-downloaded and re-run whenever one new
  model is added;
- automatically publish benchmark data;
- silently download model weights outside the exact explicitly approved campaign
  candidate set; or
- define one permanent model winner or one permanent local runtime platform.
