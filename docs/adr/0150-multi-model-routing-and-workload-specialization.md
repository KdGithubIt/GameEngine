# ADR 0150: Multi-Model Routing and Workload Specialization

Status: Proposed
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

## Dependencies and parallel work

Implementation is sequenced after ADR 0141 and ADR 0142. ADR 0143 and ADR 0144 are optional inputs; if they are not yet on `main`, the router uses only available resource/provider signals.

## Verification

Tests and benchmarks must prove stable session/run identity across model switches, explicit context handoff, capability-safe routing, preserved permission/completion semantics, measured improvement over baseline, deterministic fallback policy where possible, and no hidden credential/context leakage between providers.

Routing/model-status UI requires Editor Visual Validation.

## Non-goals

This ADR does not make multiple models a correctness requirement, allow weak models to rewrite architecture-sensitive decisions without evidence, or route to an unverified model merely because it is cheaper/faster.
