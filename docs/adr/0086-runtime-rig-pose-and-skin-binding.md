# ADR 0086: Runtime Rig Pose and Per-Mesh Skin Binding

Status: Accepted
Date: 2026-07-26

## Context

ADR 0074 made the runtime `Skeleton` a component shared by every mesh drawn
from the same source skin, and ADR 0077 added stable `BoneId` identity to it.
The resulting component is not a rig: it stores **skin-local** data.

```rust
pub struct Skeleton {
    pub joints: Vec<Entity>,              // glTF skin joint order
    pub inverse_bind_matrices: Vec<Mat4>, // glTF skin joint order
    pub bone_ids: Vec<BoneId>,            // glTF skin joint order
    pub asset: Option<AssetId>,
}

pub struct SkinnedMesh {
    pub skeleton: Entity,
}
```

`joint_palette_system` therefore computes one palette per `Skeleton` and
copies it into every bound `SkinnedMesh`. That is only correct while all
bound meshes share one skin, which is exactly what the component's joint
order assumes.

Three consequences follow, and all three block the authoring model that
ADR 0087 describes:

1. **One rig cannot serve two skins.** A character whose body, clothes, and
   hair are separate skins over one skeleton needs one `Skeleton` per skin,
   which reintroduces the duplicated joint hierarchies ADR 0074 removed.
2. **Only bones a skin references exist at runtime.** Attachment bones,
   helper bones, and bones driven by a clip but unused by any skin are never
   spawned, and `animation_system` silently drops their channels because
   `Skeleton::bone_ids` has no entry for them.
3. **The joint cap is a skeleton cap.** `MAX_JOINTS = 128` bounds the whole
   rig rather than one draw's palette, because the two are the same list.

The pose of a rig and the binding of a mesh to that pose are different
things with different owners. This ADR separates them. It changes runtime
components only: no authoring schema, no serialized format, and no editor
surface changes here, so the split can be validated by rendering existing
scenes unchanged before ADR 0087 builds on it.

## Decision

### 1. `Skeleton` is the rig pose over a whole skeleton asset

```rust
pub struct Skeleton {
    /// Joint entities in `SkeletonAsset::bones` order (parent-before-child).
    pub joints: Vec<Entity>,
    /// `BoneId` per joint, same order.
    pub bone_ids: Vec<BoneId>,
    /// The skeleton asset these joints were spawned from, when known.
    pub asset: Option<AssetId>,
}
```

`inverse_bind_matrices` is removed: an inverse bind matrix belongs to a skin,
not to a rig. `joints` and `bone_ids` now cover **every bone of the skeleton
asset**, in the asset's own bone order, so a bone that no skin references
still has a joint entity and a resolvable `BoneId`.

Joint entities are spawned from `SkeletonAsset::bones` rather than from the
document node list. This is behavior-preserving: `build_skeleton_bones`
already applies the same joints-plus-ancestors inclusion walk that
`spawn_skin` applied, and copies each node's local TRS into the bone's rest
TRS, so the spawned hierarchy and its transforms are unchanged for content
that has a skeleton asset.

`spawn_skin` is replaced by:

```rust
pub fn spawn_rig(world: &mut World, skeleton: &SkeletonAsset)
    -> Result<Skeleton, RigSpawnError>;
```

There is one way to create a rig, and it takes the asset that defines the
rig. Code-authored content builds a `SkeletonAsset` (which
`compute_skeleton_identity` already supports) instead of passing a parallel
node list, joint list, and matrix list that no type relates to each other.

### 2. `SkinnedMesh` owns the skin binding

```rust
pub struct SkinnedMesh {
    /// Entity carrying the `Skeleton` this mesh is skinned to.
    pub rig: Entity,
    /// Bone driving each skin joint, in skin joint order.
    pub joint_bones: Vec<BoneId>,
    /// Inverse bind matrix per skin joint, same order.
    pub inverse_bind_matrices: Vec<Mat4>,
    /// Skin sub-asset this binding came from, for diagnostics.
    pub skin: Option<AssetId>,
}
```

The field is named `rig`, not `skeleton`, because the referenced entity now
carries a rig pose covering the whole skeleton rather than this mesh's own
joint list.

`joint_bones` is the indirection that makes rig sharing work: skin joint `i`
resolves through `joint_bones[i]` to a `BoneId`, and that `BoneId` resolves
through the rig's `bone_ids` to a joint entity. Two skins with different
joint orders, different joint counts, or overlapping joint subsets bind to
the same rig without either one imposing its order on the other.

