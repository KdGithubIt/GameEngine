# ADR 0074: Model Import Instantiation and Shared Skeletons

Status: Accepted (section 1 superseded by ADR 0076; section 2 superseded by
ADR 0086; section 4 superseded by ADR 0075 and ADR 0076)
Date: 2026-07-21

> ADR 0076 replaces §1: a glTF mesh imports as one sub-asset whose primitives
> become submeshes with per-slot materials, instead of one sub-asset per
> primitive, and the prefab emits one entity per mesh node.
>
> ADR 0086 replaces §2: `Skeleton` becomes the rig pose over a whole
> skeleton asset, and the skin's joint order and inverse bind matrices move
> to `SkinnedMesh`, so one joint hierarchy can serve several skins.
>
> ADR 0075 replaces §4: import runs automatically for any glTF/GLB under
> `assets/`, the generated prefab lives in `.engine/imported/` instead of
> beside its source and is not registered in the manifest, and the source
> entry itself is what gets placed. Sections 1, 2, 3, and 5 are unchanged.

## Context

Placing an imported glTF/GLB model in a scene currently produces a white,
partially visible object. Reproducing the failure with
`Wolf-Blender-2.82a.glb` (6 mesh nodes, 6 materials, 1 skin, 5 animation
clips) exposes four separate gaps rather than one bug:

1. **Materials are dropped at import.** `collect_meshes` merges every
   primitive of a glTF mesh into a single `Mesh` and never reads
   `primitive.material()`. `GltfMeshData` has no material field, so nothing
   downstream can know which material a mesh uses. Materials *are* imported
   (`collect_materials`) and the runtime *can* apply them
   (`load_material_asset` decodes base color, normal, and emissive
   textures), but the binding between the two is lost.

2. **An entity with `engine.mesh` and no `engine.material` renders white.**
   `render.rs` falls back to `material.cloned().unwrap_or_default()`, and
   `Material::default()` is `Material::color(1.0, 1.0, 1.0)`. The fallback is
   correct; the authoring data that should have supplied a material is
   missing.

3. **A model file has no instantiation path.** Referencing a glTF source
   directly from `engine.mesh` resolves to `imported.meshes.first()`, so a
   6-mesh character shows one mesh. Building the full character requires the
   author to create one entity per mesh by hand and type `mesh_index` /
   `skin_index` integers into `engine.skinned_mesh_source`. Unity, Unreal,
   and Godot all instantiate an imported model as a prepared node hierarchy
   instead.

4. **Skeletons are duplicated per mesh.** ADR 0043 stores joint entities
   inline in `SkinnedMesh` and explicitly deferred a shared skeleton. Every
   `engine.skinned_mesh_source` component calls `spawn_skin`, which spawns a
   fresh joint hierarchy; `GltfSkinData::skeleton_id` is never read by
   `scene_bridge`. The Wolf's five skinned meshes share one `Armature_0` in
   the source but would produce five independent skeletons at runtime, with
   five times the joint entities, palette work, and animator instances.

Gap 4 is the reason this cannot be a purely additive change: a generated
character hierarchy is exactly the case that makes per-mesh skeletons
expensive, and correcting it later would require migrating scene data that
this ADR would otherwise create.

## Decision

### 1. Mesh sub-assets are per primitive

A glTF primitive is the unit that carries exactly one material, so it
becomes the unit of the `Mesh` sub-asset.

`GltfMeshData` changes to:

```rust
pub struct GltfMeshData {
    /// Flat primitive selector across the document, in declaration order.
    pub source_index: usize,
    /// Zero-based glTF mesh index this primitive belongs to.
    pub gltf_mesh_index: usize,
    /// Zero-based primitive index within `gltf_mesh_index`.
    pub gltf_primitive_index: usize,
    pub id: AssetId,
    pub name: String,
    pub mesh: Mesh,
    /// Material sub-asset declared by this primitive, when it declares one.
    pub material: Option<AssetId>,
    pub skin_index: Option<usize>,
}
```

