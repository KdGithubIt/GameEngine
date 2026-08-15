# ADR 0076: Submeshes and Material Slots

Status: Accepted
Date: 2026-07-21

## Context

A runtime entity draws exactly one mesh with exactly one material: the render
path queries `(Handle<Mesh>, GlobalTransform, Option<&Material>)` and falls
back to `Material::default()` when the component is absent. Nothing can
express "this mesh has three parts, each with its own material".

glTF represents that case directly: a mesh contains primitives, and each
primitive declares one material. ADR 0074 §1 worked around the missing
capability by splitting every primitive into its own mesh sub-asset, and
ADR 0074 §4's prefab builder turns a multi-primitive node into a
transform-only parent with one child entity per primitive.

That divergence is visible to authors. Unity gives a renderer a `materials`
array with one entry per submesh; Godot gives a `MeshInstance3D` one surface
material override per surface. Both produce **one node** for a mesh with
three primitives. This engine produces four entities, none of which
corresponds to anything in the source, and the extra entities have to be
selected, named, and reasoned about for the rest of the model's life.

The split also costs draw efficiency: primitives that shared one vertex
buffer become separate meshes with separate buffers and separate batches.

## Decision

### 1. `Mesh` carries submesh ranges

```rust
/// One contiguous run of a mesh drawn with a single material slot.
pub struct Submesh {
    /// First index into `Mesh::indices`, or first vertex when unindexed.
    pub start: u32,
    /// Number of indices, or vertices when unindexed.
    pub count: u32,
}
```

`Mesh` gains `submeshes: Vec<Submesh>`. An **empty** vector means the mesh is
drawn as a single range covering everything, so every existing mesh —
built-in primitives, OBJ, procedural geometry, navmesh debug output — keeps
its current behavior without describing ranges it does not have.

Ranges are validated with the rest of the mesh: each must lie inside the
index (or vertex) count. `Mesh` is a runtime type with no serialized form, so
no persisted format changes.

### 2. Material slots are a per-entity component

```rust
/// One material per submesh, in submesh order.
pub struct MaterialSlots {
    pub materials: Vec<Material>,
}
```

Resolution for submesh `i`, in order: `MaterialSlots.materials[i]`, then the
entity's `Material`, then `Material::default()`. A mesh with more submeshes
than slots therefore degrades to the single material rather than
disappearing, and an entity that only ever needs one material keeps using
`Material` exactly as before.

Authoring gains `engine.material_slots` with an ordered `materials` array of
`AssetRef`. `engine.material` is unchanged and remains the right component
for single-material entities.

### 3. Batching and draws are per submesh

The static batch key becomes `(mesh, submesh index, texture bind group,
pipeline)`, and `GpuMesh` draw methods take the range to draw. Instancing is
unaffected in kind: instances of the same submesh of the same mesh with the
same material still batch together, which is what a crowd of identical
multi-material props needs.

Skinned draws follow the same rule, one draw per submesh, since skinned
meshes are already one draw per entity.

### 4. Mesh sub-assets return to one per glTF mesh

This replaces ADR 0074 §1. A glTF mesh imports as **one** `Mesh` sub-asset
whose primitives become submeshes in declaration order, and
`GltfMeshData::material` becomes `materials: Vec<Option<AssetId>>` with one
entry per submesh.

`imported_sub_asset_id(source, Mesh, n)` therefore uses the glTF mesh index
again, as it did before ADR 0074. Sub-asset names lose the `.{primitive}`
suffix and are the glTF mesh name.

The prefab builder emits **one entity per mesh node**, carrying
`engine.material_slots` when the node's mesh has more than one submesh and
`engine.material` when it has exactly one. The transform-only parent and
per-primitive children introduced by ADR 0074 §4 are gone.

## Consequences

- A mesh authored with several material slots instantiates as one entity, in
  the arrangement Unity and Godot produce.
- Primitives of one mesh share one vertex and index buffer again, so a
  multi-material model uploads less and batches better than the per-primitive
  split it replaces.
- `Mesh` gains a field, so every struct-literal construction site in the
  workspace is updated in this change under the breaking-change protocol.
  `Vertex`, `SkinningVertexData`, and the vertex buffer layouts are untouched.
- Mesh sub-asset IDs shift for sources whose meshes have more than one
  primitive, back to the pre-ADR-0074 numbering. Single-primitive meshes —
  including every model in this repository — keep their IDs.
- Per-part control is unchanged for models that genuinely have several mesh
  nodes: those still become separate entities, so hiding one part or
  attaching to it keeps working. Only primitives *within* one mesh merge.

## Alternatives Considered

- **A material array on `Material` itself.** Rejected: `Material` is the
  per-draw value the renderer already consumes; making it a collection would
  push slot indexing into every consumer, including the ones that only ever
  have one material.
- **Keep the ADR 0074 per-primitive split and add slots anyway.** Rejected:
  the split is exactly the thing slots exist to remove, and keeping both
  would leave two representations of one source concept.
- **Store slots as `Vec<Handle<Material>>` instead of `Vec<Material>`.**
  Rejected for consistency: the existing `Material` component stores a
  resolved value, and mixing a handle-based and a value-based path in one
  renderer query is a larger change than this ADR needs.
- **One draw call with a per-instance material index and a bindless
  texture table.** Rejected: it removes the per-submesh bind group change but
  requires a bindless capability this renderer does not have and that WebGL2
  cannot provide.

## Compatibility and Migration

- `Mesh::submeshes` is additive and empty by default; every existing mesh
  draws exactly as before.
- `MaterialSlots` and `engine.material_slots` are additive. No existing scene
  uses them.
- No serialized format, `StableId` format, or schema version changes. Mesh
  sub-asset IDs derive from the same function with a different selector, and
  affected projects recover by reimporting the source, which already reports
  `asset.imported_sub_asset_missing` for a reference that no longer resolves.
- ADR 0074 §1 and the per-primitive part of §4 are superseded. ADR 0074 §2
  (shared skeletons), §3 (authoring components), §5 (prefab EntityRef remap),
  and ADR 0075 in full remain in force.
