# ADR 0159: Benchmark Model Exchange Observability and Failure Diagnosis

Status: Accepted
Date: 2026-08-20
Builds on: ADR 0142, ADR 0156
Relates to: ADR 0141, ADR 0155

## Context

ADR 0142 defines seven benchmark task classes and ADR 0156 automates them into
campaigns. Both assume that a recorded run is evidence. A campaign executed on
2026-08-20 shows that this holds for passing runs and fails for failing ones.

Across three models on one host, every task class that requires tool use failed
and only `read_question_v1`, the one class that completes without a tool call,
passed. The failures are reproducible and task-dependent rather than random:
`read_question_v1` passed three times out of three on two different models,
while `project_inspection_v1` failed three times out of three on the same
models. That pattern is exactly what the benchmark exists to surface.

The recorded evidence shows that those failures are not one failure. On
`project_inspection_v1`, one model issued three tool calls of which one was
invalid, made no code edit, and failed the completion gate. Another model, on
the same task and fixture, issued no tool call at all across three repetitions,
and failed the same gate. Tool dispatch works for the first model and never
starts for the second, so at least two distinct causes are being reported under
one outcome.

The record cannot separate them beyond that count. `agent_run_record` builds the
metrics for write-capable tasks from `AgentRun`, and `AgentRun` carries no record
of a model exchange. `AgentEventEvidence` has variants for progress, tool
actions, playtests, captured frames, and completion gates, and none for a model
turn. The record therefore reports `model_turns`, `prompt_tokens`,
`response_tokens`, `load_latency_ms`, and `ttft_ms` as `Unavailable`. That is an
honest statement of what was captured, not a defect in the reporting path, and
`harness_message` is likewise empty because no harness-level error occurred.

So a run that called tools and a run that never answered are both recorded as a
failed completion gate with a tool-call count, and neither carries the model
output that would say why the gate was not satisfied or why no call was ever
issued.

The practical consequence is that failure diagnosis proceeds by guessing. Two
candidate causes were implemented and measured on the same fixture: applying the
model's own chat template to the managed server, and correcting the capability
profile that the runtime interpolates into the agent prompt. Both were refuted
by a controlled single-variable rerun, and both cost a build, a run, and an
analysis cycle. Neither experiment was informative, because neither could
observe what the model actually returned. A benchmark whose failing runs are
unfalsifiable measures completion rate and nothing else.

The gap is specific. The campaign already knows the task, the fixture, the model
representation, the runtime version, the wall clock, and how many tool calls were
dispatched. What it does not know is whether a failing model produced no output,
produced output the agent runtime could not parse, produced a well-formed refusal,
or exhausted its turn budget mid-task. Those four outcomes are indistinguishable
in the current record and require different fixes. The tool-call count narrows
the question without answering it: a count of zero does not say whether the model
was silent or answered in a shape the runtime discarded, and a non-zero count
does not say why the work the model did failed to satisfy the gate.

Recording this requires a decision rather than an implementation. `AgentRun`,
`AgentEvent`, and `AgentEventEvidence` are serialized and persisted per session,
and `BenchmarkRecord` is versioned at `schema_version: 2` precisely so that
comparison identity stays checkable across builds. Adding observability touches
both formats, so it falls under the project rule that serialized formats and
cross-crate contracts are not changed silently.

A second constraint is size. The diagnostic value lives in the model's actual
output, but a benchmark record is a comparison object that is read, aggregated,
and diffed. Embedding full transcripts in it would make records unbounded in
size and would put free-form model text inside the identity surface that ADR
0142 keeps strict.

## Decision

### 1. A model exchange becomes recorded evidence

`AgentEventEvidence` gains a `ModelExchange` variant. The agent runtime records
one per request/response pair against the model, for successful and failing runs
alike, and records it before the run reaches a terminal state so that a run that
fails mid-turn still carries the turns it completed.

The variant carries the turn ordinal, the prompt and response token counts as
reported by the backend, the finish reason, a digest of the full response, and a
bounded excerpt of the response. It does not carry the full response.

### 2. The full transcript is a per-run artifact, not part of the record

The complete request and response text is written to a per-run artifact beside
the existing fixture run directory. The benchmark record references it by the
same digest carried in the evidence.

This keeps `BenchmarkRecord` bounded and comparable while making the raw
evidence available on the machine that produced it. It also keeps free-form
model text out of the comparison identity surface.

### 3. Failing runs report the metrics they can measure

