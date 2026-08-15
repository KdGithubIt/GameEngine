# ADR 0113: Runtime Domain Crate Decomposition

Status: Accepted
Date: 2026-08-14
Builds on: ADR 0111, ADR 0112

## Context

`engine-rig` established that low-level runtime domains can be separated from the
large `engine` package without changing their public engine-facing paths. The
remaining `engine` crate still owns unrelated animation, asset, audio, physics,
gameplay, import, rendering, scene, and scripting code. A change in one of those
domains therefore continues to compile a package whose dependency graph includes
most of the runtime stack.

The problem is not specific to MMD. PMX/VMD happened to expose the cost because
those modules are large, but treating MMD as a special compile boundary would
preserve the same problem for FBX, gameplay, audio, collision, animation, and
render integration changes.

The desired boundary is therefore domain ownership rather than source-format
ownership. Package sizes are a guardrail, not the primary objective: change
frequency, dependency weight, and incremental compilation cost must not remain
concentrated in one high-level package.

ADR 0111 rejected moving every subsystem in the same first rig-boundary change.
This ADR does not reverse that safety decision. It fixes the complete target
architecture now, then requires implementation in dependency-ordered commits so
that each step remains reviewable and reversible.

## Decision

### 1. Split high-level runtime ownership into domain crates

The workspace target is:

```text
crates/
  ecs/                # engine-ecs; existing
  authoring/          # engine-authoring; existing
  renderer/           # engine-renderer; existing low-level GPU/surface crate
  core/               # engine-core
  assets/             # engine-assets
  rig/                # engine-rig; existing from ADR 0111
  animation/          # engine-animation
  physics/            # engine-physics
  render-runtime/     # engine-render-runtime
  import/             # engine-import
  gameplay/           # engine-gameplay
  platform/           # engine-platform
  scripting/          # engine-scripting
  scene/              # engine-scene
  engine/             # thin composition and compatibility facade
```

The ownership target is:

| Package | Primary ownership |
| --- | --- |
| `engine-core` | frame/fixed time, runtime metadata, engine-owned input primitives, small shared runtime contracts |
| `engine-assets` | runtime asset handles/manifests, generic data assets, derived cache, format-independent model IR |
| `engine-rig` | transform hierarchy, skeleton identity, skinning, layered rig pose, rigid-body/secondary-motion rig descriptions |
| `engine-animation` | clips/animator, animation graph, parameters, pose graph, retargeting, foot IK, contact detection, animation-side morph state |
| `engine-physics` | gameplay physics, collision, character controller, advanced geometry, navmesh, engine-native secondary-motion simulation |
| `engine-render-runtime` | camera/light, runtime mesh/material resources, LOD, particles, shadows, post-process, preview, debug drawing, runtime UI presentation |
| `engine-import` | format-independent model build/import orchestration plus glTF, FBX, PMX, and VMD parsers/importers |
| `engine-gameplay` | ability, behavior tree runtime, combat, hitbox, lock-on, player/gameplay control |
| `engine-platform` | audio backend/runtime, platform input ingestion, gamepad adapters, target-specific runtime integration |
| `engine-scripting` | host-independent project scripting/game SDK contracts: game API, game IO/query ABI, module registration/export ABI, command payloads, and typed convenience helpers |
| `engine-scene` | scene loading/management, authoring-to-runtime scene build bridge, save, replay |
| `engine` | `App`, schedule/runtime-system composition, concrete cross-domain host/effect adapters, dynamic game-module loading/`World` application, compatibility re-exports |

Runtime HUD/UI modules that execute and render in-game UI belong to
`engine-render-runtime`; editor UI remains outside this boundary. Cross-domain
registries whose only purpose is to assemble types from several domain crates
may remain in `engine`, but the domain-owned type declarations and systems they
register must live in their owning crates.

### 2. MMD is classified by functionality, not by a dedicated runtime crate

