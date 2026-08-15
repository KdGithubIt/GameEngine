# ADR 0089: Skinned Model Bind-Pose Bake to Static Mesh

Status: Accepted
Date: 2026-07-26

## Context

ADR 0087 gave every character a `engine.skinned_model` owner and a rig, and
deferred one piece of the original authoring sketch: converting a Skinned
Model that will never animate into a Static Mesh, so it stops carrying
Skeleton, Skin, bone weights, and (if present) an Animation Controller.

The motivating case is authoring cleanliness, not draw-call cost. A model
placed as a Skinned Model and never given a Graph already renders correctly
in its rest pose (ADR 0084) at the cost of one joint entity per bone plus one
transform-propagation step each; for the scale this project targets, that
cost is not worth designing around. What is worth fixing is that a background
statue, a corpse, or discarded prop keeps a Skeleton picker, a rig it will
never use, and the conceptual weight of "this might animate" for no reason.
Bake exists to let an author say "this is decoration now" and have the
authoring data agree.

Two facts about the existing pipeline make the bake itself simple:

1. **Bind-pose vertex positions need no transform.** A skin's bind pose is
   defined so that `joint_world * inverse_bind` is the identity matrix for
   every joint at rest (ADR 0043). A skinned render part's own
   `engine.transform` is already identity, because its world placement comes
   entirely from the joint palette (`crates/engine/src/gltf_prefab.rs`, the
   comment on skinned node transforms). The imported mesh's raw vertex
   positions are therefore already the correct bind-pose geometry; nothing
   needs to be computed against `Skeleton` or `SkinnedMesh` to bake it.
2. **The engine has exactly one mesh file format it can read back: `.obj`,
   with no multi-material or submesh support.** `crate::asset::load_obj`
   merges every group into one draw with `submeshes: Vec::new()`. Any format
   choice that preserves more than that would need a new reader this ADR
   would be introducing solely to read back its own output.

## Decision

### 1. Bake operates on a Skinned Model, in one editor action

The author selects an entity carrying `engine.skinned_model` and runs
**Bake to Static Mesh**. Every Skinned Mesh Renderer whose `model` reference
names that entity is found by reverse lookup and converted; there is no
per-part bake. A render part is an ordinary entity, so nothing stops an
author from deleting a Static Mesh Renderer part after the fact if only some
of the model should freeze.

### 2. Blocking preconditions — warn and refuse, never partially apply

- Any `engine.bone_attachment` in the scene whose `rig` names this entity.
  Baking removes the rig those attachments resolve against; proceeding would
  silently strand a weapon or effect with no diagnostic path back to what
  broke it.
- An `engine.animation_controller` on the entity with `graph` assigned. That
  is configured, active playback, not the rest-pose-only case ADR 0084
  already treats as normal. A controller with no `graph` is discarded
  silently, because it was not doing anything.

Both checks run before any file is written or command is applied. Nothing is
half-converted.

### 3. Baked geometry is bind pose, verbatim

Per §Context point 1, baking reads the render part's mesh sub-asset from its
owning model source, strips `skinning` and `tangents`, and keeps vertex
positions unchanged. No matrix math, no partial ECS conversion, and no
runtime `World` is needed to bake — only the model source's already-imported
`Mesh`.

### 4. One `.obj` file per submesh; multi-submesh parts become sibling entities

A render part with one submesh becomes one Static Mesh Renderer on the same
entity: its `engine.skinned_mesh_renderer` is removed and an
`engine.static_mesh_renderer` referencing the new baked mesh and the same
material is added in its place.

A render part with more than one submesh (ADR 0076) becomes one sibling
entity per submesh, each a Static Mesh Renderer with that submesh's own
material, sharing the original entity's parent and `engine.transform`. This
follows directly from §Context point 2: writing one `.obj` with `usemtl`
groups would produce a file the engine cannot read back correctly, since
`load_obj` does not parse multi-material groups anywhere else either. Adding
that parsing would be new reader surface built to serve one feature.

