# ADR 0108: MMD Secondary-Motion Constraint and Unit Semantics

Status: Superseded by ADR 0112
Date: 2026-08-14

## Context

ADR 0096 §5 gives every imported MMD character an isolated Rapier world for
secondary motion, ADR 0097 defines the PMX import that populates it, and
ADR 0106 defines the pose layer its result is written to. ADR 0096 was
extended so a discontinuous animation pose (a looping clip wrapping to its
first frame, a seek, a teleport) reseats the solver instead of being driven
into it as velocity. That removed one specific divergence.

What remained is not fidelity but correctness: driven by ordinary motion, the
rig came apart. The cause is that PMX rigid-body data is authored for one
solver, in one set of units, and was handed to a different solver through the
nearest-looking parameter each time. Six separate mismatches compounded.

### Measurements

Values below were measured against
`examples/miku_test/assets/meshes/YYB Hatsune Miku_NT_1.0ver.pmx` — 284 bodies
(206 dynamic) and 318 joints — driven by a whole-body sway of ±0.5 rad at 1 Hz
for 3600 fixed steps, with every bone position recorded in the character's own
frame. The character's rest pose spans 2.06 m.

| | before | after |
|---|---|---|
| widest pose the rig reached | 580.6 m | 2.45 m |
| worst joint separation error | 579.1 m | 0.056 m |
| worst per-step bone displacement | 30.0 m | 0.34 m |

The authored constants that produced it:

- mass 0.01–100 kg (median 1.0); rotational inertia 2e-6 – 0.256 kg·m²
- linear damping median 0.999, and ≥ 1.0 on 38 of 284 bodies
- angular damping median 0.9999, and ≥ 1.0 on 106 of 284 bodies
- of 954 linear axes, 694 are locked and 260 ranged; of 954 angular axes,
  767 are locked and 187 ranged; none are free
- no locked axis is pinned anywhere but its frame origin
- 69 translation springs, all at 36900; 234 rotation springs at 3.0–30.0
- **239 of those 303 springs sit on an axis PMX locks**

### 1. Damping means something different in each solver

Bullet — the solver every MMD runtime drives PMX data with — clamps the
authored damping factor to `0..=1` and removes that *fraction* of the velocity
per second: `v *= (1 - d)^dt`. Rapier integrates a damping *rate*:
`v *= 1 / (1 + dt * rate)`. `crate::mmd_physics` passed the authored number
straight through.

The original correction used the continuous-time rate `-ln(1 - d)`. That is
the limit as the timestep approaches zero, but Rapier applies its rational
damping formula once per fixed step. Exact fixed-step equivalence instead
requires `rate = ((1 - d)^(-dt) - 1) / dt`. The difference is small in position
but material in residual velocity for this model's unusually high damping:
at 60 Hz, the continuous approximation leaves about 1.45e-3 after one second
for authored `0.999` instead of Bullet's 1.0e-3, and about 1.90e-4 for authored
`0.9999` instead of 1.0e-4. That residual energy presents as a fine oscillation
after the large divergence has already been fixed.

This is the single largest error. Correcting only this one takes the rig from
580 m to 3.20 m.

### 2. Limit semantics

Bullet reads a 6-DOF limit range as three distinct states: `lower == upper`
locks the axis, `lower > upper` frees it entirely, and `lower < upper` limits
it. `crate::mmd_physics` built every joint as
`GenericJointBuilder::new(JointAxesMask::empty())` and expressed the whole
authored range through `limits()`, which is a soft inequality constraint. No
axis was ever locked, and a "free" axis would have become a range whose minimum
exceeds its maximum.

For this model that is not an edge case: 1461 of 1908 axes are the locked case.

### 3. The spring was applied where MMD never applies it, at the wrong strength

Two errors sit on top of each other here.

Bullet resolves a locked axis through its equality limit, and that limit
overrides the spring motor sharing the axis — so a spring authored on a locked
axis never acts in MMD. 239 of this model's 303 springs are authored exactly
that way. With nothing locked (§2), those springs were the *only* thing holding
those axes, which is how a rig came to depend on constants its author never
intended to act.

Independently, the strength was wrong twice over. Rapier's default
`MotorModel::AccelerationBased` divides the authored constant back out by the
body's own mass and inertia — the exact quantity a PMX author tuned it against
— so one authored number produced wildly different behavior across a rig whose
inertias span five orders of magnitude. And an angular spring constant is a
torque per radian: it carries one factor of the import scale for force and one
for length, so simulating in meters requires multiplying by
`PMX_TO_METERS²`. (A translation spring is a force per length, where the two
conversions cancel and the authored number passes through unchanged.)

### 4. Gravity

Reference MMD runtimes step Bullet in PMX units under MMD's own gravity of
`9.8 × 10` (babylon-mmd uses exactly this; three.js's `MMDPhysics` rounds it to
`10 × 10`). At this engine's import scale that is 7.84 m/s², against the
9.81 m/s² Rapier defaults to.

