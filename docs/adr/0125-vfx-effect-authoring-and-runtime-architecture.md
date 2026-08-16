# ADR 0125: VFX Effect Authoring and Runtime Architecture

Status: Proposed
Date: 2026-08-16
Builds on: ADR 0042, ADR 0044, ADR 0054, ADR 0072, ADR 0104, ADR 0113, ADR 0121

## Context

The current particle system is a useful deterministic MVP. One
`ParticleEmitter` owns a CPU particle pool and supports continuous spawn rate,
lifetime/speed ranges, cone direction/spread, gravity, start/end color and
size, a maximum count, and deterministic preview. Rendering reuses GPU
instancing.

That model can produce smoke, sparks, and simple hit effects, but adding every
future effect feature as another field on `ParticleEmitter` would create an
unbounded component and a difficult Inspector. Production effects need bursts,
multiple emitters, shape modules, curves/gradients, billboard and texture-sheet
rendering, trails, collision/sub-emitters, and eventually a GPU simulation path.
The Editor also needs a dedicated effect preview/builder rather than asking
authors to tune dozens of unrelated component fields in a scene.

The architecture must retain deterministic CPU behavior and the existing simple
emitter use case while creating a compiled representation that does not depend
on arbitrary Rust callbacks and therefore can be moved to GPU execution later.

## Decision

### 1. Advanced VFX is an asset; scene entities host instances

Introduce a versioned `*.vfx.json` `VfxEffect` authoring document. One effect
contains one or more emitters with stable document-local IDs and ordered typed
modules.

Scene entities use a stable `engine.vfx_player` component that references a VFX
asset and owns only instance-level settings such as autoplay, looping/restart
policy, time scale, seed override, and optional parameter overrides.

Runtime simulation state lives in a transient `VfxInstance`. Particle IDs,
buffer offsets, GPU handles, random generator state, and elapsed simulation
state are never serialized into the asset or scene.

### 2. Preserve a simple emitter as a convenience surface, not a second runtime

The existing simple ParticleEmitter authoring surface remains useful for very
small effects. It is retained as a supported convenience authoring form while
advanced effects are introduced.

Both simple emitters and `VfxEffect` assets compile into the same runtime
`CompiledVfxEffect`/instance model. The engine MUST NOT maintain one simulation
implementation for simple scene particles and another unrelated implementation
for VFX assets.

This allows an Inspector-first workflow for a tiny spark effect and a dedicated
VFX Builder for complex assets without runtime semantic drift.

### 3. Use typed phase modules, not an unconstrained execution graph in v1

Each emitter has ordered module lists with explicit phases:

- Spawn: initial position, lifetime, velocity, color, size, rotation, custom
  attributes;
- Update: forces/drag/noise, color/size/rotation over life, kill/constraint
  operations; and
- Render: billboard, mesh, ribbon/trail, texture-sheet/material output.

Every module has a stable module ID, stable module type ID, schema-defined
properties, and phase. Module order is deterministic within a phase.

The VFX authoring document does not initially expose an arbitrary general graph.
A module stack gives common effects a clearer Editor UX, deterministic ordering,
and a straightforward GPU-compilable execution model. A future node graph may
compile to the same `CompiledVfxEffect` IR rather than replacing runtime
simulation.

### 4. Curves, gradients, shapes, and random ranges are engine-owned data types

VFX modules use shared serializable primitives rather than embedding bespoke
fields in every module. The initial authoring types include:

- constant/range scalar and vector values;
- normalized-lifetime curves with stable key identity and interpolation mode;
- color gradients with stable keys;
- emitter shapes such as point, sphere, box, cone, and mesh surface where
  supported; and
- deterministic random-stream/channel selection.

All values have finite/range validation. Curve and gradient evaluation is pure
and shared by runtime plus Editor preview.

### 5. Compile authoring documents to a backend-neutral effect IR

A shared VFX authoring service validates and compiles `VfxEffect` into a
`CompiledVfxEffect` that describes:

- emitter execution order;
- attribute layout;
- spawn/update operations;
- renderer outputs/material dependencies;
- event/sub-emitter dependencies when supported; and
- capability requirements.

The IR contains engine-owned operations/data, not closures, trait objects,
`wgpu` handles, or Editor widgets. That constraint is what makes a future GPU
backend possible without changing the authoring file.

Compilation diagnostics retain module/emitter stable IDs so the Editor can
highlight the exact failing row/module.

### 6. CPU is the reference backend; GPU simulation is an implementation backend

The first advanced implementation extends the existing deterministic CPU
simulation. It is the reference semantic backend used by headless tests and
small effects.

A future GPU backend may execute effects whose compiled operations are GPU
compatible. Backend selection is runtime/capability policy, not persisted effect
meaning. Effects that require unsupported operations receive a structured
capability diagnostic or use the CPU backend when allowed; the engine does not
silently drop modules.

Render-runtime remains the VFX runtime owner under ADR 0113. `wgpu` compute/
render implementation stays behind the existing GPU feature boundary.

### 7. Define an intentional first feature set and extension points

The first advanced authoring release includes:

- continuous rate and burst emission;
- point/box/sphere/cone spawn shapes;
- lifetime, initial velocity, gravity/forces/drag;
- color/size/rotation over lifetime curves;
- billboard and mesh rendering;
- texture-sheet animation;
- deterministic seed handling; and
- per-emitter maximum-particle/per-effect budget diagnostics.

The IR/module registry is designed for, but the first release need not include:

- collision and collision events;
- sub-emitters;
- ribbons/trails if they cannot share the first renderer cleanly;
- depth/soft-particle interaction;
- GPU compute simulation; or
- vector-field/fluid simulation.

Those features are added as typed modules/render outputs, not by adding generic
"script this particle" callbacks that the compiler cannot reason about.

