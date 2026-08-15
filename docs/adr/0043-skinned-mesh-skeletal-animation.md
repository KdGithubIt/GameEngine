# ADR 0043 — Skinned Mesh & Skeletal Animation

## Status: Accepted (sections 2 and 4 superseded by ADR 0074)

Date: 2026-07-04

> ADR 0074 replaces the inline joint list of §2 with a shared `Skeleton`
> component referenced by `SkinnedMesh`, and moves the §4 joint-target lookup
> from `SkinnedMesh` to the `Skeleton` on the animator's own entity. The
> vertex layout (§1), palette upload (§3), and import scope (§5) are
> unchanged.

## Context

Phase 48 adds skinned mesh rendering and skeletal animation. The current
engine state constrains the design in four ways:

1. `Vertex` (`position` / `normal` / `color` / `uv`) is frozen by the
   breaking-change protocol. Every static mesh, the OBJ loader, the glTF
   importer, and the instancing pipeline depend on its exact layout.
2. The mesh render pipeline binds vertex buffer slot 0 (`Vertex::LAYOUT`) and
   slot 1 (`InstanceData::LAYOUT`, ADR 0042).
3. glTF import (ADR 0032) is static-only: `JOINTS_0` / `WEIGHTS_0`, skins,
   and animation tracks are silently ignored.
4. `AnimationClip` / `Animator` (Phase 37) drive exactly one entity's
   `Transform`; channels have no concept of a target joint. Runtime
   parent-child transform propagation (`Parent` / `Children` +
   `transform_propagation_system`) exists as of 2026-07-04.

Decisions required before implementation: where skinning vertex data lives,
how skeletons are represented at runtime, how joint matrices reach the GPU,
and how animation clips address joints.

## Decision

### 1. Skinning attributes live in a second vertex buffer, not in `Vertex`

`Vertex` is not modified. A new per-vertex type carries skinning data:

```rust
#[repr(C)]
pub struct SkinningVertexData {
    /// Indices into the skin's joint array.
    pub joints: [u16; 4],
    /// Normalized blend weights; the importer renormalizes to sum to 1.0.
    pub weights: [f32; 4],
}
```

`Mesh` gains an optional `skinning: Option<Vec<SkinningVertexData>>` field
(length must equal `vertices.len()`; validated on upload). Skinned draws bind
it at **vertex buffer slot 2**, leaving slot 0 (`Vertex`) and slot 1
(`InstanceData`) untouched.

### 2. Skeleton joints are runtime entities

Importing a glTF skin spawns one runtime entity per joint node. Joint
entities are linked with the existing `Parent` / `Children` components and
carry `Transform` / `GlobalTransform`, so joint world matrices come from the
existing `transform_propagation_system` with no duplicated hierarchy logic.

A new component ties a mesh entity to its skin:

```rust
pub struct SkinnedMesh {
    /// Joint entities in glTF skin joint order.
    pub joints: Vec<engine_ecs::Entity>,
    /// One inverse bind matrix per joint, same order.
    pub inverse_bind_matrices: Vec<glam::Mat4>,
}
```

`joints.len()` must equal `inverse_bind_matrices.len()`; violations are
reported as diagnostics at spawn/import time, never as panics.

### 3. GPU skinning with a fixed-size uniform joint palette

A per-frame system computes the joint palette
`palette[i] = joint_world[i] * inverse_bind[i]` and uploads it as a uniform
buffer of `MAX_JOINTS = 128` matrices (8 KiB, within the WebGPU / WebGL2
minimum uniform buffer size, so the WASM target needs no storage-buffer
fallback). Skins with more than 128 joints fail import with a blocking
diagnostic.

Rendering uses a dedicated `mesh_skinned.wgsl` shader and pipeline. Skinned
meshes are **not instanced** in v1: each skinned entity issues its own draw
with its own palette. A despawned joint entity contributes its last known
palette entry fallback (identity if never resolved); it does not panic.

### 4. Animation channels gain an optional joint target

`AnimChannel` gains `target_joint: Option<usize>`:

- `None` — the channel drives the `Animator` entity's own `Transform`
  (existing Phase 37 behavior, unchanged).
