# ADR 0143: Native ModelBackend Resource Controls and Hardware Telemetry

Status: Proposed
Date: 2026-08-17
Builds on: ADR 0131, ADR 0135
Relates to: ADR 0003, ADR 0072, ADR 0104, ADR 0136, ADR 0142

## Context

ADR 0135 defines workload classes, quality preferences, inference-focused Editor suspension, transient renderer reclaim, interruption/resume, and capability-driven model residency. The current generic local backend truthfully reports provider-specific resource controls as unavailable. The missing layer is concrete adapter support for resource telemetry and safe unload/offload/residency operations.

Without that integration the policy can suspend Editor presentation but cannot reliably return model memory to Play or request a larger safe reasoning budget from a backend.

## Decision

`ModelBackend` implementations may expose a capability object for resource behavior. Capabilities are optional and truthful; unsupported controls remain unavailable. The application-layer resource broker consumes them without importing backend-specific process or GPU ownership into renderer/runtime crates.

Capabilities may include:

- model representation size;
- estimated/measured GPU residency;
- CPU/GPU offload controls;
- device selection;
- context/KV-cache placement;
- inference-cache release;
- model unload/reload;
- load/unload timing; and
- backend-specific memory telemetry.

A backend operation reports completion only after the adapter has evidence that the requested state change occurred or after the backend's documented acknowledgement contract is satisfied. Guessing from process lifetime, model name, or nominal VRAM is forbidden.

## Hardware observation

Renderer/application and model telemetry remain separate sources. The broker may combine total device capacity, known renderer allocations, backend estimates, measured peaks from earlier runs, conservative headroom, and operating-system/platform information. If exact free VRAM is unavailable, the result is an estimate with explicit uncertainty rather than a fabricated exact value.

## Resource transitions

The broker requests backend controls only at ADR 0135 safe boundaries. Play and required frame capture have priority over local inference. Interrupt-for-Editing may reduce or unload model residency. Resume can reload/reacquire resources only after authoritative-state re-inspection.

Deep-only aggressive renderer reclaim remains conditional on measurements. This ADR does not authorize the agent layer to destroy renderer-owned resources directly; renderer release/invalidation APIs remain owned by their renderer/resource layer.

## Adapter isolation

Provider-specific HTTP/process APIs belong in the ModelBackend adapter/application boundary. Runtime ECS, `engine-authoring`, `engine-mcp`, and low-level renderer crates must not depend on Ollama or another inference runtime SDK merely to expose telemetry.

## Dependencies and parallel work

This ADR can be implemented in parallel with ADR 0141, ADR 0144-0149, ADR 0151, and ADR 0153. It does not require ADR 0142, but ADR 0142 should consume the resulting telemetry when available. ADR 0150 may later use resource capabilities as routing inputs.

## Verification

Tests must cover unavailable capability honesty, successful and failed unload/offload requests, interruption releasing supported residency, Play priority reclaim, reload timing/telemetry, no canonical authoring changes during resource transitions, and safe renderer restoration after any approved reclaim path. Hardware-dependent tests must distinguish deterministic adapter contract tests from reference-machine measurement runs.

Editor-visible resource posture controls require Visual Validation.

## Non-goals

This ADR does not implement OS-wide GPU scheduling, control external Claude/Codex processes that do not expose a safe resource API, or define model selection/routing policy.
