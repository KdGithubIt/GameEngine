# ADR 0028: Advanced Authoring Roadmap Boundaries

Status: Accepted
Date: 2026-06-13

## Context

The roadmap after Phase 25 is moving from a loose "Phase 26+" bucket to a
numbered Advanced Authoring plan. That creates several ambiguities:

1. The existing roadmap says the AI Agent Bridge should be referenced by
   name, not by the old Phase 26 number, because ADR 0026 already defines
   the engine boundary.
2. Proposed editor phases such as Edit Mode Scene View, Scene Picking, and
   Transform Gizmo appear to overlap with Phase 17, which already covers
   "Scene Editing / Preview" stabilization.
3. Input Actions in the advanced-authoring plan overlap with the deferred
   Action Mapping item in Phase 12-D.
4. ProjectHub overlaps with Phase 9 project opening and the existing
   `ProjectRoot` implementation.
5. Some future phases, such as Asset Database v2 and Prefab, will eventually
   require ADRs, but their detailed contracts are not ready to freeze yet.

Without a boundary decision, the roadmap can accidentally treat stabilization
work as replacement work, or freeze speculative future contracts too early.

## Decision

1. **Advanced Authoring may use concrete phase numbers after Phase 25.**
   Phase numbers are scheduling metadata. Feature names remain the stable
   reference when an existing ADR or cross-document reference already uses a
   feature name.
2. **AI Agent Bridge remains the stable feature name.** It may be scheduled
   as a later numbered phase in the Advanced Authoring roadmap, but documents
   should refer to it as "AI Agent Bridge" and may append the current planned
   phase number in parentheses. ADR 0026 remains the boundary: the engine owns
   virtual input and frame observation primitives; PNG encoding, prompt
   construction, AI API calls, MCP/CLI exposure, and agent orchestration live
   outside the engine.
3. **Phase 17 is not superseded by later Scene View work.** Phase 17 remains
   the stabilization phase for the existing edit/play/save loop: camera,
   light, material, hierarchy, selection synchronization, Game View resizing,
   and the regression checklist. Later Edit Mode Scene View, Scene Picking,
   and Transform Gizmo phases build direct-manipulation tools on top of that
   stable loop. They do not replace Phase 17.
4. **Input Actions are the implementation home for deferred Action Mapping.**
   Phase 12-D remains a deferral note: game systems continue to use direct
   `Input<KeyCode>` / mouse resources until a later Input Actions phase
   introduces project-wide action bindings. That later phase must define the
   binding data model, persistence, defaults, editor UX, and runtime lookup
   contract before implementation.
5. **ProjectHub is an editor entry-point phase, not a new project model.**
   New Project, Open Project, and Recent Projects must use
   `engine_authoring::ProjectRoot` and the project layout from ADR 0023.
   Recent project history remains editor preference state in `crates/editor`,
   not `engine-authoring`. Existing Phase 9 project-opening code is the
   underlying mechanism; ProjectHub makes it the front-door workflow and adds
   missing creation/recent-project UX.
6. **Future ADRs are written at the contract boundary, not for every roadmap
   row.** A numbered phase needs a dedicated ADR before it changes a shared
   crate contract, serialized file format, authoring command, diagnostics
   contract, CLI/MCP surface, or durable editor/runtime boundary. Roadmap-only
   phase documents may be written earlier, but they must not freeze those
   contracts without an ADR.

## Consequences

- The roadmap can assign concrete numbers to Advanced Authoring phases
  without reviving the old "AI Agent Bridge = Phase 26" coupling.
- Phase 17 remains necessary even if later direct-manipulation editor phases
  are added.
- Phase 12-D and the later Input Actions phase no longer compete for the same
  implementation slot.
- ProjectHub work can proceed without moving `ProjectRoot` or duplicating
  project path validation in the editor.
- Asset Database v2, Prefab, Input Actions, and AI Agent Bridge still require
  their own ADRs when their contracts are ready to implement.

## Alternatives Considered

### Keep the unnumbered Phase 26+ bucket

Rejected. The bucket no longer gives enough scheduling information once the
roadmap wants ProjectHub, Scene View, Picking, Gizmo, Asset Database, Prefab,
Input Actions, and AI Agent Bridge as separate deliverables.

### Replace Phase 17 with the later Scene View phases

Rejected. Phase 17 is deliberately a stabilization and regression-checklist
phase. Direct manipulation tools are higher-level editor features and should
not be used to avoid stabilizing the existing edit/play/save loop.

### Create detailed ADRs for every future phase immediately

Rejected. Asset Database v2, Prefab, and Input Actions are likely to change
shared contracts, but their exact data models and APIs should be decided when
implementation is close enough to evaluate real constraints. Creating detailed
ADRs now would freeze speculative designs.

### Treat ProjectHub as a new project ownership layer

Rejected. ADR 0023 already places project ownership in `engine-authoring`.
ProjectHub is editor UX over that shared project model.

## Compatibility and Migration

No serialized project, scene, graph, asset manifest, or command format changes
in this ADR.

Existing references to "AI Agent Bridge (old Phase 26)" should migrate to the
stable feature name plus the current planned phase number when the roadmap is
renumbered. Existing Phase 9 project-opening implementation remains valid and
is reused by ProjectHub.
