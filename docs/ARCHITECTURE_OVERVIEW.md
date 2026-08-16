# Current Architecture Overview

Status: Current overview
Last audited: 2026-08-16
Audit baseline: `e99d227ab40a7d839afa718e40ab372ec1899f32`

This document answers **what the GameEngine architecture is now**. Architecture
Decision Records answer **why the project chose that architecture**. When this
overview conflicts with an Accepted ADR or a canonical specification, fix the
overview or explicitly revise the contract; do not create a third competing
architecture.

The ADR register and domain entry points are in
[`docs/adr/README.md`](adr/README.md). The audit that produced this overview is
[`docs/adr/AUDIT_2026-08-16.md`](adr/AUDIT_2026-08-16.md).

ADR 0122 through ADR 0127 are `Proposed` at this audit baseline. They describe
possible future spatial-audio, Behavior Tree, navigation, VFX, Timeline, and
native-2D contracts and are intentionally not treated as current architecture
here. ADR 0128 is the Accepted renderer-owned full image-based-lighting
decision, and ADR 0129 is the Accepted generic directional/point/spot direct
lighting decision. Both renderer records were renumbered from later colliding
ADR 0122/0123 records during this audit.

## 1. Workspace shape

The current workspace contains these runtime-domain packages:

| Package | Current architectural role |
| --- | --- |
| `engine-ecs` | Runtime ECS storage, queries, resources, and scheduling primitives |
| `engine-core` | Small shared runtime contracts such as time/input primitives and metadata |
| `engine-assets` | Neutral runtime asset contracts, cache data, and format-independent model IR |
| `engine-rig` | Transform hierarchy, skeleton identity, skin binding, layered pose, rig descriptions |
| `engine-animation` | Clips, animator, animation graph/runtime parameters, pose/retargeting contracts |
| `engine-physics` | Collision, controller/navigation, solver-backed physics, secondary motion |
| `engine-renderer` | Low-level GPU context and optional presentation-surface integration |
| `engine-render-runtime` | Runtime mesh/material/light/UI/render passes and presentation behavior |
| `engine-import` | Format-independent import orchestration plus glTF, FBX, PMX, and VMD import |
| `engine-gameplay` | Ability, behavior, combat, hitbox, lock-on, and gameplay control |
| `engine-platform` | Native input, audio, gamepad, and target-specific adapters |
| `engine-scripting` | Host-independent project scripting and game SDK contracts |
| `engine-scene` | Scene loading/management, save, and replay domain ownership |
| `engine` | Final runtime composition, concrete cross-domain adapters, and compatibility facade |

The application and authoring side contains `engine-authoring`,
`engine-project-lifecycle`, `engine-launcher`, `engine-editor`, `engine-cli`,
`engine-mcp`, and the project Rust macro crates.

This is an ownership map, not a declaration that every package depends on the
package above it. Actual dependencies should be narrower than the conceptual
architecture whenever possible.

## 2. Runtime dependency rules

ADR 0113 and ADR 0114 define the main runtime rules:

1. A runtime package below `engine` must not depend on the `engine` facade.
2. Circular workspace package dependencies are forbidden.
3. `engine-renderer` owns low-level GPU/surface integration.
   `engine-render-runtime` may depend on it, never the reverse.
4. Neutral contracts belong below heavyweight implementations. Asset/model IR,
   input primitives, rig descriptions, and similar data must not acquire
   higher-domain or source-format dependencies merely to preserve an old code
   shape.
5. Heavy GPU, physics, windowing, audio, gamepad, GUI, and scripting-runtime
   backends stay behind the owner or composition boundary. Lightweight
   consumers request contract-only feature sets where the owner supports them.
6. `engine` is allowed to retain compatibility re-exports and concrete
   cross-domain composition that would otherwise create an upward dependency or
   cycle. A facade module is not a second owner of the concrete type.

