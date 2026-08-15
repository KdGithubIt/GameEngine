# ADR 0083: Unified Mesh Renderer Authoring

Status: Accepted (the Skinned Mesh Renderer `skeleton` field is superseded
by ADR 0087; the compatibility schemas are removed by ADR 0091)
Date: 2026-07-22

> ADR 0087 replaces `engine.skinned_mesh_renderer.skeleton` with an optional
> `model` EntityRef. The renderer-to-model reference is the sole authoritative
> binding.
>
> ADR 0091 carries out the migration this ADR left to "a future explicit
> migration": the legacy Mesh, Material, Material Slots, and Skinned Mesh
> components are unregistered and deleted, the compatibility aliases for the
> renderer's rig reference are gone, and no diagnostic reports mixing the two
> forms because only one form exists.

## Context

Static rendering currently exposes `engine.mesh`, `engine.material`, and
`engine.material_slots` as separate authoring components. Skinned rendering
adds `engine.skinned_mesh`, while particles carry their mesh but depend on the
standalone material component. These runtime-oriented pieces make ordinary
scene authoring harder to understand and allow incomplete combinations that
do not describe one clear drawing operation.

The underlying ECS separation remains useful. Mesh handles are shared by LOD,
skinning, particles, and other draw paths, while materials and joint palettes
have different mutation and scheduling requirements. The authoring model does
not need to expose that runtime decomposition.

## Decision

### Unified public renderer components

New static content uses `engine.static_mesh_renderer`, with these fields:

- `mesh`: the mesh asset to draw;
- `material`: the base material, defaulting to the built-in white material;
- `material_slots`: ordered optional per-submesh material overrides.

New skinned content uses `engine.skinned_mesh_renderer`, with these fields:

- `mesh`: the imported mesh sub-asset to deform and draw;
- `skeleton`: the authoring entity that owns the runtime pose;
- `material`: the base material, defaulting to the built-in white material;
- `material_slots`: ordered optional per-submesh material overrides.

An entity must not combine either unified renderer with the legacy mesh,
material, material-slot, or skinned-mesh components. It must also not contain
both unified renderer types.

### Runtime expansion remains separated

Authoring-to-runtime conversion expands a static renderer into the existing
runtime mesh handle, material, and material-slot components. A skinned
renderer additionally creates the existing skinned-mesh binding and joint
palette. Rendering systems and GPU resource ownership do not change.

The mesh asset remains independently shareable. LOD, static drawing, skinned
drawing, particles, and future draw paths can resolve the same immutable mesh
asset into their own runtime components without sharing authoring renderer
state.

### White material fallback

`asset_01JP0000000000000000000203` is the stable built-in white material.
Unified renderer schemas use it as the material default. This makes a newly
added renderer visible without requiring a separate material component while
still serializing an explicit, category-valid asset reference.

Particle Emitter schema version 2 adds its own `material` field with the same
default. Version 1 particle values that omit the field remain readable and
continue to use the runtime material fallback.

### Editor and import behavior

The Add Component menu exposes Static Mesh Renderer and Skinned Mesh Renderer.
The legacy Mesh, Material, Material Slots, and Skinned Mesh definitions remain
registered but are hidden from new-component discovery.

Primitive creation, mesh drag-and-drop, new-project starter content, and glTF
generated prefabs create unified renderer values. Dropping a material onto a
unified renderer or particle emitter updates its `material` field. Dropping a
material onto an unchanged legacy entity continues to update or add the
legacy standalone Material component.

Generated model prefabs are disposable import artifacts and switch to the new
components immediately. Author-owned scenes and prefabs are not rewritten
automatically.

## Consequences

- A normal draw operation is represented by one discoverable authoring
  component instead of several coordinated components.
- Mesh-only and material-only replacement remain field edits on that one
  component.
- Missing material setup produces a visible white result by default.
- Runtime ECS systems retain focused components and existing query behavior.
- Legacy documents continue to load, but new and legacy renderer forms cannot
  be mixed on one entity.
- Compatibility schemas and bridge paths remain until a future explicit
  migration removes them.

## Alternatives Considered

### Merge mesh and material into one runtime ECS component

Rejected. It would couple asset sharing, LOD selection, skinning, material
preparation, and rendering queries without improving the public authoring
experience beyond what bridge expansion already provides.

### Keep separate components and group them only in the Inspector

Rejected. Scene JSON, prefabs, CLI tools, validation, and AI authoring would
still need to coordinate the split representation.

### Make material optional in serialized renderer values

Rejected for new schemas. An explicit built-in white reference keeps schema
validation and asset-category inspection straightforward. Runtime fallback is
retained for legacy data and defensive rendering behavior.

## Compatibility and Migration

Both unified renderer components start at schema version 1. Particle Emitter
moves to schema version 2 by adding an optional-at-load material field with a
schema default. Existing stable component IDs and serialized values remain
readable.

An author-owned legacy entity may migrate explicitly by replacing its Mesh,
Material, Material Slots, and optional Skinned Mesh set with one corresponding
unified renderer. No automatic file rewrite occurs in this ADR.
