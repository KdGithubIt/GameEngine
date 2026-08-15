# ADR 0096: Rapier Physics Adoption and the MMD Rigid-Body Bridge Boundary

Status: Accepted
Date: 2026-08-12

Amendment: ADR 0112 replaces §5's MMD-fidelity bridge with source-format-
independent Secondary Motion and supersedes ADR 0109's seek pre-roll contract.
The gameplay Rapier decisions in §1–4 and the custom character-controller
boundary remain in force.

## Context

Every physics-shaped system in the engine is hand-rolled today:

- `physics.rs` — `Velocity`/`Gravity`/`GravityScale` and three fixed-step
  systems (`gravity_system`, `velocity_system`, `restitution_system`)
  integrating `PhysicsBody::Dynamic` entities with a hand-written reflection
  formula for restitution.
- `collision.rs` — AABB/Sphere/CapsuleY shapes with hand-written pairwise
  overlap math (`world_shapes_overlap`) and an O(n²) sweep-and-prune broad
  phase (`collision_detection_system`), producing `CollisionEvents` /
  `CollisionTransition` / `CollisionLayers` / `TriggerVolume`.
- `character_controller.rs` — `KinematicCharacterController`, a hand-tuned
  move-then-resolve loop (substepping, slope limit, step offset, ground
  snap) built directly on `collision.rs`'s shape queries.

No `rapier`/`bullet`/`physx`/`jolt` dependency exists anywhere in the
workspace. This was sufficient through Phase 57 because the only physics
consumers were simple gameplay props and the character controller.

The immediate driver is an MMD (PMX/VMD) model import ambition explored
outside the M1 roadmap: a PMX character's "physics" (jiggle hair, skirt
panels) is not cloth/soft-body simulation — it is rigid-body capsules
connected by 6-DOF spring joints (a ragdoll-style chain), whose simulated
transforms are written back onto the character's bones each frame. This
model requires a real rigid-body + joint solver, which nothing in this
engine has. The Rust crate family `mmd-anim-runtime` / `mmd-anim-format`
(alpha, active development as of 2026-08) already evaluates PMX/VMD bone
hierarchies, MMD-style IK, and appended-parent (付与親) inheritance, and
ships an optional `mmd-anim-physics-bullet` backend for the rigid-body
layer. Bullet is a C++ FFI dependency; this engine has an existing wasm32
build target (`docs/phases/phase-46-wasm.md`, ADR 0041) and already gates
FBX import off wasm32 specifically because `ufbx`'s C compilation step
does not target `wasm32-unknown-unknown` (ADR 0081 §2). Adding a second,
larger C++ physics dependency with the same wasm32 gap is a worse trade
than adopting a pure-Rust alternative.

Once a rigid-body/joint solver enters the dependency graph for the MMD
bridge, the question of whether it should also absorb the engine's
existing hand-rolled `physics.rs`/`collision.rs` math is unavoidable —
maintaining both a real solver and parallel hand-written AABB/sphere/
capsule overlap code for the same category of problem is the kind of
duplication the project's own guidance says to avoid. This ADR settles
that boundary before any MMD-specific work (PMX/VMD parsing, IK
integration, morph rendering, skin splitting for `MAX_JOINTS`) begins;
those remain separate, later decisions.

## Decision

### 1. Dependency: `rapier3d` (features `dim3`, `f32`)

- **Why rapier over Bullet**: pure Rust, compiles to `wasm32-unknown-
  unknown` without a C toolchain step, and is the de-facto standard rigid-
  body engine in the Rust ecosystem (used throughout the Bevy ecosystem,
  1.4M+ crates.io downloads). Matches this engine's existing "pure Rust
  where the dependency allows it" posture better than any C++ FFI engine.
- **Why rapier over Avian/PhysX/Jolt**: Avian (XPBD) is also pure Rust and
  a reasonable alternative, but has no ready-made bridge from
  `mmd-anim-runtime`'s rigid-body/joint data the way this decision assumes
  for rapier's `RigidBodySet`/`ImpulseJointSet`/`GenericJoint` API; PhysX
  and Jolt are both C++ FFI with the same wasm32 gap as Bullet, and Jolt
  additionally lacks a mature, stable Rust binding. See Alternatives
  Considered.
