# ADR 0140: AI Capability Roadmap and Parallel Delivery Order

Status: Proposed
Date: 2026-08-17
Relates to: ADR 0035, ADR 0117, ADR 0121, ADR 0131, ADR 0132, ADR 0133, ADR 0135, ADR 0139

## Context

ADR 0121, ADR 0131, ADR 0133, and ADR 0135 establish the first-release AI authoring, Agent Host, Remote AI Studio, and native-inference resource contracts. Their first-release requirements intentionally leave several later capabilities open. Those capabilities are large enough that implementing them under one follow-up ADR would couple unrelated provider, renderer, lifecycle, security, and authoring risks.

The repository also supports parallel ChatGPT implementation work. Parallel work is safe only when each task has a clear ownership boundary and does not depend on an unmerged sibling branch. A roadmap therefore needs to state dependencies without making later work assume another task branch.

## Decision

The post-first-release AI roadmap is split into focused ADRs:

- ADR 0141: native write-capable Agent Runtime and governed tool loop;
- ADR 0142: GameEngine Agent Benchmark and curated local-model catalog;
- ADR 0143: native ModelBackend resource controls and hardware telemetry;
- ADR 0144: hosted and enterprise ModelBackend integration and credential ownership;
- ADR 0145: first-class external Agent Runtime provider adapters;
- ADR 0146: governed AI asset acquisition and generative-content providers;
- ADR 0147: detached local AI Studio frontend and versioned local host protocol;
- ADR 0148: remote host lifecycle, project activation, and narrow remote startup operations;
- ADR 0149: engine-native live observation and media transport;
- ADR 0150: multi-model routing and workload specialization;
- ADR 0151: headless write-capable MCP host and project-writer ownership;
- ADR 0152: multi-agent writer coordination and conflict ownership; and
- ADR 0153: agent process confinement and OS/provider sandbox integration.

These ADRs refine existing boundaries. They MUST NOT replace `AgentSession`, `AgentRun`, `AgentEvent`, proposal snapshots, permission policy, the code workspace, MCP authoring authority, completion gates, Remote AI Studio idempotency/reconnect semantics, or ADR 0135 resource arbitration.

## Delivery waves

### Wave A: independent foundations

The following ADRs may be implemented in parallel from current `main` because their required base contracts already exist:

```text
0141 Native write-capable Agent Runtime
0143 Native backend resource controls
0144 Hosted / enterprise ModelBackend
0145 External Agent Runtime adapters
0146 Asset acquisition providers
0147 Detached local AI Studio frontend
0148 Remote host lifecycle
0149 Engine-native live observation
0151 Headless writer ownership
0153 Process confinement / sandbox integration
```

Each implementation still starts from its own current-main baseline. No Wave A task may assume another Wave A branch is merged. If two tasks need the same new public contract, that contract must first land through the owning ADR or the tasks must remain independent until integration on `main`.

### Wave B: measured model selection

ADR 0142 follows ADR 0141 for its full write-capable benchmark corpus. Benchmark harness scaffolding may begin earlier, but official implementation/repair/playtest recommendations require the native write-capable path to exist. ADR 0143 telemetry SHOULD be consumed when available but is not required to start the benchmark service.

### Wave C: model routing optimization

ADR 0150 follows ADR 0141 and ADR 0142. A multi-model router is not accepted as a correctness dependency before a single-model native Agent is complete and GameEngine-specific evidence shows a measurable benefit. ADR 0143 and ADR 0144 may provide additional routing signals/backends when they have already landed.

### Wave D: concurrent writers

ADR 0152 follows ADR 0151 and the existing ADR 0139 working-copy authority. Multiple AI writers require an explicit writer/lease/conflict model and MUST NOT be introduced merely by allowing several existing single-writer runs to mutate concurrently. ADR 0141 should also be stable enough to provide a representative writer client before multi-writer behavior is enabled.

## Parallel implementation rule

Parallel ADR work uses only accepted/current contracts from `main`. An implementation branch MUST NOT import types, assumptions, schema changes, or behavior that exist only on another unmerged ADR branch. When an optional integration becomes desirable, it is performed after both prerequisite contracts are on `main` or through a separate integration change.

## Deferred ideas that do not yet receive separate ADRs

ADR 0133 already permits later WebSocket transport, native iOS/Android clients, direct semantic remote controls over shared authoring capabilities, and supported public-Internet reachability subject to a new threat model. Those remain explicit future triggers rather than scheduled implementation ADRs until a demonstrated product need justifies their additional platform or security surface.

Likewise, provider-specific model families are catalog data under ADR 0142 rather than one ADR per model.

## Verification

This roadmap is satisfied when each child ADR can be implemented and validated independently against current `main`, declared prerequisites are respected, and no child ADR weakens existing first-release completion or trust boundaries.

Documentation-only adoption of this ADR requires no Visual Validation.
