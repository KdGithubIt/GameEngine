# ADR 0142: GameEngine Agent Benchmark and Curated Model Catalog

Status: Accepted
Date: 2026-08-17
Builds on: ADR 0131, ADR 0135, ADR 0141
Relates to: ADR 0143, ADR 0150, ADR 0152

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
- available tool/permission/work-claim kind/count budget without raw project target keys; and
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

## Implementation

The first-release implementation keeps benchmark state in machine-local GameEngine application data and leaves canonical project/session data unchanged. `engine-editor` owns a versioned seven-class corpus contract covering read questions, project inspection, code implementation, typed authoring mutation, validation/repair, runtime interaction, and visual evaluation. Records retain the corpus/task identity, benchmark and provider harness versions, exact discoverable model representation, backend version, hardware/resource telemetry with explicit unavailable values, quality/workload policy, tool/permission/work-claim kind/count budget, completion criteria, and measured Agent Host evidence. The selected benchmark task identity and ADR 0135 workload classification are frozen when inference or a native run starts. Read-question evidence satisfies its provenance completion gate only when the read harness actually retrieved at least one evidence chunk; a plain model answer without retrieved provenance remains a recorded failure and cannot qualify a catalog recommendation. On Windows, the first release snapshots the active Editor wgpu adapter identity, accepts dedicated GPU memory only from one matching non-software DXGI adapter, and measures total physical system memory through the operating system. Ambiguous adapter matching, API failure, zero dedicated-memory reporting, and unsupported platforms remain explicitly unavailable rather than being estimated.

Model-only comparison is permitted only when corpus/task, both harness identities, backend runtime, hardware profile, quality/workload policy, tool/permission/work-claim kind/count budget, and completion criteria match. Changed dimensions are returned as an explicit non-equivalent comparison instead of being ranked as if only the model changed. Write-capable benchmark success is task-specific: every completion gate named by the versioned task descriptor must be `Passed`; generic Agent Host `NotApplicable` completion is not sufficient for a required benchmark gate, and the validation/repair task additionally requires at least one repair/revalidation cycle represented by two managed validation attempts. The benchmark store intentionally omits prompts, conversation history, retrieved source text, project paths, raw work-claim target keys, and credentials. Recording local evidence is an explicit AI Studio action rather than silent collection or upload.

The curated catalog is bundled as versioned product data separate from model weights. A candidate records exact model/runtime representation plus source, license, transfer/storage size, memory guidance, context, modality, and tool-capability metadata. Lightweight, Balanced, and High Quality recommendation slots are derived only from complete comparable GameEngine corpus evidence; an empty or incomplete evidence set produces no recommendation rather than a guessed default. The first catalog ships without invented candidate recommendations and becomes populated only from measured evidence with explicit provenance.

The initial Ollama-compatible loopback backend can discover its installed model inventory and backend version through its supported local HTTP API. AI Studio keeps exact custom model entry available, clearly distinguishes compatible/unverified selections from benchmark-qualified recommendations, and exposes unknown provider/resource capability data as unavailable. ADR 0144 hosted/enterprise backends remain on the same provider-independent `NativeModelConfig` path; representation metadata unavailable from those providers is not inferred. Discovery never reaches a non-loopback endpoint, and model acquisition remains outside this ADR so no weights are silently downloaded.

### ACP benchmark migration

Benchmark record schema v4 adds an optional explicit runtime classification without reinterpreting historical records. Schema v1-v3 records remain valid legacy-harness evidence with no synthesized lane identity. New integrations may register exactly three migration lanes: `raw_model` for direct model/GGUF/runtime measurement, `agent_harness` for a fixed underlying model/runtime exercised through an agent harness, and `coding_agent` for end-to-end coding-agent measurement. Raw Model evidence uses the dedicated `raw_model_generation_v1` task identity and `model_response_completed` criterion rather than reusing one of the seven Agent Benchmark task descriptors. A missing v4 runtime identity continues to mean the existing GameEngine benchmark harness; it is not silently promoted into one of the new lanes.

The explicit runtime identity records harness identity/version, adapter version, agent runtime identity/version when applicable, negotiated ACP protocol version when applicable, MCP/tool-contract identity, and permission profile. ACP runtime identity is copied from the provider-neutral ADR 0166 `AcpRuntimeIdentity`; benchmark code does not depend on Codex, Claude, Goose, or another provider adapter. Existing `BenchmarkModelIdentity` remains authoritative for model/GGUF/backend-runtime representation and is not replaced by agent identity.

Comparison equivalence is lane-aware. Legacy and `raw_model` evidence may qualify a model-only comparison only when the established non-model dimensions match. `agent_harness` comparison additionally requires the same exactly measured underlying model representation and the same measured ACP, MCP/tool, and permission contracts while allowing the harness/agent implementation to be the candidate dimension. `coding_agent` comparison treats the end-to-end agent implementation as the candidate dimension while requiring the same measured MCP/tool and permission contracts. Agent-inclusive equivalence is never consumed as model-only catalog or routing evidence. Different lanes are explicitly non-equivalent.

Model-internal telemetry exposed only below an ACP agent remains truthful: model turns, prompt tokens, and response tokens are `Unavailable` when the adapter cannot observe them. GameEngine MUST NOT infer those values from ACP prompt boundaries, session events, or normalized Agent Host progress. The legacy native AgentRun path keeps its existing recorded-exchange semantics so historical records retain their original meaning.

## Dependencies and parallel work

Benchmark storage/UI scaffolding may begin in parallel with ADR 0141. Official write-capable implementation recommendations require ADR 0141 to be available. ADR 0143 telemetry enriches resource measurements but does not block the benchmark architecture. ADR 0150 MUST NOT become the default until this ADR demonstrates a measurable benefit over the single-model baseline.

## Verification

Implementation must prove reproducible same-harness comparisons, explicit unavailable telemetry, stable benchmark versioning, catalog provenance, no secret/project-history leakage, installed-model discovery where supported, and UI distinction between compatible/unverified and benchmark-recommended models.

Catalog/benchmark UI changes require Editor Visual Validation.

## Non-goals

This ADR does not select one permanent model family, make third-party scores authoritative, automatically download weights without consent, or define multi-model routing policy.
