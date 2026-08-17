# ADR 0142: GameEngine Agent Benchmark and Curated Model Catalog

Status: Proposed
Date: 2026-08-17
Builds on: ADR 0131, ADR 0135, ADR 0141
Relates to: ADR 0143, ADR 0150

## Context

GameEngine already records native-question metadata and ADR 0131 requires model recommendations to be based on GameEngine-specific evidence rather than third-party leaderboards. The current product still requires a user to supply a compatible local endpoint/model manually and has no authoritative benchmark corpus that can justify Lightweight, Balanced, or High Quality recommendations.

A recommendation system must compare models under the same harness and completion criteria. Raw tokens per second is insufficient because the product goal is successful governed game creation.

## Decision

GameEngine maintains a versioned **GameEngine Agent Benchmark** and a curated **Local Model Catalog** derived from its results. Benchmark data is product/application data, not canonical project authoring data.

### Benchmark identity

Each comparable result records at least:

- benchmark corpus/version;
- harness version;
- exact model and runtime representation, including quantization when applicable;
- backend/runtime version;
- hardware identity and total GPU/system memory;
- quality/workload policy;
- available tool/permission budget; and
- completion criteria used by the task.

A comparison that changes multiple dimensions must be labeled non-equivalent rather than presented as a model-only ranking.

### Task corpus

The benchmark SHOULD include representative read questions, project inspection, code implementation, typed authoring mutation, validation/repair, Play/runtime interaction, and visual evaluation tasks. The corpus must exercise real GameEngine contracts such as stale revisions, permission requests, code workspace reconciliation, managed validation, and host-owned completion.

Private user projects are not silently uploaded into the benchmark corpus. Repository-owned fixtures and intentionally contributed benchmark projects are preferred.

### Metrics

Applicable measurements include acceptance-criteria success, completion-gate success, model turns, tool calls, invalid tool calls, code edits, validation attempts, repair loops, Play/frame/evaluation attempts, human interventions, elapsed time, token counts, load latency, TTFT, generation throughput, peak backend/Editor GPU memory, model unload/reload cost, renderer reclaim/resume cost, and OOM failures. Unavailable telemetry is recorded as unavailable.

### Curated catalog

The catalog presents a small set of understandable profiles such as Lightweight, Balanced, and High Quality. Each entry records runtime/backend identity, exact model/version, source, license/provenance, transfer/storage size, memory guidance, context/modality/tool capabilities, benchmark version, and recommendation evidence.

Model weights are never bundled merely because an entry is recommended. Download is an explicit user action with source and expected storage/transfer shown before acquisition. Existing compatible local runtimes/models SHOULD be discoverable. Advanced users may configure compatible custom models that remain marked unverified until benchmarked.

## Recommendation policy

No hard-coded `VRAM >= N => model X` rule is architectural truth. Recommendations combine measured success, latency, memory pressure, model capabilities, licensing/provenance, and supported backend behavior. Hardware-specific recommendations may exist as product data.

The RTX 4070 Ti 12 GB remains the first reference profile from ADR 0135, not a minimum requirement. Additional hardware profiles may be added independently.

## Dependencies and parallel work

Benchmark storage/UI scaffolding may begin in parallel with ADR 0141. Official write-capable implementation recommendations require ADR 0141 to be available. ADR 0143 telemetry enriches resource measurements but does not block the benchmark architecture. ADR 0150 MUST NOT become the default until this ADR demonstrates a measurable benefit over the single-model baseline.

## Verification

Implementation must prove reproducible same-harness comparisons, explicit unavailable telemetry, stable benchmark versioning, catalog provenance, no secret/project-history leakage, installed-model discovery where supported, and UI distinction between compatible/unverified and benchmark-recommended models.

Catalog/benchmark UI changes require Editor Visual Validation.

## Non-goals

This ADR does not select one permanent model family, make third-party scores authoritative, automatically download weights without consent, or define multi-model routing policy.