There is no `engine-mmd` package.

- PMX and VMD parsing/import live in `engine-import`.
- VMD output is an ordinary animation asset consumed by `engine-animation`.
- PMX-generated secondary-motion data is converted to engine-native rig data.
- Secondary-motion simulation lives in `engine-physics` and follows ADR 0112.
- Rendering of imported models uses the same `engine-render-runtime` contracts
  as models from other source formats.

No lower runtime crate may depend on PMX, VMD, MMD, or Bullet concepts merely to
support an imported asset after conversion.

### 3. Dependencies are one-way and `engine` is never a lower-layer dependency

The conceptual dependency order is:

```text
                         engine-core
                            |
             +--------------+---------------+
             |              |               |
        engine-assets   engine-rig    engine-platform
             |              |
             +-------+------+------+
                     |             |
              engine-animation  engine-physics
                     |             |
                     +------+------+ 
                            |
                  engine-render-runtime
                     |             |
                     +------+------+
                            |
                       engine-import
                            |
                       engine-gameplay
                            |
                      engine-scripting
                            |
                        engine-scene
                            |
                          engine
```

This drawing expresses a legal topological order, not a requirement that every
higher crate depend on every lower crate. Dependencies should remain narrower
than the drawing whenever possible.

The following rules are normative:

1. No workspace crate below `engine` may depend on `engine`.
2. Circular workspace crate dependencies are forbidden.
3. `engine-renderer` remains the low-level GPU/surface owner defined by the
   authoring specification; `engine-render-runtime` may depend on it, never the
   reverse.
4. `engine-rig` retains the ADR 0111 boundary and must not gain renderer,
   importer, audio, or physics-solver dependencies.
5. Import-format crates/modules depend on engine-owned neutral contracts; lower
   runtime domains do not depend on importers.
6. Cross-domain debug/presentation systems belong at the higher dependency
   layer instead of forcing a lower domain to depend upward. For example,
   collider/navmesh visualization must not make `engine-physics` depend on
   `engine-render-runtime`.
7. Scripting command payload definitions must not require the scripting crate
   to own scene/audio/UI implementation. Command application is delegated to
   the owning domain or to the final `engine` composition layer so
   `engine-scene -> engine-scripting` does not form a cycle.

For scripting and project-module integration, ownership is therefore split at
the host-independence boundary rather than at file-name boundaries. The
`engine-scripting` crate owns contracts that can execute or serialize without a
live ECS `World` or concrete scene/audio/UI/physics resources. The final
`engine` layer may retain dynamic-library lifetime/loading, `World` compilation
and output application, Rhai context/effect adapters, command effect systems,
and live physics/camera snapshot construction when moving them downward would
introduce an upward dependency or a cycle. Such adapters are final composition,
not unfinished domain ownership.

### 4. Neutral boundary data must actually be neutral

Moving a file is insufficient when its types point back into a higher domain.
The extraction must remove those dependency leaks.

In particular, `model_ir` belongs to `engine-assets`, but the existing IR uses
runtime animation keyframe/property types and runtime mesh types directly. The
IR must instead own format-independent data representations (or depend only on
lower neutral contracts), and the model builder must convert those values into
`engine-animation` and `engine-render-runtime` assets. `engine-assets` must not
acquire reverse dependencies on those higher crates merely to preserve the old
implementation shape.

Likewise, generic data assets must use authoring schema/value contracts directly
rather than reaching them through the scripting/game-module implementation.

Engine-owned input primitives must be separated from platform event ingestion.
`engine-core` must not depend on `winit`, `gilrs`, `rodio`, a browser API, or
another OS adapter just to expose input state/contracts.

### 5. Preserve the `engine` public facade during migration

Existing public paths such as `engine::animation`, `engine::asset`,
`engine::collision`, and `engine::scene_manager` remain compatibility facades
unless a separate API-breaking decision removes them.

