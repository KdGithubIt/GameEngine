# ADR 0087: Skinned Model and Renderer-to-Model Rig References

Status: Accepted (the deferred field removal is completed by ADR 0091)
Date: 2026-07-26

> Two later changes amend this ADR.
>
> The renderer-to-model reference replaced both §1's `render_parts` owner list
> and §2's `rig_source` override. `engine.skinned_model` carries only
> `skeleton`; `engine.skinned_mesh_renderer.model` is the single authoritative
> binding, so rig resolution is that field alone, ownership is derived by
> reverse lookup rather than stored (ADR 0089 §1 reads it that way), and §6's
> delete-what-the-list-names rule no longer applies.
>
> ADR 0091 carries out the removal deferred under
> "Delete the legacy `skeleton` fields now": the version 1 renderer
> `skeleton` read path, the version 3 controller `skeleton` field, the
> legacy `engine.skeleton` / `engine.skinned_mesh` components, and the
> **Convert Legacy Rigs to Skinned Models** command are all removed.
> **Resync Model Parts** (§5) is unaffected.

## Context

ADR 0086 separated the runtime rig pose from the per-mesh skin binding, so one
joint hierarchy can serve several skins. The authoring model also needs to
separate the entity that owns the rig from the entities that draw meshes.

The old renderer field was named `skeleton`, accepted every scene entity, and
forced authors to understand runtime internals. An intermediate design stored
an authoritative `render_parts` array on Skinned Model and an optional
`rig_source` override on each renderer. That produced two ways to express the
same relation, made the visible list directly editable, and made an
unassigned renderer incorrectly look like a missing required value.

## Decision

### 1. Skinned Model owns only the rig

`engine.skinned_model` has one required field:

```text
skeleton: AssetRef -> imported Skeleton sub-asset
```

The component creates one runtime `Skeleton` on its own entity. It does not
serialize renderer ownership. The Inspector shows renderers that currently
reference the model as a read-only reverse-derived list.

### 2. Each renderer explicitly selects one model

`engine.skinned_mesh_renderer` has an optional:

```text
model: EntityRef -> entity carrying engine.skinned_model
```

This field is the single authoritative relation. The Inspector picker lists
only compatible entities and provides a Clear action. An unassigned value is
a valid editing state: the imported mesh remains visible in bind pose and a
non-blocking diagnostic explains that no deformation rig is assigned.

Hierarchy is independent of rig selection. Reparenting a renderer does not
change the model it uses, and one model may be referenced by renderers
anywhere in the scene.

Deleting or removing a Skinned Model clears references from surviving
renderers rather than deleting them through this relation. When a generated
renderer is also a child of a deleted model entity, the editor reparents that
renderer branch outside the deleted subtree, preserves its world transform,
and clears `model`. A renderer explicitly included in the deletion selection
is still deleted.

### 3. Animation Controller uses the model on the same entity

`engine.animation_controller` carries no authored model, rig, Skin, or
Skeleton reference. At conversion time it uses the runtime rig created by
`engine.skinned_model` on the same authoring entity. A controller without that
component reports `scene.component_dependency_missing`.

Animation Set, Animation Graph, playback speed, root motion, fade duration,
events, and parameter defaults are unchanged.

### 4. Import and resync derive the relation

Generated FBX/glTF prefabs emit one Skinned Model per imported skeleton and
write its entity ID into each generated renderer's `model` field. The
generated renderers may also be children for transform and organization, but
the parent link is not used for rig resolution.

Reimport does not rewrite placed scene structure. **Resync Model Parts**
reverse-scans the selected model's renderer references and adds only missing
source meshes. Existing renderers, materials, transforms, enabled flags, and
unmatched author content are preserved.

### 5. Runtime conversion

Conversion creates all model rigs first. For each renderer it then:

1. installs mesh and material data;
2. resolves `model`;
3. adds `SkinnedMesh` and `JointPalette` when the rig resolves;
4. otherwise leaves the mesh in bind pose and emits a recoverable diagnostic.

The legacy fields `rig_source` and `skeleton` are accepted as compatibility
aliases after `model`, in that order. They are not exposed by the current
schema.

### 6. Reference and asset pickers use human-readable catalogs

EntityRef fields declare required target component types. Both picker
filtering and validation enforce the requirement. Skeleton AssetRef fields
include imported Skeleton sub-assets in the normal asset catalog, displaying
the source and sub-asset names while retaining stable IDs only for storage.

## Consequences

- The relation has one editable source of truth: renderer to model.
- Model Inspector lists cannot create duplicates or disagree with renderers.
- Renderers can be reassigned, cleared, or survive model-component removal.
- A model can legitimately have no renderers, for example when used only by
  attachments or animation logic.
- Animation Controller remains simple and cannot accidentally select a Skin
  or a renderer.
- The runtime ECS separation from authoring data is preserved.

## Alternatives Considered

### Store `render_parts` on Skinned Model

Rejected. It makes model deletion and resync convenient, but makes the
renderer-to-rig relation indirect, requires an editable array of internal
entity IDs, and conflicts with an optional renderer-side override. Reverse
lookup provides the useful list without duplicated serialized state.

### Resolve the nearest model by walking the hierarchy

Rejected. Reparenting would silently change animation behavior and the
binding would not be explicit in scene data or diagnostics.

### Store both model list and renderer reference

Rejected. Synchronization rules cannot prevent temporarily contradictory
authored values, and neither side has a principled reason to win.

### Put render parts inside the model component

Rejected. Transform, enabled state, LOD, materials, selection, and arbitrary
game components are entity-level concepts and must remain available per part.

## Compatibility and Migration

`engine.skinned_model` stores only `skeleton`.
`engine.skinned_mesh_renderer.model` is optional.
`engine.animation_controller` uses the co-located model.

The explicit migration command performs these rewrites:

| Legacy value | Current value |
| --- | --- |
| `skinned_mesh_renderer.skeleton` | `skinned_mesh_renderer.model` |
| `skinned_mesh_renderer.rig_source` | `skinned_mesh_renderer.model` |
| `skinned_model.render_parts[]` | corresponding renderer `model` fields |
| `animation_controller.skeleton` = Skin | co-located Skinned Model `skeleton` |

Loading, importing, and saving never silently rewrite authored documents.
Compatibility aliases keep legacy scenes playable until the author runs
**Project -> Convert Legacy Rigs to Skinned Models** and saves.