The legacy native `agent_run_record` derives `model_turns`, `prompt_tokens`, and
`response_tokens` from the recorded exchanges instead of reporting them as
`Unavailable`. A native run with zero recorded exchanges reports zero turns,
which is itself a diagnosis, and is distinct from a run that never recorded
whether it had any.

ACP-backed runtimes are a separate observability boundary. When an ACP adapter
cannot observe the coding agent's internal model exchanges, schema-v4 benchmark
recording MUST leave model turns and token counts `Unavailable`; GameEngine does
not infer them from ACP prompts, session events, or normalized Agent Host events.
An ACP adapter may report those metrics as measured only when the agent/runtime
itself exposes authoritative telemetry for them. The distinction between
"measured as zero" and "not measured" is therefore preserved across both the
legacy native and ACP paths.

Standard ACP session usage is a separate diagnostic surface. An agent-reported
current context usage and effective context limit may be recorded as
`ProviderOutput`/benchmark live activity without populating
`BenchmarkModelTelemetry.prompt_tokens` or `response_tokens`: session context
occupancy is not the same measurement as one model request/response exchange.
Likewise, configured Managed Local context budgets, metadata-only tool-result byte
sizes, and provider-reported context-overflow limits improve diagnosis but do not
change comparison identity or manufacture model-turn telemetry. Unavailable ACP
token breakdowns remain unavailable rather than estimated.

### 4. Both formats are versioned explicitly

`BenchmarkRecord` moves to `schema_version: 3`. Records at version 2 remain
readable and are treated as lacking exchange evidence rather than as invalid.
Comparison treats a version 2 record and a version 3 record as equivalent for
every metric that existed at version 2, so campaign history collected before
this ADR stays usable.

Readers of persisted `AgentRun` sessions must tolerate an unknown evidence
variant rather than failing the whole session, so that a session written by a
newer build stays loadable by an older one.

### 5. Failure classification uses the new evidence

The existing `completion_gate` failure kind is retained, and the campaign
distinguishes, within it, a run that produced no model output from a run that
produced output the runtime rejected. That distinction is derived from recorded
exchanges rather than introducing a new failure kind, so the classification
surface that ADR 0156 defines stays stable.

## Consequences

Failing benchmark runs become diagnosable from their own record. The four
outcomes that are currently indistinguishable become distinguishable, and a
hypothesis about a failing task class can be tested against recorded evidence
before a change is built.

The recorded evidence for a run grows. The record itself stays bounded because
it carries counts, a digest, and an excerpt, but each run now also writes a
transcript artifact, and campaign storage grows with the number of runs and the
length of model responses. Retention of those artifacts is machine-local and is
not part of comparison identity.

Two serialized formats change. Sessions written by a build that implements this
ADR contain an evidence variant that older builds do not know, which is why
tolerant reading is required rather than optional. Benchmark records gain a
schema version, and any tooling that pins version 2 needs updating.

Model output is persisted to disk in full. That is the point of the decision,
but it means benchmark artifacts may contain whatever the model produced,
including text derived from the fixture project. The artifacts stay machine-local
and are not published with comparison results.

The benchmark keeps measuring the product rather than the transport. Recording
exchanges does not change what the agent does, which tools it may call, or how
completion is gated.

## Alternatives Considered

**Leave the record as is and diagnose by rerunning with instrumentation.** This
is the current state. It was tried twice on 2026-08-20 and refuted two
hypotheses at the cost of a build and a run each, without ever observing model
output. It does not scale to seven task classes and it makes every diagnosis
depend on a guess being right before the evidence is collected.

**Embed the full transcript in `BenchmarkRecord`.** This is simpler and needs no
side artifact, but it makes records unbounded, puts free-form model text inside
the comparison identity surface ADR 0142 keeps strict, and makes aggregation and
diffing of campaign results impractical.

**Reuse the existing `Progress { step, detail }` variant and encode turn data in
the detail string.** This avoids changing the serialized format and would let
`model_turns` be derived immediately. It was rejected because it encodes
structured data in a human-readable field, gives no token counts, gives no
access to the model output that the diagnosis actually needs, and creates a
parsing contract that is less stable than the schema change it avoids.

**Record exchanges only for failing runs.** This bounds storage growth, but the
comparison between a passing and a failing run is often what identifies the
cause, and a run's outcome is not known until it ends. Recording only failures
would also make the evidence surface depend on the outcome, which weakens the
comparison identity ADR 0142 defines.
