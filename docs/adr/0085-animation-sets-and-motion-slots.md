# ADR 0085: Animation Sets and Stable Motion Slots

Status: Accepted (the legacy clip binding is removed by ADR 0091)
Date: 2026-07-22

> ADR 0091 removes the compatibility path described here. An
> Animation Graph state carries `motion_slot` only; the load-only
> `clip_id` binding, the `anim.ambiguous_motion_binding` diagnostic, and
> the in-memory slot-list reconstruction for pre-slot graphs are gone.

## Context

The Animation Controller introduced by ADR 0082 resolves every graph state by
clip name from one `clip_source`. That prevents one graph instance from using
clips imported from different glTF, GLB, or FBX source files. It also couples
reusable state-machine behavior to source-local names even though imported
animation clips already have stable sub-asset IDs and source-skeleton binding.

The model-source contract from ADR 0029 remains desirable: one registered model
file owns one atomic import catalog, while meshes, materials, skeletons, skins,
and clips remain independently addressable derived sub-assets.

## Decision

### Source containers and first-class sub-assets

Registered glTF, GLB, and FBX files remain the top-level source and reimport
unit. Imported mesh, material, texture, skeleton, skin, and animation entries
remain nested manifest sub-assets with deterministic `AssetId` values. They are
not emitted as ordinary author-owned files merely to make them addressable.

An imported animation sub-asset is a first-class `AssetRef` target. Animation
authoring must not require a reference to the whole model source plus a
source-local clip-name string.

### Stable motion slots

Animation graph states reference `MotionSlotId` values using the
`motion_<ULID>` format. A slot ID remains stable when its display name or bound
clip changes. The existing `clip_id` state property remains readable only for
legacy Controller/Graph Player content.

Playback semantics that belong to graph behavior, including looping, state
speed, transition fade, and completion transitions, belong to graph states or
transitions rather than the entity-level Controller.

### Animation Set asset

An author-owned `*.animset.json` asset binds one graph's motion slots to
imported animation-clip `AssetId` values. One set may bind clips owned by any
number of model sources. Bindings may add author-owned timeline events without
modifying generated import data.

The Animation Controller references:

- the target skeleton used by the character pose;
- one Animation Graph;
- one Animation Set implementing that graph's motion slots;
- instance settings such as enabled state, global playback speed, root-motion
  consumption, and parameter overrides.

Graph and Animation Set are either both assigned or both absent. The absent
case creates a rest-pose-only rig.

### Resolution and retargeting

During authoring-to-runtime conversion, every Animation Set binding resolves
independently. A clip bound to the target skeleton is used directly. A clip
bound to another skeleton requires the explicit Retarget Map selected by the
existing source/target pair rules from ADR 0079. Retargeted clips remain
derived cache/package artifacts and are not committed as author-owned assets.

The runtime expansion remains decomposed into focused ECS components. The
public Animation Controller is not a mandate to merge the runtime pose,
Animator, graph instance, root-motion request, or event queue.

### Packaging

Build reachability follows Scene or Prefab Animation Controller references to
the graph, Animation Set, every bound imported clip, each owning model source,
and every required Retarget Map and baked clip. Dynamic set or clip assignment
must use the existing explicit always-package/dependency mechanism rather than
depending on an untracked runtime file lookup.

## Consequences

- One graph can use clips imported from multiple model files.
- The same graph and Animation Set can drive multiple target skeletons; each
  binding is retargeted independently when necessary.
- Graph behavior can be reused with another Animation Set without duplicating
  the graph.
- Imported model files stay atomic reimport sources and generated project-file
  clutter does not increase.
- Editor, validation, preview, Play conversion, and packaging must resolve
  stable motion slots rather than one source-local clip-name table.

## Alternatives Considered

### Store direct clip references in every graph state

Rejected as the sole model. It solves cross-source selection but hard-codes
content into graph behavior and requires a second override system to reuse the
same graph for characters with different motion collections.

### Generate standalone mesh and clip files for every imported model element

Rejected. It creates duplicate sources of truth, noisy version-control diffs,
and reimport ownership ambiguity. Explicit user extraction may create an
author-owned copy in the future, but automatic import output remains derived.

### Allow multiple `clip_source` fields on the Controller

Rejected. It preserves source-local string lookup, introduces name collisions,
and does not provide a reusable contract between graph logic and content.

## Compatibility and Migration

`engine.animation_controller` advances to schema version 3 and replaces
`clip_source` with `animation_set`. Legacy Controller values and legacy graph
`clip_id` properties remain readable through compatibility schemas and paths;
new content writes motion slots and Animation Sets. Existing source and
sub-asset IDs, model import settings, Retarget Maps, and cache-key inputs do not
change.