The facade re-exports the single concrete types owned by domain crates. Types
must not be duplicated merely to keep the old module path, because duplicate
concrete types would change Rust `TypeId` identity and ECS behavior.

Serialized authoring schemas, stable IDs, component type strings, command
semantics, and persisted file formats are unchanged by this decomposition.
`std::any::type_name` output may change to the owning crate name and is not a
persisted compatibility contract.

### 6. Migrate in dependency order

Implementation proceeds in independently reviewable commits in this order:

1. `engine-core` and `engine-assets`, including neutralization of shared data
   contracts that would otherwise point upward.
2. `engine-platform` where it can consume core input contracts without pulling
   platform dependencies into core.
3. `engine-animation` and `engine-physics`, both building on the already
   separated `engine-rig`.
4. `engine-render-runtime`, with cross-domain visualization moved upward rather
   than introducing reverse dependencies.
5. `engine-import`, after model IR and target runtime asset APIs are stable.
6. `engine-gameplay`.
7. `engine-scripting`, with command application inverted at domain boundaries
   where required.
8. `engine-scene`, including the broad scene bridge only after lower domain
   APIs no longer point back to `engine`.
9. Reduce `engine` to application/runtime-system composition, cross-domain
   registration, and compatibility re-exports.
10. Update CI changed-path classification so every new known crate selects its
    own package instead of falling back to full validation.

A temporary facade in `engine` is acceptable during this sequence. A temporary
lower-crate dependency on `engine` is not.

### 7. Package size is a diagnostic guardrail

The intended steady-state size for most newly extracted runtime crates is
roughly 200-500 KiB of Rust source. `engine-rig` is intentionally smaller and a
domain may exceed this range when a coherent implementation warrants it.

The split must not be distorted solely to hit equal LOC. A package that remains
far larger, changes much more often, or carries disproportionately expensive
third-party dependencies should be reviewed for another natural boundary.
Conversely, tiny packages should not be invented only to reduce line counts.

### 8. CI must preserve the latency benefit

The changed-path classifier must recognize the new crate directories and map
them to their package names. Workspace/Cargo/build-infrastructure changes still
select full validation as defined by `DEVELOPMENT_WORKFLOW.md`.

Domain-only changes should be able to use affected-package validation without
compiling unrelated high-level domains. Full validation on `master` and nightly
continues to provide reverse-dependent workspace coverage.

## Consequences

- Changes to audio, collision, animation, importers, gameplay, or rendering no
  longer inherently invalidate one monolithic `engine` package.
- MMD code follows the same dependency rules as every other source format.
- Some current modules require real dependency inversion rather than a mechanical
  file move; `model_ir`, input ingestion, debug visualization, scripting command
  application, and the scene bridge are explicit examples.
- Host-independent scripting/game SDK contracts compile in `engine-scripting`,
  while adapters requiring a live `World` or multiple concrete runtime domains
  remain intentionally in the final `engine` composition layer.
- The compatibility `engine` crate remains convenient for downstream users while
  compile ownership moves to smaller packages.
- Cargo manifests and CI classification become part of the architectural change,
  because an unrecognized crate would otherwise erase much of the expected PR
  latency improvement.
- The migration produces several commits but one coherent final dependency DAG.

## Alternatives considered

### Add an MMD-specific package

Rejected. It optimizes around the workload that exposed the problem instead of
the dependency domains that cause it. PMX/VMD import, animation, physics, and
rendering have different owners and should compile independently.

### Keep `engine` monolithic and rely on sccache

Rejected. Caching helps repeated compiler work but does not remove the package
invalidation boundary or the dependency weight of compiling unrelated runtime
systems together.

### Move every file mechanically in one commit

Rejected. Existing cross-module references contain real upward dependencies and
would either create crate cycles or force lower crates to depend on higher
implementation details. The final architecture is fixed by this ADR, but code
moves are dependency ordered and may include small API-neutral inversions.