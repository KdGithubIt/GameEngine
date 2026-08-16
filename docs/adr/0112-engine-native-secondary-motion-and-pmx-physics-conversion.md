# ADR 0112: Engine-Native Secondary Motion and Best-Effort PMX Physics Conversion

Status: Accepted
Date: 2026-08-14
Supersedes: ADR 0108, ADR 0109
Amends: ADR 0096, ADR 0097
Builds on: ADR 0106

## Context

The engine currently treats PMX rigid bodies and joints as a runtime MMD
compatibility contract. ADR 0096 introduced an isolated Rapier world per MMD
character, ADR 0108 translated Bullet/PMX damping, limits, springs, gravity, and
solver iteration semantics into Rapier, and ADR 0109 added clip-history
pre-roll so an animation seek could reconstruct a state closer to normal MMD
playback.

That work is technically coherent, but it optimizes for a product goal the
engine no longer needs to own. The useful game-development value of MMD assets
is primarily their models, morphs, and authored motions, especially VMD
animation. A general game engine does not need to promise that a PMX skirt,
hair chain, or accessory reproduces MikuMikuDance/Bullet secondary motion
exactly in order to make those assets useful.

At the same time, hair, skirts, coats, ribbons, tails, and accessories are not
MMD-specific needs. FBX, glTF, VRM, and manually authored rigs need the same
class of post-animation cosmetic motion. Keeping that capability inside an
`mmd_physics` domain makes a generally useful engine feature depend on one
source format and encourages source-format semantics to leak into runtime
architecture.

ADR 0097 already chose the opposite boundary for VMD animation: MMD-specific
FK/IK/appended-parent evaluation is consumed at import time and baked into a
normal `AnimationClip`, so the runtime animation stack does not know that the
clip came from VMD. ADR 0106 likewise already provides a format-independent
pose-composition boundary: stateful physics modifiers read a resolved pose and
write the `RigPose` physics layer without feeding the previous final pose back
into animation.

The same principle should apply to cosmetic secondary motion. PMX physics data
is valuable evidence of author intent: it identifies bones that should move,
collider shapes and offsets, constraint topology, collision groups, and tuning
values. It does not need to remain the runtime contract.

## Decision

### 1. Secondary Motion is an engine-native, source-format-independent feature

GameEngine will define **Secondary Motion** as the standard stateful pose
modifier for cosmetic skeletal motion such as hair, skirts, coats, ribbons,
tails, and accessories.

Its runtime and authored representation MUST NOT depend on PMX, VMD, MMD,
Bullet, or any other source-format-specific type. Conceptually the pipeline is:

```text
VMD / FBX / glTF / other animation
                |
                v
          AnimationClip
                |
             Animator
                |
                v
       RigPose Animation
                |
       procedural modifiers
                |
                v
        Secondary Motion
                |
                v
        RigPose Physics
                |
                v
           Final Pose
```

VMD remains an import source for ordinary `AnimationClip` data under ADR 0097,
ADR 0098, ADR 0099, and ADR 0110. Secondary Motion does not inspect which
format produced the animation.

The engine-native rig is referred to conceptually in this ADR as
`SecondaryMotionRigAsset`. The implementation may choose the final Rust type
and serialized selector names, but the model MUST be format-independent and
capable of describing at least:

- stable skeleton/bone bindings;
- simulation nodes or bodies;
- collider shapes and bone-local offsets;
- constraint topology and angular/linear limits;
- mass or equivalent weighting;
- stiffness and damping;
- gravity influence;
- simulation/follow-bone intent; and
- collision groups or equivalent filtering.

Backend-specific Rapier handles, Bullet objects, solver snapshots, and
source-format indices MUST NOT be part of the authored contract.

### 2. Hair, skirt, ribbon, and accessory concepts are authoring presets, not separate solvers

The engine SHOULD expose high-level authoring operations such as Hair Chain,
Skirt, Ribbon, Tail, or Accessory when those improve editor usability. Those
operations generate or edit the same generic Secondary Motion rig.