- `Some(i)` — the channel drives `SkinnedMesh.joints[i]`'s `Transform`.

The animator system resolves joint targets through the `SkinnedMesh`
component on the same entity. Out-of-range indices and missing joint
entities skip the channel; linear interpolation only, matching Phase 37.

### 5. glTF import scope (v2)

| Item | Decision |
|------|----------|
| `JOINTS_0` (u8 / u16) | Imported, widened to `u16` |
| `WEIGHTS_0` (f32 / normalized u8 / u16) | Imported, renormalized to sum 1.0 with a warning diagnostic when the source sum deviates by more than 1e-3 |
| `inverseBindMatrices` | Imported; missing accessor falls back to identity with a warning |
| Skins with > 128 joints | Blocking import diagnostic |
| Animation samplers (translation / rotation / scale, `LINEAR`) | Imported as `AnimationClip` channels with joint targets |
| `STEP` / `CUBICSPLINE` samplers | Downgraded to linear with a warning diagnostic |
| Morph targets / weights animation | Not supported in v1 |
| External buffer URIs | Still unsupported (ADR 0032 unchanged) |

### 6. No authoring / serialization changes in Phase 48

`SkinnedMesh`, `Skeleton` entities, and clip targets are runtime-only state
produced by the importer. No authoring component schema, scene format, or
`StableId` format changes. Editor exposure (assigning skinned meshes from the
Inspector) is deferred to a later phase and will get its own ADR if it
requires schema changes.

## Consequences

- `Vertex`, every serialized format, and the instancing path are untouched;
  the breaking-change protocol is not triggered.
- `render.rs` gains a second mesh pipeline and a per-draw uniform palette
  bind group; static rendering performance is unaffected.
- Skinned meshes skip the instancing batcher; large crowds of skinned
  characters stay O(n) draws until a future skinned-instancing phase.
- Joint entities appear in the runtime world (visible to queries and debug
  tooling); a 60-joint character adds 60 entities. This is accepted for the
  hierarchy reuse it buys.
- Palette computation is O(total joints) per frame on the CPU.
- `engine` gains no new third-party dependency (`gltf` crate already
  present).

## Alternatives Considered

- **Extend `Vertex` with joints/weights** — rejected on technical grounds,
  not to avoid the breaking-change protocol (which the project owner accepts
  when justified): every static mesh would carry 24 dead bytes per vertex in
  memory and GPU bandwidth, every static draw would ship unused attributes
  (or a second `Vertex` type would be needed anyway, recreating the split
  this ADR chooses), and the static/instanced pipeline would be touched for
  no functional gain. The separate slot-2 buffer isolates the cost to
  skinned meshes only.
- **CPU skinning into a rewritten vertex buffer** — rejected for v1: no new
  pipeline needed, but re-uploads every skinned mesh every frame and scales
  poorly; may return later as a debug fallback.
- **Storage-buffer joint palettes** — rejected for v1: removes the 128-joint
  cap but complicates the WASM/WebGL2 story; the uniform palette is the
  portable baseline.
- **Flat joint matrices inside `SkinnedMesh` (no joint entities)** —
  rejected: duplicates hierarchy propagation that `Parent` / `Children`
  already provide and prevents attaching entities (weapons, effects) to
  bones.
- **Separate `Skeleton` component shared by multiple `SkinnedMesh`es** —
  deferred: v1 keeps joints inline in `SkinnedMesh`; meshes sharing a skin
  share the same joint entities, so sharing already works at the entity
  level.

## Compatibility and Migration

- No persisted format changes. `AnimationClip` is a runtime asset;
  `target_joint: Option<usize>` is additive and defaults to the existing
  single-entity behavior.
- Public API additions only (`SkinningVertexData`, `SkinnedMesh`,
  `Mesh::skinning`, palette system, importer entry points). One exception:
  if `Mesh` construction sites use struct literals, adding the `skinning`
  field is source-breaking within the workspace; all call sites are updated
  in the same PR per the breaking-change protocol.
- Existing scenes, manifests, prefabs, and graphs load unchanged.