The binding stores `BoneId` rather than a resolved index into
`Skeleton::joints` so that a binding remains meaningful when it is built
against a rig it did not spawn with — the external-rig case ADR 0087 needs —
and so a bone missing from the rig is a reportable condition rather than an
out-of-range index.

### 3. Palettes are computed per bound mesh

`joint_palette_system` builds, once per frame and once per rig, a
`BoneId -> world matrix` map from that rig's joints. Each `SkinnedMesh` then
gathers its own palette:

```
palette[i] = world(rig, joint_bones[i]) * inverse_bind_matrices[i]
```

Existing degrade-gracefully behavior is preserved and extended: a despawned
rig entity yields an empty palette, a joint with no `GlobalTransform`
contributes an identity world matrix, and a `BoneId` absent from the rig now
also contributes an identity world matrix rather than dropping the joint.
Every one of these renders the bind pose instead of panicking.

Palettes are **not** deduplicated across meshes that share a skin. The shared
work is the per-rig world-matrix map, which is shared; the remaining per-mesh
cost is one matrix multiply per skin joint. Caching whole palettes by
`(rig, skin)` would only pay off when the same skin asset is drawn twice
against the same rig, which normal multi-part characters never do, and it
would add per-frame map allocation to every character that does not.

### 4. `MAX_JOINTS` bounds one palette, not one rig

The 128-matrix uniform palette cap (ADR 0043 §3) applies to a single skin
binding, which is what a draw uploads. A skeleton asset may now carry more
than 128 bones as long as no single skin binds more than 128 of them. The
existing per-skin import diagnostic already enforces the limit at the right
granularity and is unchanged.

## Consequences

- One rig serves any number of skins, so ADR 0087 can place body, clothes,
  and hair as separate render parts over one joint hierarchy.
- Bones that no skin references are spawned and addressable, which is the
  prerequisite for bone attachment (ADR 0088) and for clips that drive
  helper bones.
- **Clip channels that were silently dropped now apply.** A channel whose
  `target_bone` is a bone outside every skin previously found no entry in
  `Skeleton::bone_ids`; it now resolves and animates. Content that relied on
  those channels being inert changes appearance.
- Joint entity counts rise for sources whose skeleton contains bones no skin
  uses. The increase is bounded by the skeleton asset's bone count, which the
  importer already builds.
- `foot_ik_system` sees the full bone list instead of the skin's subset, so a
  leg chain whose intermediate bone is unskinned now resolves.
- `Skeleton`, `SkinnedMesh`, and `spawn_skin` are public API. This is a
  breaking change; all in-tree call sites (`scene_bridge`, `foot_ik`,
  `animation`, `gltf_import`, `examples/skinned_mesh`) change in the same PR.

## Alternatives Considered

### Keep `Skeleton` skin-local and spawn one per skin

Rejected. It is the arrangement ADR 0074 replaced. Two skins over one
skeleton would need two joint hierarchies, two palette computations, and two
animation targets, and attaching a weapon would have to pick one of them.

### Store resolved joint indices in `SkinnedMesh` instead of `BoneId`

Rejected. Index resolution is only valid against the exact rig the binding
was built with, which forecloses the external-rig override in ADR 0087 and
turns a missing bone into an out-of-range access instead of a diagnostic.
The per-frame cost of the `BoneId` lookup is one hash map built per rig.

### Put the skin binding on the rig, keyed by skin asset

Rejected. It makes the rig entity accumulate state owned by meshes that may
be spawned and despawned independently, and it reintroduces the ordering
dependency between mesh spawning and rig spawning that the entity-map
lookup currently avoids.

### Do this together with the authoring change

Rejected. The runtime split is verifiable on its own: existing scenes must
render identically. Bundling it with new authoring components would leave no
state in which a regression can be attributed to one or the other.

## Compatibility and Migration

No serialized format changes. Scene and prefab schemas, component schemas and
their versions, `StableId` and `AssetId` derivations, manifest records,
`Vertex`, and `SkinningVertexData` are all unchanged, so existing projects
load and render without migration.

`engine.skeleton`, `engine.skinned_mesh`, `engine.skinned_mesh_renderer`, and
`engine.animation_controller` keep their current fields. The skin binding a
mesh needs is derived at conversion time from the mesh sub-asset itself: the
manifest resolves the mesh to its owning source, and that source's import
result gives the skin it binds (`GltfMeshData::skin_index`), the skin's
`joint_bone_ids`, and its inverse bind matrices. No authoring field is added
to carry it.

A skinned mesh whose owning source or skin cannot be resolved keeps an empty
binding and renders in its bind pose, with a
`scene_bridge.skin_binding_unresolved` warning, matching the existing
non-blocking treatment of unresolved skinned-mesh state.