### 5. Baked meshes are author-owned project files, not derived-cache entries

Each baked mesh is written under `assets/baked_meshes/` and registered in the
asset manifest with a fresh `AssetId`, the same way `Create Retarget Map` and
`Create Material` register new hand-authored content. This is deliberately
**not** `crate::derived_cache::DerivedCache`: that store's contract is
"delete the whole directory any time, the next lookup rebuilds it"
(ADR 0079 §3), which only holds for content re-derivable from a
still-present source input. A bake result is meant to *replace* its source
relationship — after baking, the render part no longer refers back to the
model source or skin at all — so treating it as disposable cache content
would let ordinary cache-clearing maintenance silently delete a mesh the
scene still draws.

### 6. The model entity is not deleted

Baking removes `engine.skinned_model` (and, per §2, an unconfigured
`engine.animation_controller`) from the entity but leaves the entity itself,
its name, its `engine.transform`, and its position in the hierarchy exactly
as they were. What remains is an ordinary group entity with static children —
the same shape ADR 0074/0075 already produce for a model source that was
never skinned in the first place. No new "delete the model" behavior is
introduced; deleting it afterward is the existing entity-delete path.

## Consequences

- A frozen character, prop, or set piece stops presenting Skeleton, Skin, or
  Animation Controller pickers, matching how a never-skinned static prop
  looks.
- Baking needs no runtime `World`: it operates on the authoring scene and the
  model source's import result directly, so it works identically whether or
  not the scene is open in Play.
- A render part with several submeshes becomes several entities. This is
  visible in the Hierarchy and is the direct, honest consequence of §4 rather
  than a hidden merge.
- Once baked, a render part has no path back to its model source: reimporting
  the original glTF/GLB/FBX does not affect it. This is intentional — bake is
  the point at which an author is choosing to stop tracking the source — but
  it means bake is a one-way authoring decision, reversible only through the
  session's ordinary undo history, not through reimport.
- One new project-file convention (`assets/baked_meshes/`) and one new
  editor-only workflow. No component schema changes: baked entities use the
  existing `engine.static_mesh_renderer` unchanged.

## Alternatives Considered

### Bake into `DerivedCache`

Rejected; see §5. The cache's delete-anytime guarantee is exactly wrong for
content meant to outlive and replace its source.

### Write one `.obj` + `.mtl` with multi-material groups

Rejected; see §4. It requires new OBJ/MTL reader support the engine has
nowhere else, to serve exactly one feature.

### Introduce a native engine mesh format (e.g. `*.mesh.json`)

Rejected for this ADR. It would preserve submeshes, tangents, and vertex
color faithfully, but a new persisted format is a permanent compatibility
commitment (CLAUDE.md's serialized-format rule) for a feature whose actual
geometry needs — position, normal, UV, triangle list — are exactly what
`.obj` already carries through the engine's one existing mesh-file reader.

### Compute the baked pose from the runtime joint palette instead of relying on bind-pose identity

Rejected. It would require constructing a partial ECS conversion (spawning a
rig, running propagation and the joint-palette system) just to reproduce
values that are already identity by construction at bind pose. Reading the
model source's imported `Mesh` directly is simpler and needs no `World`.

### Delete the model entity and reparent its render parts elsewhere

Rejected. Nothing about baking requires moving anything in the hierarchy; the
entity keeps working as a plain group node, which is the same shape a
never-skinned prop already has.

## Compatibility and Migration

No schema changes. Baking is a client of the existing
`engine.static_mesh_renderer` (unchanged) and `engine.skinned_mesh_renderer`
/ `engine.skinned_model` (unchanged) schemas, applied as an
`AuthoringCommand` batch through the ordinary transaction path — one undo
entry, nothing written until the scene is saved except the newly baked
`.obj` files and their manifest entries, which are written immediately
because they are new project content, matching how `Create Material` and
`Create Retarget Map` already behave. No existing content is rewritten and no
existing file format changes.
