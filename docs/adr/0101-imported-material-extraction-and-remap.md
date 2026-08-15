# ADR 0101: Imported Material Extraction and Remap

- Status: Accepted
- Date: 2026-08-13

## Context

Imported model sources (glTF/GLB/FBX) surface their embedded materials as
`Material`-kind sub-assets (ADR 0076) so they can be dragged into a Scene
View or an Inspector field, but the sub-asset's persisted record
(`ImportedSubAsset`) carries only an ID, kind, name, and index — never the
actual shading values. The runtime `Material` for such a sub-asset is
re-derived on every load by reimporting the source (`import_gltf_cached` in
`crates/engine/src/scene_bridge/asset_load.rs`) and pulling the baked
`GltfMaterialData` for that index. There is consequently no surface for
editing shader, color, or texture values on an imported material: the
Material Editor only opens standalone `.material.json` files by path.

A user reported this directly while trying to adjust the shader on an
imported material sub-asset. The fix requires a real design decision because
it crosses the `engine`/`editor` boundary: whatever mechanism lets an author
edit an imported material has to be visible to both the editor's Scene View
preview and the standalone Player binary, which reads the same
`asset_manifest.json` through the same `engine::scene_bridge` loading code
the editor uses — it does not read editor-only state such as
`.engine/editor/`.

Unity, Unreal, and Godot all converge on the same shape for this problem:
an imported material becomes editable by **extracting it into an
independent, first-class asset file**, with the import keeping a mapping
from the original slot to the extracted file so every existing reference
continues to resolve correctly without being reassigned. None of the three
keeps an editable value embedded in import metadata that gets edited in
place.

## Decision

`ImportSettings` (`crates/engine/src/asset.rs`) gains a new optional field:

```rust
/// Imported material sub-asset ID → standalone Material asset ID.
#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
pub material_remaps: BTreeMap<String, String>,
```

This is a v2 manifest extension in the same spirit as ADR 0029: additive,
optional, and empty for every existing project.

**Extraction** (editor-only, triggered from the Inspector's sub-asset panel
when a `Material` sub-asset is selected):

1. Re-run `engine::import_model_path_with_contact_bones` synchronously
   against the source to get the sub-asset's current baked `MaterialAsset`.
2. Write it to a new standalone `<name>.material.json` file under
   `assets/materials/`.
3. Register that file as an ordinary top-level Material asset with a fresh
   `AssetId`.
4. Insert `material_remaps[sub_asset_id] = extracted_asset_id` on the
   source's `ImportSettings` and persist the manifest.
5. Open the new file in the Material Editor.

**Resolution** (`load_material_asset`,
`crates/engine/src/scene_bridge/asset_load.rs`): before reimporting the
source for a `Material`-kind sub-asset, check
`entry.import_settings.material_remaps.get(&sub_asset.id)`. If present,
resolve that ID instead — recursing into the same function, which now takes
the standalone-file branch already used for ordinary Material assets. This
reuses that branch's general-purpose texture resolver
(`decode_material_texture`), which resolves both ordinary top-level Texture
assets and imported Texture sub-assets. A top-level Texture is decoded from
its registered file. An imported Texture sub-asset is resolved back to its
owning PMX/glTF/FBX source, loaded through the existing shared import cache,
and decoded through `decoded_imported_texture`. An extracted material can
therefore keep every texture inherited from its source while also accepting
any independently registered Texture in the project. Reimport of the source
model is skipped when the extracted material has no imported Texture
references; otherwise the cached source import supplies those texture pixels.

Editor UI: the Inspector's "Selected Sub-asset" panel shows "Extract to
Editable Material..." for an un-remapped Material sub-asset, or "Open in
Material Editor" / "Reset to Imported" once extracted. Resetting removes the
manifest entry (a non-destructive pointer removal, mirroring "Reset to
Source Name" for sub-asset renames) without deleting the extracted file.

Reimport interaction: sub-asset IDs are derived deterministically from
source ID, kind, and index (ADR 0075/0077), so a reimport that keeps the
same material count and order keeps the same ID and the remap keeps
applying automatically — this is the entire point, matching how Unity/
Unreal/Godot extraction survives a mesh-only reimport. If a reimport removes
or reorders a material such that its derived ID disappears, its
`material_remaps` entry becomes orphaned; this is intentionally left
in place (harmless — it is simply never looked up again) rather than
pruned automatically, since eager pruning could delete a mapping the
author intended to reattach after fixing the source.

Scope: only `Material`-kind sub-assets are extractable in this iteration.
Mesh, Texture, and Animation sub-assets are out of scope — they are baked
geometry/keyframes/pixels rather than an authored value tree, so "extract
and edit in place" does not apply to them the same way.

## Consequences

- Both the editor's Scene View preview and the standalone Player binary
  observe the same edited material, because both read
  `entry.import_settings.material_remaps` through the same
  `load_material_asset` function and the same `asset_manifest.json`.
- Editing an already-extracted material without imported Texture references
  does not reimport the source model. When it retains imported Texture
  references, the owning source is loaded through the existing conversion and
  Scene View caches, so all slots reuse one parsed source and one decoded image
  per Texture sub-asset.
- Every existing reference to the original sub-asset ID (e.g. a
  `MeshRenderer`'s material slot) keeps working unmodified — nothing needs
  reassignment.
- `asset_manifest.json` gains one small string-to-string entry per
  extracted material, plus one new top-level entry and one new
  `.material.json` file per extraction; this is opt-in and only appears for
  materials an author actually chose to edit.
- The standalone material branch accepts both top-level and imported Texture
  IDs. Imported textures reuse the same source-import and decoded-texture
  caches as the direct imported-material branch instead of duplicating pixel
  extraction or writing redundant texture files.

## Alternatives Considered

### Embed an editable override value directly in `ImportedSubAsset`/`ImportSettings`

Rejected. This was the first design explored. It would require either
duplicating `decode_material_texture`'s general texture resolution for the
override path or building a second one, would let an override reference a
texture the standalone-material path could already resolve for free, and
does not match how any of the three surveyed engines solve this problem.
Storing a live value in metadata instead of a first-class asset also has no
natural home for future material-editing features (previews, dependency
tracking, `Register`/`Reimport` context menus) that already exist for
ordinary Material assets.

### Store the override under `.engine/editor/`, editor-only

Rejected. The standalone Player binary does not read `.engine/editor/`, so
this would either make Player output blind to overrides — breaking "what
you see in the editor is what ships" — or force the Player runtime to learn
to read editor-UI-only state, contradicting its established meaning (see
the sub-asset display-name override, which is deliberately editor-only and
cosmetic).

### Duplicate to a standalone material without a remap

This is the smaller-scope alternative offered before this ADR — extract a
copy but require the author to manually reassign every reference. Rejected
because it breaks reference identity for no benefit once the remap
mechanism is available; every engine surveyed avoids exactly this
limitation by remapping the slot instead.

## Compatibility and Migration

- Additive, optional field on `ImportSettings`; existing `asset_manifest.json`
  files parse unchanged and continue to serialize without the field when no
  material has been extracted (`skip_serializing_if`).
- No change to `StableId` format, sub-asset ID derivation, or any existing
  `ImportedSubAsset` field.
- `docs/AI_FRIENDLY_AUTHORING_SPEC.md` is not updated: consistent with prior
  `ImportSettings` extensions (ADR 0077, 0080, 0099), that spec documents the
  hand/AI-authored Scene/Prefab/UI Document surface, not `ImportSettings`
  internals, which the importer and editor UI generate and no author writes
  by hand.
