# ADR 0150: Multi-Model Routing and Workload Specialization

Status: Accepted
Date: 2026-08-17
Builds on: ADR 0131, ADR 0141, ADR 0142
Relates to: ADR 0135, ADR 0143, ADR 0144

## Context

ADR 0131 intentionally chooses one user-selected model as the first native baseline. That makes context continuity, failure diagnosis, resource behavior, and benchmark comparisons understandable. It permits a later `ModelRouter` only when GameEngine-specific measurements demonstrate a meaningful benefit.

Potential specialization includes a smaller model for simple questions, a strong reasoning/coding model for implementation and repair, and a vision-capable model for frame evaluation. Routing too early would hide whether failures come from the model, context handoff, tool policy, or orchestration.

## Decision

GameEngine may introduce a provider-independent `ModelRouter` after the ADR 0141 single-model native Agent is complete and ADR 0142 provides a reproducible baseline. Routing is an optimization layer inside the native Agent Runtime; it does not create several AgentRuns or several proposal authorities.

A routing decision may consider:

- workload class and run phase;
- required structured-output/tool/image/reasoning capabilities;
- measured task quality;
- latency and power targets;
- context size;
- local resource pressure and ADR 0143 capabilities;
- user quality preference; and
- provider/network availability.

## Context handoff

All models operate over the same provider-independent session/run state. Handoff uses `AgentWorkingState`, selected source provenance, proposal snapshot, relevant tool results, validation/runtime evidence, and explicit unresolved problems. One model's private transient KV cache or hidden conversation state cannot be the only copy of information required by the next model.

Routing MUST preserve immutable run objectives, permissions, source provenance, stale-state checks, audit history, and completion gates.

## Capability loss

The router may select only a backend/model whose declared capabilities satisfy the requested turn. If image evaluation requires image input, switching to a text-only model must produce an explicit unavailable/fallback decision rather than silently marking evaluation successful.

## Measurement and adoption

Every routing policy has a version and is benchmarked against the ADR 0142 single-model baseline. A policy is adopted only when it improves a defined combination of task success, latency, memory, power, or cost without unacceptable regressions. Routing decisions and model identities are recorded in run/benchmark audit data with secrets removed.

## Failure and fallback

Provider/model failure may fall back to another compatible model only when the fallback preserves the same run semantics and user policy. A fallback that changes remote-processing posture, cost class, or required permission must request the appropriate user decision instead of silently switching.

## Implementation

The first-release `ModelRouter` is an optimization inside the existing `NativeAgentRuntime`; it never creates a second `AgentRun`, proposal authority, permission broker, or completion owner. The user's selected backend/model remains the single-model baseline. AI Studio considers only other models already discovered on the same local backend as automatic routing candidates. Hosted or enterprise processing is never introduced automatically from a local baseline, so a change of remote-processing posture, permission requirement, or cost class remains an explicit user decision.

Routing policy `adr0150-measured-routing-v1` is derived from machine-local ADR 0142 benchmark records. A specialist is eligible only when its task record is comparable with the baseline through the ADR 0142 equivalence contract, the specialist completes successfully, it introduces no measured OOM regression, and it either converts a measured baseline failure into success or preserves success while improving measured elapsed time by at least five percent. Missing or non-equivalent evidence retains the selected single-model baseline rather than inventing a recommendation.

Each routed turn rebuilds the provider prompt from the same immutable proposal snapshot, `AgentWorkingState`, completion state, prior managed tool results, explicit phase context, and current Agent Host evidence. Switching the inference backend therefore preserves provider-independent run state instead of depending on one model's transient KV cache or hidden conversation state. The Agent Host continues to own stale-state checks, permissions, managed side effects, validation, Play/frame evidence, repair, and completion truth.

Image-bearing turns require a backend that declares image input or has successful GameEngine visual-evaluation benchmark evidence. If neither the qualified specialist nor the baseline satisfies that requirement, routing fails explicitly instead of allowing a text-only model to fabricate visual success. A specialist that fails before a turn starts may deterministically fall back only to a compatible selected baseline without changing the user's processing posture.

Every decision records the routing policy version, workload class, backend/model identity, handoff/fallback state, and sanitized reason in the existing run event audit. A native run that actually hands work to a specialist is excluded from ADR 0142 single-model evidence so specialist work cannot be misattributed to the selected baseline model; comparable single-model records remain the source evidence used to qualify routing. AI Studio exposes the active measured-routing policy and count of benchmark-qualified specialist workloads next to model/resource status. No prompt, credential, private provider state, or transient model cache is persisted by the router.

## Dependencies and parallel work

Implementation is sequenced after ADR 0141 and ADR 0142. ADR 0143 and ADR 0144 are optional inputs; if they are not yet on `main`, the router uses only available resource/provider signals.

## Verification

Tests and benchmarks must prove stable session/run identity across model switches, explicit context handoff, capability-safe routing, preserved permission/completion semantics, measured improvement over baseline, deterministic fallback policy where possible, and no hidden credential/context leakage between providers.

Routing/model-status UI requires Editor Visual Validation.

## Non-goals

This ADR does not make multiple models a correctness requirement, allow weak models to rewrite architecture-sensitive decisions without evidence, or route to an unverified model merely because it is cheaper/faster.