### 8. The VFX Builder is a first-class authoring workspace

The Editor adds a VFX Builder for `*.vfx.json` assets with:

- emitter hierarchy/list with enable, duplicate, reorder, and rename controls;
- categorized/searchable Add Module palette driven by the shared module schema
  registry;
- phase-separated module stack with drag reorder where order is meaningful;
- schema-driven property Inspector;
- purpose-built curve and gradient editors;
- a dedicated preview viewport using the same render/runtime implementation;
- Play, Pause, Restart, Step, speed, and deterministic seed controls;
- shape gizmos and bounds in the preview;
- live particle count, spawn rate, estimated/max capacity, and renderer stats;
- compile/performance diagnostics inline and in Problems; and
- templates such as spark, smoke, burst, and trail that create ordinary VFX
  documents through the shared authoring service.

Templates are data-generation conveniences, not special runtime effect types.

### 9. Preview is deterministic and shares runtime semantics

Restarting an effect with the same asset, seed, and time sequence produces the
same CPU result. Editor preview uses an explicit preview clock and can rebuild
from zero for seeking. To keep scrub latency bounded, the implementation may add
preview checkpoints, but checkpoints are transient Editor cache data and do not
alter runtime simulation semantics.

Preview must reuse ADR 0072's persistent Scene View/preview-resource approach
where the effect is displayed in scene context. Editing one curve/module should
recompile/restart only the affected VFX preview, not rebuild unrelated scene
content.

A simulation that cannot meaningfully reverse is not faked by integrating with
negative delta. Scrubbing backward restores a checkpoint or restarts and
replays forward deterministically.

### 10. Authoring, CLI, and MCP use one transactional VFX service

The GUI-free service owns:

- schema/catalog discovery;
- document inspect/validate;
- granular emitter/module/curve commands;
- transactional apply/undo semantics;
- compile and diagnostics; and
- deterministic canonical save.

The Editor never edits raw VFX JSON directly as its business-logic path. Per
ADR 0121, MCP/CLI adapters can expose the same semantic operations without
recreating VFX rules.

Expensive compile/shader/pipeline work is debounced/cached off the immediate UI
edit path according to ADR 0104. Cheap property/schema errors remain immediate.

### 11. Runtime budgets and diagnostics are part of the design

Each effect has explicit authored or derived limits so a typo cannot allocate an
unbounded particle pool. Runtime statistics expose at least live count, spawn
count, dropped/capped spawns, and selected backend.

The Editor warns when authored rates/lifetimes imply a capacity above the
configured budget. Packaged runtime enforces the cap deterministically rather
than attempting an unbounded allocation.

### 12. Implementation is staged from shared semantics outward

Implementation order is:

1. VFX document/module schema, shared primitives, commands, and compiler IR;
2. CPU `VfxInstance` backend and simple ParticleEmitter-to-IR adapter;
3. billboard/mesh output plus burst/shapes/curves/texture-sheet feature set;
4. `engine.vfx_player` scene conversion and project gameplay start/stop/restart
   commands/views;
5. VFX Builder, preview clock, curve/gradient editors, gizmos, and stats; and
6. collision/trail/GPU modules only after profiling and capability tests justify
   each extension.

## Verification

The accepted implementation must prove:

- same asset/seed/time sequence produces deterministic CPU particle state;
- simple ParticleEmitter and its equivalent generated VFX effect share runtime
  semantics;
- module ordering is deterministic and invalid phase combinations are rejected;
- curve/gradient evaluation matches between runtime and Editor preview;
- burst/rate combinations obey the configured particle cap;
- backward Editor scrub reconstructs a deterministic state rather than applying
  negative-time simulation;
- VFX compile diagnostics map to stable module/emitter IDs;
- VFX edits use shared transactions and round-trip canonically;
- Editor preview recompiles only the affected effect where practical; and
- packaged Player and Editor Play load the same compiled effect meaning.

The VFX Builder, preview, gizmos, curve/gradient controls, and effect rendering
require Visual Validation when implemented. Performance tests should record CPU
simulation cost and draw/instance counts for representative effect budgets.

## Consequences

Simple particles stay easy, while complex effects gain an asset model and
dedicated tool instead of turning one component into a giant property bag. A
backend-neutral compiled IR keeps GPU simulation and richer render outputs open
without forcing those costs into the first implementation.

The engine gains a new persisted asset/document type and a more substantial
Editor workspace. That is deliberate: VFX authoring is a content-production
workflow, not merely another ECS component.

## Alternatives Considered

### Keep extending `ParticleEmitter` with fields

Rejected. Multiple emitters, curves, render modes, collision, and sub-emitters
would create a monolithic component and an unusable generic Inspector.

### Build a fully general node graph immediately

Rejected for the first advanced system. Most particle effects follow ordered
spawn/update/render stages; a typed module stack is easier to validate, compile,
and author. A future graph can target the same IR if more expressive dataflow is
needed.

### Implement GPU simulation first

Rejected. It would make correctness and headless testing depend on GPU
capabilities before the authoring/runtime semantic model is stable. The CPU
backend is the deterministic reference; GPU is an optimization backend.

### Create separate preview-only effect code

Rejected. It would drift from packaged results. Preview owns a clock and caches,
not a second simulator.

## Compatibility and Migration

The current ParticleEmitter capability remains intentionally supported as the
simple authoring surface and is routed through the new runtime IR; this is a
product feature, not a legacy compatibility parser. The new `engine.vfx_player`
and `*.vfx.json` schema are additive current contracts.

If the existing particle component schema is reshaped while introducing the
adapter, its current version and in-repository scenes/tests are updated together
under ADR 0115. No persisted GPU/backend representation is introduced. Public
`engine` facade paths continue to re-export the owning render-runtime types per
ADR 0113.
