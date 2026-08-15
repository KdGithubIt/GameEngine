# ADR 0044 — Particle System (CPU Simulation + Instanced Rendering)

## Status: Accepted

Date: 2026-07-04

## Context

Phase 49 adds a particle system. GPU instancing (Phase 47, ADR 0042) already
batches entities that share a mesh and texture into single instanced draws,
and `InstanceData` carries a per-instance model matrix and RGBA color. The
design question is where particles are simulated, how they reach the GPU,
and how randomness is sourced (the engine has no `rand` dependency).

## Decision

### 1. Particles are emitter-owned data, not entities

A `ParticleEmitter` component owns its particle pool (`Vec` of private
particle structs). Particles are never ECS entities; a 1 000-particle
emitter is one component. `max_particles` caps the pool (default 1 024).

### 2. CPU simulation on the frame schedule

`particle_update_system` runs on the frame schedule (variable `Time` delta),
registered by `App::new` after transform propagation. Particles are visual,
not gameplay-authoritative, so they do not use the fixed timestep. Particles
simulate in **world space**: spawn position comes from the emitter's
`GlobalTransform` at spawn time, so moving emitters leave trails.

### 3. Rendering reuses the instanced mesh pipeline unchanged

The emitter references its particle mesh via a `mesh: Handle<Mesh>` field
(not a `Handle<Mesh>` component, so the batcher does not also draw the
emitter entity itself). The render batch collection adds one `InstanceData`
per live particle (scale from particle size, translation from particle
position, color from the particle's interpolated color multiplied by the
entity's optional `Material`). Particles therefore batch with each other and
with regular instanced meshes; no new pipeline, shader, or bind group.

Billboarding is **not** part of v1. Particle meshes are small 3D meshes
(`Mesh::cube()` by default); camera-facing quads are a follow-up.

### 4. Deterministic engine-owned RNG

Emitters seed a small xorshift RNG (`seed` field, default derived from the
emitter's spawn parameters). No `rand` dependency is added. Two emitters
with equal configuration and seed produce identical particle streams, which
keeps tests deterministic.

### 5. Emitter model (v1)

| Field | Meaning |
| --- | --- |
| `mesh` | Particle mesh handle (shared by all particles of the emitter) |
| `spawn_rate` | Particles per second (`0.0` stops emission; pool keeps simulating) |
| `lifetime` | Seconds each particle lives (min/max range) |
| `initial_speed` | Speed range along the emission direction |
| `direction` / `spread` | Emission cone: base direction and half-angle in radians |
| `gravity` | World-space acceleration applied to every particle |
| `start_color` / `end_color` | RGBA lerped over each particle's life |
| `start_size` / `end_size` | Uniform scale lerped over each particle's life |
| `max_particles` | Hard pool cap |

## Consequences

- No renderer changes beyond one additional batch-collection pass; particle
  draws inherit instancing, `InstanceStats`, WASM support, and the existing
  material/texture path for free.
- CPU cost is O(live particles) per frame; acceptable for v1 scale
  (thousands, not millions). A GPU-simulated system would be a new ADR.
- No authoring schema, editor integration, or serialized format changes;
  emitters are runtime-constructed (same boundary as Phase 48).
- Without billboarding, flat quad particles look wrong from the side; the
  default cube mesh avoids the worst artifacts until billboards land.

## Alternatives Considered

- **Per-particle ECS entities** — rejected: thousands of spawns/despawns per
  second churn archetypes and the entity allocator for no queryability gain.
- **GPU compute simulation** — rejected for v1: wasm32/WebGL2 has no compute
  path in the current renderer baseline, and CPU simulation is sufficient at
  target scale.
- **Dedicated particle pipeline with billboards** — deferred: reusing the
  instanced pipeline ships value with near-zero renderer risk; a billboard
  mode can be added later without discarding this design.
- **`rand` / `fastrand` dependency** — rejected: a 10-line xorshift meets
  the need deterministically with zero dependency cost.

## Compatibility and Migration

Additive only: new `particles` module, one new frame system, one new
batch-collection pass. No persisted formats, public API removals, or
`Vertex` / pipeline changes.