`source_index` is a flat counter over `(mesh, primitive)` pairs in document
declaration order, so `imported_sub_asset_id(source, Mesh, n)` keeps its
current shape (`mesh:{n}`) and no ID derivation rule changes. Names are the
glTF mesh name for single-primitive meshes and `{mesh name}.{primitive}`
otherwise, keeping the Asset Browser readable.

Primitives that are skipped by the importer (unsupported topology, missing
`POSITION`) still consume a selector so that a later primitive keeps a
stable ID when an earlier one becomes invalid.

### 2. `Skeleton` is a separate runtime component shared by skinned meshes

This supersedes ADR 0043 §2 and its "Separate `Skeleton` component" deferral.

```rust
/// Joint hierarchy spawned once per imported skin.
pub struct Skeleton {
    /// Joint entities in glTF skin joint order.
    pub joints: Vec<engine_ecs::Entity>,
    /// One inverse bind matrix per joint, same order.
    pub inverse_bind_matrices: Vec<glam::Mat4>,
}

/// Binds a mesh entity to the entity carrying its `Skeleton`.
pub struct SkinnedMesh {
    pub skeleton: engine_ecs::Entity,
}
```

`joint_palette_system` resolves `SkinnedMesh::skeleton` and computes one
palette per `Skeleton`, not per skinned mesh. A missing or despawned
skeleton entity yields an identity palette and a diagnostic; it never
panics. The `MAX_JOINTS = 128` cap and the uniform-buffer upload path from
ADR 0043 §3 are unchanged.

Animation joint targets (ADR 0043 §4) resolve through a `Skeleton` component
on the **animator's own entity**, replacing the previous lookup through
`SkinnedMesh` on the same entity. One `Animator` beside one `Skeleton`
drives every mesh bound to it, which is the arrangement Godot's
`AnimationPlayer` + `Skeleton3D` and Unity's `Animator` + Avatar both use.

### 3. Authoring components replace index-typed skin references

`engine.skinned_mesh_source` v1 (`source` + `mesh_index` + `skin_index`) is
removed and replaced by two components:

| Component | Field | Type | Meaning |
| --- | --- | --- | --- |
| `engine.skeleton` (new) | `source` | AssetRef | Registered glTF/GLB source |
| | `skin` | AssetRef | `Skin` sub-asset of that source |
| `engine.skinned_mesh` (new) | `mesh` | AssetRef | `Mesh` sub-asset (one primitive) |
| | `skeleton` | EntityRef | Entity carrying `engine.skeleton` |

Both reference fields follow ADR 0069: an unassigned reference makes the
component inactive with a diagnostic instead of failing conversion. Raw
integer selectors disappear from authoring data; every reference is a stable
sub-asset ID or an entity ID.

`engine.mesh`, `engine.material`, and `engine.animator` keep their current
schemas. `engine.animator` continues to select clips by name.

### 4. Import generates a prefab

A successful glTF import writes `<source stem>.prefab.json` beside the
source, registers it in the manifest, and records its ID in a new
`ImportSettings::generated_prefab: Option<String>` field. The prefab mirrors
the glTF node hierarchy:

- one root entity named after the source;
- one `engine.skeleton` entity per skin, parented to the root, carrying
  `engine.animator` when the source has at least one clip (first clip,
  `autoplay: true`);
- one entity per mesh node, carrying `engine.transform` from the node's
  local transform, `engine.material` from the primitive's material, and
  either `engine.skinned_mesh` (node has a skin) or `engine.mesh` (node has
  no skin).

The generated prefab is **import output, not user data**: every successful
reimport rewrites it. Authors who need a customized version instantiate it
and save their own prefab, which is unaffected by reimport. This matches the
read-only imported-scene model in Unity and Godot and avoids a merge policy
that v1 cannot implement correctly.

The Asset Browser offers `Instantiate in Scene` on the generated prefab
through the existing Phase 33 path, so a model reaches the scene fully
materialed in one action.

### 5. Prefab instantiation remaps `Value::EntityRef`

`PrefabAsset::instantiate_with_root` currently clones component values
verbatim and only rewrites `parent`. A generated prefab stores
prefab-local entity IDs inside `engine.skinned_mesh.skeleton`, so
instantiation must rewrite every `Value::EntityRef` — including refs nested
in `Array` and `Object` values — through the same `id_map` used for parents.