The runtime MUST NOT grow independent `HairPhysics`, `SkirtPhysics`, and
`RibbonPhysics` solver architectures merely because the authoring workflows
differ. A skirt may need a lattice of vertical and lateral constraints while a
hair chain may need a simple chain, but both are configurations of the same
secondary-motion domain.

This keeps the public mental model centered on author intent while allowing the
solver representation to remain generic.

### 3. PMX rigid bodies and joints become best-effort import hints

PMX import continues to parse rigid bodies and joints, but their role changes
from "runtime MMD physics definition" to "input for generating an
engine-native Secondary Motion rig."

The converter SHOULD preserve source intent where a stable correspondence
exists:

| PMX information | Secondary Motion treatment |
| --- | --- |
| Bone binding | Preserve as the target bone binding |
| Body shape and offset | Use as collider shape/offset input |
| Joint graph | Use as constraint topology |
| Follow-bone / dynamic mode | Use as simulation intent |
| Collision group and mask | Map to secondary-motion filtering |
| Linear/angular limits | Approximate with engine limits |
| Mass | Use as an initial mass/weight value |
| Damping | Convert to an engine tuning starting point |
| Spring values | Convert to an engine stiffness starting point |

The converter is explicitly **best effort**. It MAY simplify, approximate, or
drop source constructs that do not map cleanly. Unsupported or lossy conversion
MUST produce structured, actionable diagnostics and MUST NOT fail the entire PMX
model import when mesh, skeleton, material, morph, and animation-relevant data
remain usable.

The importer SHOULD produce a normal engine-native Secondary Motion sub-asset,
not raw PMX/Bullet state required by packaged runtime code.

### 4. MMD/Bullet physics fidelity is not a runtime compatibility contract

GameEngine does not promise that a converted PMX rig reproduces
MikuMikuDance/Bullet secondary motion numerically or visually.

In particular, Secondary Motion is not required to preserve:

- Bullet's exact damping equation;
- Bullet's spring/motor semantics;
- Bullet's default solver iteration count;
- MMD-specific gravity constants;
- Bullet-specific locked/free-axis edge cases;
- exact body velocities or constraint impulses from a reference MMD runtime; or
- exact hair/skirt state after seeking to a VMD frame.

PMX values may still inform useful defaults, but they are interpreted as source
authoring hints rather than a solver-compatibility specification.

This supersedes ADR 0108's requirement to map PMX/Bullet semantics onto Rapier
with fixed-step-equivalent formulas and solver settings.

### 5. Secondary Motion keeps the existing RigPose and gameplay-isolation boundaries

ADR 0106 remains the pose ownership contract. Secondary Motion is a stateful,
world-aware modifier outside the pure Pose Graph. It reads the resolved
pre-physics pose and writes the `RigPose` physics layer. It MUST NOT directly
write skeletal joint `Transform` components as authoritative simulation state.

Secondary Motion is cosmetic by default and MUST remain outside gameplay
physics semantics:

- its bodies MUST NOT appear in gameplay `CollisionEvents`;
- they MUST NOT become hitboxes or character-controller obstacles merely by
  existing;
- they MUST NOT be returned by ordinary gameplay spatial queries; and
- they MUST NOT apply solver impulses back into gameplay rigid bodies.

An implementation MAY allow Secondary Motion to *read* selected gameplay/world
collision geometry so cloth or hair can react to a wall or character body, but
that interaction must be one-way unless a later ADR explicitly promotes
Secondary Motion into gameplay simulation.

The current per-character isolated Rapier world from ADR 0096 is therefore a
valid initial implementation technique, but the isolation is the contract, not
the Rapier object layout. Gameplay collision/dynamics remain on Rapier under
ADR 0096.

### 6. Timeline discontinuities reset Secondary Motion; MMD seek pre-roll is removed

Secondary Motion version 1 uses generic discontinuity semantics rather than
reconstructing source-format-specific historical physics.

The required behavior is:

| Timeline change | Secondary Motion behavior |
| --- | --- |
| Normal continuous playback | Continue simulation |
| Positive-duration crossfade | Continue simulation |
| Explicit seek | Reseat to the current resolved pose and clear dynamic history |
| Loop wrap | Reseat and clear dynamic history |
| Instant state switch | Reseat and clear dynamic history |
| Teleport / respawn / scene discontinuity | Reseat and clear dynamic history |

"Clear dynamic history" includes zeroing or otherwise resetting solver velocity
and cached state so an instantaneous pose change is not interpreted as motion.

The runtime no longer needs to distinguish an `Animator::seek` from another
generic reposition *for the purpose of Secondary Motion reconstruction*.
Historical `AnimationClip` sampling, damping-derived pre-roll horizons, and
forward-seek solver-history reuse from ADR 0109 are not part of the new
Secondary Motion contract.

If editor UX later requires a natural-looking state after timeline scrubbing,
that feature must be designed as a source-format-independent Secondary Motion
warm-up/reconstruction policy. It must not reintroduce a VMD/MMD-only runtime
path.

This supersedes ADR 0109.

### 7. Fixed-step simulation and presentation interpolation remain separate

The useful, format-independent part of ADR 0108 is retained: authoritative
Secondary Motion runs on the fixed-step schedule, while rendering MAY
interpolate between the previous and current completed post-secondary-motion
poses.

Presentation interpolation MUST NOT feed interpolated transforms back into
`RigPose` or the solver. A discontinuity/reset sets both presentation samples
to the same resolved pose so rendering cannot interpolate across unrelated
timeline states.

This is a general presentation-quality feature, not an MMD-fidelity feature.

### 8. PMX-generated rigs are editable starting points, not opaque imported behavior

PMX import SHOULD make a successfully generated Secondary Motion rig available
as an imported sub-asset. Because conversion is approximate, importing a PMX
MUST NOT silently assert that the generated rig is MMD-compatible.

The common authoring flow should be:

```text
PMX rigid bodies / joints
          |
          v
best-effort conversion + diagnostics
          |
          v
generated Secondary Motion rig
          |
     user opts in
          |
          v
      simulation
```

The generated rig SHOULD NOT be automatically enabled on every imported
character merely because the PMX contained physics records.

The editor SHOULD provide a way to create an independently authored/local copy
of an imported generated rig before the user performs durable tuning that must
survive source reimport. Reimported generated data remains derived from the
source; user-authored tuning must not be silently overwritten by a later PMX
reimport.

FBX, glTF, VRM, or manually built characters must be able to author the same
Secondary Motion asset without passing through PMX at all.

### 9. The solver backend is an implementation detail

The first implementation SHOULD reuse the existing Rapier dependency and as
much of the current isolated MMD solver infrastructure as remains appropriate.
That avoids introducing a second physics dependency merely to rename the
feature.

However, Secondary Motion assets and authoring APIs MUST be expressed in
engine-owned concepts. A future change from Rapier to another solver, or to a
specialized secondary-motion solver, must not require changing source-format
assets solely because backend parameter conventions changed.

Bullet integration is therefore not required for PMX support under this
decision.

## Consequences

- MMD remains useful as a model, morph, and motion asset ecosystem without
  making MMD physics fidelity a permanent GameEngine requirement.
- VMD motion continues to become an ordinary `AnimationClip`; there is no MMD
  runtime animation path.
- Hair, skirts, and other cosmetic motion become reusable engine functionality
  for non-MMD characters.
- The large amount of MMD-specific solver emulation in ADR 0108 and historical
  seek reconstruction in ADR 0109 can be removed during implementation.
- PMX physics data is still useful: it can bootstrap a secondary-motion setup
  instead of being discarded.
- Converted PMX rigs can look different from MikuMikuDance. That difference is
  expected and must not be reported as an engine compatibility defect by
  itself.
- Secondary Motion stays isolated from gameplay collision/event semantics,
  preserving predictable character control and gameplay queries.
- Fixed-step presentation interpolation can survive the migration because it
  solves a general rendering problem rather than an MMD compatibility problem.
- Authoring gains a general asset and editor workflow that will need explicit
  schemas, diagnostics, extraction/copy semantics, and reusable presets.