This is a real error but a small one — 1.25× — and correcting it alone changes
nothing that matters (580.6 m becomes 462.6 m). It is included because the
authored constants have no other reference, not because it was visible.

### 5. Bodies with nothing to solve

A PMX joint that locks all six axes is a weld, and an MMD model uses that to
attach decoration rigidly to a limb. Each sleeve on this model is five bodies
welded to the elbow, which PMX marks as animation-driven; the sleeve therefore
has no degree of freedom at all, and Blender's reference playback moves it
rigidly with the arm.

Handing that to a solver is asking it to rediscover a pose that is already
known, and it gets it wrong: measured against the real scene, the weld broke
by 53 degrees in one fixed step, against an elbow that turned 13.3 degrees in
the same step. Contacts and continuous collision detection were both ruled out
by disabling them, and the joints were confirmed to be built as full six-axis
locks. It is simply not a constraint an impulse solver holds against a fast
kinematic anchor.

### 6. Solver iterations

Bullet's `btContactSolverInfo::m_numIterations` defaults to 10, and MMD
runtimes step it with that default; Rapier's default is 4. An MMD rig is
hundreds of near-rigid constraints deep, so this is the budget its constants
were tuned against. On this model it is worth the difference between locked
axes holding to 88 mm and to 56 mm, and between the rig reaching 3.29 m and
2.45 m under the same sway.

## Decision

Map each PMX concept onto the Rapier representation that means the same thing,
rather than onto the nearest-looking parameter, and convert the two quantities
whose units differ. Concretely, in `crate::mmd_physics`:

1. **Damping converts between the two discrete integrators at the active fixed
   timestep.** `((1 - clamp(d, 0, 1))^(-dt) - 1) / dt`, with the `d >= 1` case
   (which no finite rate expresses) standing in as a rate large enough to
   effectively erase velocity. If `FixedTime::fixed_delta` changes at runtime,
   existing MMD bodies are retuned before their next solver step.

2. **Limits map to representations.** `lower == upper` becomes a locked axis in
   `JointAxesMask`; `lower > upper` becomes an axis with neither lock nor
   limit; `lower < upper` becomes `limits()`.

3. **Springs map to force-based motors, in solver units, and only where MMD
   applies them.** A spring on a locked axis is not motorized. Elsewhere the
   motor is `MotorModel::ForceBased` with no damping term — Bullet caps this
   motor's force at the Hooke force itself, so what a PMX spring applies is an
   undamped restoring force, and dissipation belongs to the per-body damping in
   §1. Rotation stiffness is multiplied by `PMX_TO_METERS²`; translation
   stiffness passes through.

4. **Gravity is MMD's**, expressed at the import scale.

5. **A body animation fully determines is driven, not simulated.** A PMX
   joint that locks all six axes welds its two bodies together, so a body
   welded to one that follows a bone follows that bone too, transitively. Such
   a body has no remaining degree of freedom: no simulation can produce motion
   its author authorised, and solving it anyway can only add error.

   This is not a tuning choice. On this project's model, PMX welds all five
   bodies of each sleeve to the elbow on every axis — Blender's reference
   playback moves them rigidly with the arm, exactly as the data says.
   Simulating them as dynamic bodies broke the weld by **53 degrees in a
   single fixed step** during an ordinary dance, four times what the elbow
   itself turned in that step, with contacts and continuous collision
   detection both ruled out by measurement. Rapier does not hold a six-axis
   lock against a fast kinematic anchor at any iteration count worth paying
   for, and it does not have to: the answer was already known. Driving those
   bodies instead takes the sleeve from 56.4° off its arm to 0.079°, and the
   physics layer stops writing to those bones at all.

   148 of this model's 284 bodies turn out to be determined this way — 78
   declared by PMX and 70 proved by welds — which is that much of the solver's
   work removed as well.

6. **The solver runs Bullet's ten iterations**, not Rapier's four.

7. **Presentation interpolates, simulation does not.** The isolated MMD world,
   animation sampling, pose layers, collision, and write-back remain
   authoritative at the fixed timestep. After each fixed MMD step the bridge
   retains the previous and current complete post-physics local rig poses.
   During the per-frame Update schedule, presentation interpolates those two
   samples using `FixedTime`'s unconsumed accumulator fraction and writes only
   the published joint `Transform` compatibility surface before normal world
   transform propagation and skinning.

   This deliberately adds one fixed-step of presentation latency. It does not
   feed interpolated joints back into `RigPose` or Rapier, so the solver never
   integrates a render-only pose. A rebuild, seek, loop wrap, teleport, or
   other declared discontinuity resets both presentation samples to the same
   pose so interpolation cannot smear across unrelated timelines.

The rig stays a meter-space asset (ADR 0097 §6) and the solver keeps stepping
in meters. `crate::mmd_physics` holds its own copy of the import scale, because
it compiles on targets `crate::pmx_import` does not (ADR 0096 §1); a test in
`pmx_import` pins the two together.