The workspace already has the domain packages accepted by ADR 0113. Some
engine-facing modules remain as re-export facades, and some genuinely
cross-domain runtime integration remains in `engine`; those are evaluated by
the composition rule above rather than by file name alone.

## 3. Authoring versus runtime

`engine-authoring` is the editable source-of-truth layer. Runtime ECS state is
derived from validated authoring data and is not persisted as the authoring
identity model.

The durable invariants are:

- persisted authoring objects use stable IDs;
- runtime ECS entity IDs and process-local runtime asset handles are not
  project-file identities;
- Editor, CLI, and MCP semantic edits use shared GUI-free authoring
  commands/services rather than separate business rules;
- current-format-only loading follows ADR 0115; and
- project selection/process lifetime is outside authoring semantics.

Domain-neutral graph storage remains part of the authoring model. There is no
requirement for separate `graph` or `graph_bt` workspace crates in the current
architecture.

## 4. Project applications and AI authoring

ADR 0117 defines a project-first desktop lifecycle:

- Launcher selects/creates projects and starts or activates Editors.
- A normal Editor process owns one concrete `ProjectRoot` for its lifetime.
- `engine-project-lifecycle` owns GUI-free application lifecycle, standard
  scaffolding, compatibility checks, and exclusive Editor ownership.
- Recent-project and Editor workspace restoration data are user/application
  state, not canonical project data.

ADR 0121 defines the structured AI boundary:

- MCP is the structured AI authoring interface for a project open in the
  Editor.
- The active Editor owns the initial project-scoped loopback MCP endpoint.
- `engine-mcp` remains a transport-agnostic adapter over shared authoring
  services.
- CLI is the supported headless authoring/automation adapter.
- The AI Agent Bridge is for runtime observation/input and visual interaction,
  not a replacement semantic authoring model.

## 5. Asset, animation, physics, and rendering boundaries

Import formats normalize into shared engine contracts instead of creating
format-specific runtime architecture.

- glTF, FBX, PMX, and VMD parsing/import belong to the import boundary.
- Model IR is format-independent and belongs to the neutral asset layer.
- Skeleton/skin/rig identity belongs to `engine-rig`.
- Animation playback, graph, pose, retargeting, and binding events belong to
  the animation domain.
- Engine-native secondary motion and collision/solver behavior belong to the
  physics domain; PMX data is converted at import rather than leaking MMD
  concepts into lower runtime contracts.
- `StandardLit`, `ToonLit`, and `Unlit` share generic material/rendering
  contracts. Outline remains an independent pass.
- Scene-lighting arithmetic and HDR composition are linear; color/data texture
  semantics follow ADR 0118.
- Tangent-space material shading follows ADR 0119.
- Material schema v3 PBR inputs follow ADR 0120.
- Full image-based-lighting precomputation and process-local derived resources
  are renderer-owned per ADR 0128.
- Generic point/spot authoring IDs, deterministic 16/8 local-light budgets, and
  the shared StandardLit direct-light path follow ADR 0129.

## 6. CI and validation architecture

Package ownership for affected validation comes from Cargo metadata. The CI
classifier must not duplicate workspace package names and crate directories in
a second hard-coded table.

The authoritative validation contract is
[`docs/DEVELOPMENT_WORKFLOW.md`](DEVELOPMENT_WORKFLOW.md):

- `affected` validates planner-selected package scope;
- `full` validates the workspace, including Check and documentation;
- `docs` skips Rust compilation for recognized documentation-only PRs.

An affected success is not a full-workspace five-command success.

## 7. How to maintain this overview

Update this document when an Accepted ADR changes a current ownership boundary
or when an accepted migration reaches a materially different steady state.
Do not rewrite historical ADRs merely to make them read like current prose.

When reviewing architecture:

1. read this overview for the current map;
2. follow the relevant links in `docs/adr/README.md` for rationale and
   amendments;
3. confirm implementation ownership in the current workspace; and
4. update the canonical specification when the current contract changes.