- Runtime reset semantics become simpler and no longer require replaying
  historical animation during a seek.

## Alternatives Considered

### Continue pursuing MMD/Bullet fidelity on Rapier

Rejected as the product contract. ADR 0108 demonstrated that meaningful
compatibility requires translating solver-specific damping, limits, springs,
gravity, iteration counts, and other semantics. That work is justified only if
matching MMD playback is itself a product goal. It is not required for using
PMX/VMD assets effectively in games.

### Use Bullet for MMD only

Rejected. A Bullet-specific MMD backend would preserve a second solver domain,
C++/WASM integration surface, and source-format-specific runtime path solely to
provide fidelity this ADR no longer promises.

### Use Bullet for all GameEngine physics

Rejected by this decision because the original motivation was MMD/Bullet
behavioral compatibility. Once MMD physics is an import hint rather than a
runtime contract, replacing the existing Rust/Rapier gameplay stack would add
migration and FFI/Web build cost without solving a required product problem.
ADR 0096's gameplay Rapier decisions remain in force.

### Ignore PMX physics data completely

Rejected. The data contains useful author intent: affected bones, body shapes,
offsets, topology, limits, collision filtering, and tuning values. Treating it
as best-effort conversion input gives imported models a useful starting point
without binding runtime behavior to MMD.

### Implement separate hair, skirt, and accessory solvers

Rejected as the architectural default. They are distinct authoring patterns,
not necessarily distinct simulation domains. Presets that generate one generic
constraint rig provide better reuse and avoid parallel runtime systems.

### Keep ADR 0109 pre-roll only for VMD

Rejected. It would preserve an MMD-only runtime special case after the rest of
the secondary-motion system became source-format-independent. A future warm-up
feature must apply to ordinary `AnimationClip` playback regardless of whether
the clip originated from VMD, FBX, glTF, or another format.

## Compatibility and Migration

The implementation of this ADR intentionally changes the Secondary Motion
authoring/import identity instead of reinterpreting the former MMD-specific
contract in place. The current contract is:

- runtime and asset APIs use `SecondaryMotion`, `SecondaryMotionRigAsset`, and
  `SecondaryMotionRigRegistry`; the ADR 0111 `engine::rigid_body_rig` umbrella
  module remains a supported facade but exposes the current types only;
- the authoring component ID is `engine.secondary_motion`, schema version 1,
  with one `rig` asset reference constrained to a Secondary Motion Rig; the
  field may remain unassigned while editing, and assigning it is the explicit
  opt-in to simulation;
- a generated model rig is catalogued as imported sub-asset kind
  `secondary_motion_rig`; its deterministic ID uses derivation prefix
  `secondarymotionrig` and selector index 0;
- the former `RigidBodyRig` / `rigid_body_rig` imported kind and
  `rigidbodyrig` derivation namespace are not aliases for the current kind. A
  persisted reference to an ID derived under that old namespace is stale and is
  not silently reinterpreted as a Secondary Motion Rig; and
- VMD-derived `AnimationClip` behavior and IDs remain unchanged by this
  migration.

The migration preserves PMX mesh, skeleton, material, morph, and other
non-physics import behavior. PMX physics records are converted best-effort into
engine-native data with structured diagnostics, while the MMD-specific solver,
seek pre-roll, and Bullet-equivalence runtime contract are removed.

This explicit identity cutover follows ADR 0115's current-format-only baseline
and ADR 0091's rule against permanent compatibility surfaces. The canonical
editable component schema is recorded in `docs/AI_FRIENDLY_AUTHORING_SPEC.md`.

ADR 0096 remains authoritative for gameplay Rapier collision/dynamics and the
custom character-controller boundary; its MMD-specific §5 contract is amended
by this ADR. ADR 0097 remains authoritative for PMX model import and VMD baking;
its MMD-rigid-body runtime contract is amended by this ADR. ADR 0106 remains
authoritative for pose ownership and fixed-step ordering. ADR 0108 and ADR 0109
remain historical records of the superseded MMD-fidelity implementation.