## Consequences

- PMX-authored secondary motion stays attached to the character under ordinary
  motion, and behaves comparably to what the model author saw in MikuMikuDance,
  which is the only reference the authored constants have.
- No serialized format, stable ID, authoring schema, public API, or imported
  asset changes. `RigidBodyRigAsset` keeps its schema version and its meters.
- Ten solver iterations cost about 2.5× the constraint work of four, per
  character, inside a world that is already isolated per ADR 0096 §5.
- The import scale now appears in two modules. The pinning test is what keeps
  them from drifting; without it, changing one would silently retune every
  imported rig's gravity and rotation springs.
- The scale factor is PMX-specific, and `rigid_body_rig` is `None` for every
  glTF and FBX source today (ADR 0097 §6). If a non-PMX source ever produces a
  rig, the asset will have to declare its authoring unit rather than have this
  factor assumed.
- Gameplay collision is unaffected: the solver stays isolated, and nothing
  outside the bridge observes these constants.
- Roughly half this model's bodies stop being simulated, so a rig that leans
  heavily on welds costs proportionally less. A rig with no welds is unchanged.
- A body the closure proves bone-driven no longer writes to its bone at all,
  which is correct — the pose it would write is the one already there — but it
  does mean a future debug draw of "simulated bodies" should distinguish the
  two, or it will look as though those bodies vanished.
- Rendering no longer exposes the fixed 60 Hz pose as a staircase on displays
  that present more frequently. The extra storage is two local `Transform`
  values per skeleton joint per simulated MMD character; authoritative
  fixed-step behavior and gameplay collision timing are unchanged.

## Validation

Each mapping in the Decision has a unit test in `crate::mmd_physics` asserting
the Rapier representation it produces: that an empty PMX range locks the axis
rather than limiting it, that an inverted range leaves it unconstrained, that a
proper range becomes a limit, that a spring becomes a force-based motor at the
converted stiffness, that a spring on a locked axis is not motorized, that the
damping conversion reproduces Bullet's one-second decay, that a body falls
under MMD's gravity rather than Rapier's, and that a weld chain reaching an
animation-driven body makes every body on it animation-driven while a hinge
stops the closure.

The damping regression also asserts Bullet and Rapier retain the same velocity
over one actual fixed step for representative authored values, including
`0.999` and `0.9999`. Fixed-time and pose interpolation have focused tests for
the accumulator fraction and local translation/rotation/scale interpolation.

Behavior over time is guarded by a case that drives a PMX-shaped chain — every
translation axis pinned, springs on pinned axes, real hair damping — through a
swaying root for 900 steps and asserts it stays within its own reach. That is a
divergence check, not a fidelity one: a short chain cannot reproduce what a
whole rig does, and the divergence this ADR exists for needed all 284 bodies
and 318 joints. The whole-rig numbers in Context were produced by a temporary
harness over the project's PMX source, not by a committed test, because that
source is a 23 MB asset and the run takes two minutes.

What no test covers is whether the result *looks* like MikuMikuDance. The
derivations above are what stands behind that claim; confirming it needs the
model on screen.

## Alternatives Considered

**Simulate in PMX units** — step the isolated world at the scale the constants
were authored at, converting only the pose at the boundary. Attractive because
mass, gravity, and both spring constants would then need no conversion at all.
Rejected: the rig asset is in meters, so every length in it — collider sizes,
bone offsets, joint frames, translation limits — would need converting on the
way in, plus the pose stream both ways on every step, which is more conversions
than the two constants this decision touches and puts them on the hot path.
It also leaves collider sizes and joint frames unreadable against meter-space
intuition when debugging. Neither variant avoids §1, §2, §3's motor model, or
§5, which are semantic rather than dimensional.

**Retune the PMX constants at import** — bake corrected stiffness and damping
into `RigidBodyRigAsset`. Rejected: it changes a serialized asset to encode a
runtime solver's parameter conventions, so a solver change would invalidate
already-imported rigs, and the stored values would no longer match what the
model author sees in PMX Editor.

**Rescale the engine so PMX imports at 1:1** — removes the unit mismatch by
deleting the scale. Rejected: gameplay, cameras, physics, navigation, and every
non-PMX asset are authored in meters, and PMX would be the only reason to move
them. It also addresses only §3's scale factor and §4.

**Accept the current behavior** — rejected. It is the defect this ADR exists to
record, and it is visible on the project's own model.

## Compatibility and Migration

No persisted data, public API, authoring command, diagnostic, or build artifact
changes. `RigidBodyRigAsset` keeps its schema version, its meter units, and its
stable IDs, so no reimport is required.

The change is observable only as different secondary motion for existing PMX
characters, which is the intent. Scenes, prefabs, manifests, and PMX and VMD
sources are untouched.