References that are not present in the map are left unchanged so that scene
validation reports `scene.bad_entity_ref` rather than the instantiation
silently substituting a value. This is a defect fix that any prefab with
intra-prefab entity references needs, independent of model import.

## Consequences

- The Wolf, and any comparable character asset, renders correctly after
  drag-and-drop with no manual material assignment.
- A character with N skinned meshes over one skin spawns one joint
  hierarchy instead of N. For the Wolf this is 56 joint entities instead of
  280, and one palette computation instead of five.
- Attaching gameplay entities (weapons, effects) to a bone works against a
  single agreed skeleton entity rather than an arbitrary one of N copies.
- `crates/engine` (importer, `skinning.rs`, `scene_bridge`, component
  registry), `crates/authoring` (prefab instantiation), and `crates/editor`
  (import result handling, Asset Browser) all change. This is a breaking
  change under the AGENTS.md protocol and ships as one PR with all call
  sites and content updated.
- Mesh sub-asset IDs shift for any source whose meshes have more than one
  primitive. Manifests recover by reimporting; see Migration.
- Import writes a file into the project on every successful import, which is
  new behavior for a background job. Failures are reported as diagnostics
  and leave any previous prefab in place.
- Sources with no skin are unaffected by the skeleton work and still gain
  correct per-primitive materials and a generated prefab.

## Alternatives Considered

- **Keep merged meshes and attach a material list to the mesh entity.**
  Rejected: it needs a multi-material renderer path and per-submesh index
  ranges in `Mesh`, a larger change to the render path than splitting
  primitives, for no authoring benefit.
- **Add a `Submesh` sub-asset kind and keep `Mesh` as the merged mesh.**
  Rejected: every single-primitive mesh would appear twice in the Asset
  Browser with identical geometry, and authors would have to know which of
  the two to reference.
- **Implicit skeleton lookup (nearest ancestor with `engine.skeleton`).**
  Rejected: it is the Godot default NodePath behavior, but it makes the
  binding invisible in the Inspector and unrepresentable in diagnostics. An
  explicit `EntityRef` costs one field and is consistent with ADR 0069.
- **Keep `engine.skinned_mesh_source` and add an optional `skeleton` field.**
  Rejected as a local workaround: `mesh_index` / `skin_index` integers would
  remain in authoring data alongside stable sub-asset references, leaving
  two identity schemes for the same thing.
- **Merge generated prefabs with author edits on reimport.** Deferred: it
  requires per-field override tracking, which ADR 0030 explicitly left out
  of prefab v1. Regeneration plus author-owned copies is the honest v1.
- **Do the prefab generation now and the shared skeleton later.** Rejected
  by the project owner: generated hierarchies are precisely the content that
  would need migrating when skeleton sharing lands.

## Compatibility and Migration

- `engine.skinned_mesh_source` is removed from the component registry.
  `examples/busters_lite` (`assets/scenes/arena.scene.json`,
  `assets/prefabs/ally.prefab.json`) is the only in-tree content using it;
  each occurrence becomes `engine.skeleton` + `engine.skinned_mesh` with the
  `skeleton` `EntityRef` pointing at the same entity, preserving the current
  single-mesh, animator-on-the-same-entity arrangement. A conversion test
  covers the migrated form.
- Scenes still containing `engine.skinned_mesh_source` produce an unknown
  component diagnostic and skip the component; they do not fail to load.
- No in-tree manifest currently stores `sub_assets`, so the per-primitive
  selector change breaks no committed asset reference. Manifests outside the
  repository recover by reimporting the source; a mesh reference that no
  longer resolves already reports
  `asset.imported_sub_asset_missing` and falls back to a cube.
- `ImportSettings::generated_prefab` is additive and serialized with
  `skip_serializing_if`, so existing `asset_manifest.json` files parse
  unchanged.
- `AnimChannel::target_joint` semantics, the `MAX_JOINTS` cap, the
  `SkinningVertexData` layout, `Vertex`, `StableId` formats, and the scene
  and prefab `schema_version` values are unchanged.