- **Cost accepted**: `mmd-anim-physics-bullet` is not usable as-is — the
  MMD rigid-body/joint bridge (§3) is written against `mmd-anim-runtime`'s
  format/evaluation output directly, feeding rapier instead of Bullet.
  This is new code this engine owns, not a crate we get for free.
- **Determinism note**: ADR 0064's replay format "does not promise
  deterministic rendering or external I/O" — it records/replays input,
  not simulation output — so rapier's floating-point behavior does not
  violate that contract. `rapier3d`'s `enhanced-determinism` feature
  exists if cross-platform bit-exact simulation is ever required; not
  enabled by this ADR (perf cost, and nothing today depends on it).

### 2. `collision.rs` migrates to rapier's collision pipeline

`Collider` (`Aabb`/`Sphere`/`CapsuleY`), `WorldShape`, `CollisionLayers`,
`TriggerVolume`, `PhysicsBody`, and the public `CollisionEvents` /
`CollisionEvent` / `CollisionTransition` / `CollisionPhase` /
`CollisionStats` / `collisions_by_entity` contract are **unchanged**.
Internally, `collision_detection_system` stops calling
`world_shapes_overlap` and the hand-written broad phase, and instead
mirrors each entity's `Collider` into a rapier `ColliderSet` (rebuilt or
diffed per fixed step) and reads overlaps from rapier's narrow-phase
query. `world_shapes_overlap` and the pairwise math
(`sphere_vs_sphere`, `capsule_vs_aabb`, etc.) are deleted once the swap
lands — they exist today only to serve this system and
`character_controller.rs`'s obstacle resolve loop (§4).

