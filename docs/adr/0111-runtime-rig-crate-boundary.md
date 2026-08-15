# ADR 0111: Runtime Rig Crate Boundary

Status: Accepted
Date: 2026-08-14

## Context

The `engine` crate currently owns both high-level runtime integration and
low-level rig primitives. As a result, a change to transform propagation,
skeleton identity, skin binding, or layered pose storage is validated through
the same package that pulls rendering, windowing, audio, importers, scripting,
and Rapier physics into its compile graph.

CI measurements on the affected-package fast path show that this cost is
dominated by compilation rather than test execution. Reducing the number of
tests therefore cannot make low-level rig changes proportionally cheaper.

The rig primitives already form a dependency layer below animation import,
rendering integration, and MMD physics. They can be separated without changing
their runtime semantics.

## Decision

Introduce the workspace package `engine-rig` at `crates/rig`.

`engine-rig` owns:

- local/global transform components and hierarchy propagation;
- stable skeleton assets and bone identity;
- skin bindings, runtime skeleton entities, rig spawning, and joint palettes;
- layered `RigPose` storage and deterministic world-space pose evaluation;
- imported rigid-body rig descriptions and registries.

`engine-rig` MAY depend on `engine-ecs`, stable authoring identifiers/data
primitives, math, collections, and serialization support. It MUST NOT depend on
`engine`, `engine-renderer`, wgpu, winit, audio, model importers, or Rapier.

The high-level `engine` crate depends on `engine-rig`. Existing module paths
such as `engine::transform`, `engine::skeleton_asset`, `engine::skinning`,
`engine::rig_pose`, and `engine::rigid_body_rig` remain source-compatible
facades that re-export the types and functions owned by `engine-rig`.

Two integration details remain callable from `engine` but are hidden from
normal generated documentation:

- the animation-seek discontinuity marker queried by MMD seek reconstruction;
- the canonical skeleton-hash helper functions reused by retarget cache keys.

They are cross-crate implementation contracts, not gameplay APIs.

Animation sampling/state machines, pose graphs, foot IK, model importers,
render integration, and MMD physics remain in `engine` in this step. Moving
solver and animation domains is a later boundary decision and must not create a
dependency from `engine-rig` back to `engine`.

## Compatibility

The move does not change serialized authoring schemas, component field layouts,
bone IDs, skeleton hashes, pose composition order, or runtime system behavior.
Rust `TypeId` identity remains internally consistent because all users observe
the same re-exported concrete type. Diagnostic `std::any::type_name` strings for
moved types now name `engine_rig`; those strings are runtime metadata and are
not persisted identifiers.

## Consequences

- Changes confined to rig primitives can be selected as `engine-rig` by
  affected validation and avoid the high-level engine compile graph.
  The Windows validation classifier maps `crates/rig/**` directly to the
  `engine-rig` package; workspace manifest changes still force full validation.
- `engine` keeps its existing public source paths, limiting migration cost for
  downstream crates and game modules.
- The dependency graph gains a one-way `engine -> engine-rig` edge and no cycle.
- MMD physics changes still compile the high-level `engine` crate until the
  solver/animation boundary is split in a later change; this ADR does not claim
  that every MMD PR is below the CI latency target by itself.

## Alternatives considered

### Keep the monolithic crate and tune caches only

Rejected. Warm sccache materially helps but the measured engine package still
spends minutes compiling while the actual unit-test execution takes seconds.

### Big-bang split of every engine subsystem

Rejected. A broad split would combine dependency inversion, API migration, and
CI changes in one hard-to-review operation. Establishing the low-level rig
boundary first is reversible and gives later animation/MMD extraction a stable
dependency target.
