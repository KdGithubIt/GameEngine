# ADR 0143: Native ModelBackend Resource Controls and Hardware Telemetry

Status: Accepted
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

## First-release implementation

The initial governed adapter is the existing loopback-only Ollama-compatible ModelBackend. It reads `/api/ps` for backend-reported model representation size, GPU residency, and context length. Values are recorded as measured only when the backend reports them; device-wide free VRAM remains unavailable rather than being inferred from model size.

Model release and reload use the backend's `keep_alive` contract, and CPU-only residency requests use the backend's `num_gpu = 0` option. Each mutation is followed by a fresh `/api/ps` observation. Release completes only after the selected model is observed non-resident, reload only after it is observed resident, and GPU offload only after residency is absent, zero, or measurably lower than before. An acknowledgement without confirming telemetry is reported as an unconfirmed transition rather than success.

AI Studio runs these resource transitions on an application-owned worker. Interrupt-for-Editing waits for the native inference worker to reach its safe interruption boundary, then releases supported model residency before requesting Editor presentation restore. Managed Play resolves a `RuntimeObservation` resource plan and releases or reduces supported model residency before requesting runtime launch. Transition failure never fabricates success and does not let local inference take priority over required Editor restoration or Play. Resume still re-inspects authoritative Editor state before any later inference can reacquire model resources.

These controls and measurements are transient application state only. They do not mutate canonical authoring data, Stable IDs, serialization, runtime ECS state, or renderer ownership. Device selection, explicit KV placement, and inference-cache release remain unavailable in the first release.

## Verification

Tests must cover unavailable capability honesty, successful and failed unload/offload requests, interruption releasing supported residency, Play priority reclaim, reload timing/telemetry, no canonical authoring changes during resource transitions, and safe renderer restoration after any approved reclaim path. Hardware-dependent tests must distinguish deterministic adapter contract tests from reference-machine measurement runs.

Editor-visible resource posture controls require Visual Validation.

## Non-goals

This ADR does not implement OS-wide GPU scheduling, control external Claude/Codex processes that do not expose a safe resource API, or define model selection/routing policy.