Rationale: this is pure "detect whether two shapes overlap and by how
much" math with no gameplay-feel tuning attached (unlike §4), so replacing
hand-written geometry with a maintained, better-tested implementation is a
straightforward win. `CollisionStats`'s `proxy_count` /
`candidate_pair_count` / `narrow_phase_count` fields are preserved by
reading rapier's own broad/narrow-phase counts; exact numbers may shift
(rapier's broad phase is not sweep-and-prune-on-a-single-axis), so
`sweep_and_prune_rejects_far_apart_colliders_before_narrow_phase`'s exact
assertions are expected to need updating, not its intent.

### 3. `physics.rs`'s `Dynamic` bodies migrate to rapier `RigidBody`s

`Velocity`, `Gravity`, `GravityScale` stay as the public components/
resource — scripts, prefabs, and existing gameplay code read and write
them unchanged. Internally, every `PhysicsBody::Dynamic` entity gets a
backing rapier dynamic `RigidBody`; `gravity_system` no longer manually
integrates `Vec3` addition, and `restitution_system`'s hand-written
reflection formula is replaced by rapier's own restitution/friction
material properties on the collider. `PhysicsBody::Static` entities get a
rapier fixed collider (no rigid body). `PhysicsBody::Kinematic` entities
that are *not* a `KinematicCharacterController` (e.g. Phase 21 moving
platforms) get a rapier kinematic-position-based `RigidBody`.

### 4. `KinematicCharacterController` stays fully custom — no rapier rigid body, no rapier's own character controller

The move-then-resolve loop, substepping, slope limit, step offset, and
ground snap in `character_controller.rs` are **not** migrated to rapier
dynamics or to `rapier`'s own `KinematicCharacterController` helper. This
is a deliberate boundary, not an oversight: the existing tests assert
exact tuned outcomes of this specific algorithm (e.g. "resting center
settles near y=1.4", "stopped at x∈(1.0, 1.2)"), and action-game feel
(precise jump arcs, exact step-climb behavior, no physics-engine
sleep/wake latency) is easier to keep correct in code this project
controls outright than to reproduce through a general solver's tuning
knobs. The controller keeps querying collider shapes for its own resolve
loop; those shapes are now rapier colliders (§2), so it becomes a *reader*
of rapier's collider data, never a rapier rigid body.

### 5. New, isolated domain: MMD rigid-body/joint bridge

A new module (name TBD when the MMD import ADR is written) owns a
**separate** rapier `PhysicsPipeline`/`World` instance per skinned MMD
character — not shared with the gameplay world in §2–4. It is populated
from `mmd-anim-runtime`'s parsed PMX rigid-body shapes (sphere/box/
capsule, bone-local offset, mass, damping) and joints (6-DOF spring
constraints, rapier's `GenericJoint` with motor/limit configuration per
axis), stepped once per fixed tick, and its output is written to the RigPose
physics layer (ADR 0106). Kinematic-mode PMX rigid bodies (mode 0 —
collision-only proxies that follow the bone rather than drive it) become
rapier kinematic colliders so dynamic bodies (mode 1/2) can collide
against the character's own body, matching MMD's own physics semantics.

Isolation is intentional: MMD jiggle-bone rigid bodies must never appear
in gameplay `CollisionEvents`, interact with `CollisionLayers`, or be
queried by `hitbox.rs` / the character controller's obstacle scan. A
second `rapier` instance is the simplest way to guarantee that boundary
without threading an "is this a gameplay body or a cosmetic one" filter
through every gameplay collision query.

Only a *continuous* animation pose may be driven into the solver. A
looping clip that wraps to its first frame, a seek, a hard state switch,
or a teleported character all reposition the whole skeleton within one
fixed step; feeding that displacement to kinematic mode 0 bodies would
make rapier derive a velocity from motion that never happened, and the
impulse joints would then throw every attached chain through the limits
meant to hold it. The bridge therefore reseats every body on the new pose
with zero velocity instead of integrating through the jump. MMD resolves
a frame jump the same way.

**Whether a step is continuous is declared, never inferred.**
`RigPose::mark_discontinuous` is raised for one fixed step by whatever
performed a generic repositioning — a loop wrap or an instant clip switch in
`animation_system`, and in future a teleported character, a respawn, or a
scene load. `Animator::seek` is still a declared discontinuity, but ADR 0109
preserves it internally as a seek so the MMD bridge may reconstruct its clip
history. A crossfade does not raise either declaration: its blended pose is
continuous by construction.

This was originally inferred instead, by testing each body's per-step
displacement against a fraction of the rig's rest extent. That cannot
work, and no threshold or statistic repairs it: fast motion and a
repositioning reach the reader as the same quantity, a large
displacement. On this project's own character the inference fired
several times a second during an ordinary dance — the fastest step whips
30 of 504 bones past 0.30 m while the median moves 0.031 m — reseating
the entire rig mid-swing, which reached the screen as the cloth flicking
and snapping back, including parts the model author had pinned rigid and
which could not legitimately move at all. Only the code that moves the
clock or the character knows which case it is, so only that code may say.

A reseat is still not a full reconstruction: seeking to frame 1000 places
the bodies on that frame's pose with no velocity, whereas the skirt that
frame actually shows was reached by swinging through the frames before
it. ADR 0109 defines the physics pre-roll used when a seek has reconstructible
clip history; the reseat described above remains the fallback for other
discontinuities and unsupported seek contexts.

## Consequences

- One new dependency (`rapier3d`) enters every build target, including
  wasm32 — unlike `ufbx` (ADR 0081), this is not gated off wasm, so its
  wasm32 compile and runtime behavior must be verified before this ADR's
  status moves to Accepted.
- `collision.rs` loses ~250 lines of hand-written geometry
  (`sphere_vs_sphere` through `capsule_vs_capsule`, `closest_point_on_*`
  helpers); their unit tests are replaced by tests asserting the same
  observable `CollisionEvents`/`PushOut` contract against rapier-backed
  detection, not rapier's internals.
- `physics.rs` loses its hand-written gravity integration and restitution
  reflection; `TERMINAL_VELOCITY_DOWN` clamping either moves to a rapier
  velocity cap or is dropped in favor of rapier's own solver stability —
  decided during implementation, not by this ADR.
- `character_controller.rs` is untouched in behavior and public API;
  only the shape-query calls it makes change their backing implementation.
- The MMD rigid-body bridge is new code, gated behind whatever feature
  flag the eventual MMD import ADR defines (mirroring `fbx-import`'s
  pattern) — this ADR does not make MMD import buildable by itself, only
  removes the physics-engine dependency question as a blocker for it.
- Determinism, save/replay (ADR 0064), and any future networking work
  must treat rapier's internal state as non-authored, non-persisted
  simulation state, exactly like the hand-rolled systems it replaces.

## Alternatives Considered

- **Bullet via `mmd-anim-physics-bullet`** — zero glue code for the MMD
  bridge specifically, and behavioral parity with existing Bullet-based
  MMD viewers (PMX rigid-body/joint parameters are commonly tuned against
  Bullet's solver). Rejected as the *general* engine dependency: C++ FFI,
  no wasm32 path, and would leave `collision.rs`/`physics.rs` on
  hand-written math indefinitely since Bullet's rigid-body API is a poor
  fit for retrofitting under the existing `Collider`/`CollisionEvents`
  contract. Remains an option to reconsider *only* for the MMD bridge
  specifically if rapier's joint behavior turns out not to reproduce PMX
  physics convincingly — the isolation in §5 keeps that swap contained to
  one module.
- **Avian (`bevy_xpbd`)** — pure Rust, wasm-capable, XPBD solver. A
  reasonable second choice; rejected for now because rapier has the wider
  install base and more prior art for joint-chain (ragdoll-style) setups
  matching PMX's rigid-body/joint model. Revisit if rapier's joint solver
  proves inadequate for MMD jiggle behavior.
- **PhysX / Jolt (via FFI bindings)** — mature, high-performance, but both
  are C++ dependencies without a wasm32 story, the same objection as
  Bullet. Jolt's Rust bindings are also less mature than rapier's native
  crate. Rejected.
- **Keep everything hand-rolled, write a bespoke rigid-body/joint solver
  for MMD only** — avoids any new dependency, but a 6-DOF spring-joint
  solver with stable integration is exactly the kind of numerically
  delicate code this project's own guidance says not to hand-roll when a
  maintained library exists. Rejected outright.
- **Adopt rapier for the MMD bridge only, leave `physics.rs`/
  `collision.rs` untouched** — avoids touching working, tested gameplay
  code. Rejected: running two independent collision/rigid-body
  implementations side by side (one hand-rolled for gameplay, one rapier
  for MMD) is the duplication this ADR exists to avoid paying for twice;
  since rapier is already a mandatory dependency once the MMD bridge
  exists, letting it also own §2–3 is close to free.

## Compatibility and Migration

- No persisted format changes: `Collider`, `PhysicsBody`,
  `CollisionLayers`, `TriggerVolume`, `Velocity`, `Gravity`,
  `GravityScale` keep their existing shapes, so scene/prefab JSON
  containing these components is unaffected.
- Public API surface (`crate::collision::*`, `crate::physics::*`,
  `crate::character_controller::*`) is unchanged; only the internal
  implementation of `collision_detection_system`, `gravity_system`,
  `velocity_system`, and `restitution_system` changes. Downstream callers
  (`foot_ik.rs`, `hitbox.rs`, `character_controller.rs`'s obstacle scan,
  `crate::scripting`'s `ctx.collisions()`, `lock_on.rs`, `camera.rs`) need
  no changes.
- Existing collision/physics/character-controller test suites must pass
  unchanged in their assertions about public behavior (positions,
  grounded state, event contents); tests asserting internal broad-phase
  counters (`CollisionStats`) or exact geometry helper outputs are
  expected to be rewritten against the new implementation, not the
  observable contract.
- The MMD rigid-body bridge introduces no persisted format of its own in
  this ADR — its shape is deferred to the MMD import ADR, which must also
  cover PMX/VMD parsing (`mmd-anim-format`), IK/appended-parent evaluation
  (`mmd-anim-runtime`), morph target rendering (currently absent from
  `mesh.rs`/`skinning.rs`), and skin splitting for `MAX_JOINTS = 128`
  (`skinning.rs`, ADR 0086 §4). None of those are decided here.
