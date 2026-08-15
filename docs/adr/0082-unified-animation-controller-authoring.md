# ADR 0082: Unified Animation Controller Authoring

Status: Accepted (the compatibility period is ended by ADR 0091)
Date: 2026-07-22

The optional graph and implicit single-clip fallback decisions in this ADR
are superseded by ADR 0084. The Controller's ownership of the rig, and its
`skeleton` field naming a Skin sub-asset, are superseded by ADR 0087: the rig
belongs to the `engine.skinned_model` on the same entity. The unified
component and runtime expansion decisions remain in force.

> ADR 0091 removes the compatibility path described here. The legacy
> `engine.skeleton` / `engine.animator` / `engine.animation_graph_player`
> components are unregistered and deleted, so nothing can carry both the
> unified controller and the legacy pair.

## Context

Animated characters currently expose three authoring components:
`engine.skeleton`, `engine.animator`, and
`engine.animation_graph_player`. Their runtime responsibilities are distinct,
but exposing that decomposition in scene and prefab data makes authors, tools,
and AI agents maintain ordering and same-entity dependencies that the engine
can derive.

The engine already distinguishes immutable skeleton assets from per-character
pose instances. A skeleton asset can be shared by many characters, while each
character still needs an independent runtime pose. Several skinned meshes on
one character consume that same pose instance.

Animation Graph is the canonical controller for character animation. Imported
models still need a useful default without generating a user-owned graph file
for every source.

## Decision

### One public authoring component

New content uses `engine.animation_controller`. It owns the authoring
references and playback settings needed to construct one animated rig:

- `skeleton`: the imported Skin sub-asset that identifies the skeleton asset;
- optional `clip_source`: the registered model source or animation sub-asset;
- optional `graph`: an `anim.graph` asset;
- `clip_name`: the fallback clip used when `graph` is unassigned;
- looping, playback speed, events, completion event, root-motion mode,
  crossfade duration, and boolean parameter defaults.

The source model reference for the skeleton is derived from the Skin
sub-asset's manifest ownership. It is not persisted a second time.
When `clip_source` is unassigned, the controller still creates the rig and
leaves it in its rest pose. Assigning `graph` requires `clip_source`, because
the graph identifies clips by name within that source.

### Runtime expansion remains separated

Authoring-to-runtime conversion expands one `engine.animation_controller`
value into the existing runtime `Skeleton`, `Animator`, and `AnimGraphPlayer`
components on the same runtime entity. Runtime systems keep their existing
component access and scheduling boundaries.

Skinned meshes continue to reference the authoring entity that owns the
controller. At runtime that reference resolves to the entity carrying the
per-character `Skeleton` pose instance.

### Graph is always the runtime control model

When `graph` is assigned, its entry state selects the initial clip and the
graph owns subsequent transitions. When it is unassigned, conversion creates
an in-memory one-state compiled graph for `clip_name` (or the only clip in the
selected source). This implicit graph is runtime-derived data and is never
serialized.

This keeps a single runtime control path without forcing imported-model
prefabs to create and register a user-visible graph artifact.

### Compatibility

`engine.skeleton`, `engine.animator`, and
`engine.animation_graph_player` remain registered for loading existing scenes
and prefabs. They are hidden from new-component discovery and are deprecated
authoring forms. Existing documents are not rewritten merely by opening them.

Generated model prefabs use `engine.animation_controller` immediately. Tools
that scan scenes or prefabs for animation reachability recognize both the new
component and the legacy pair during the compatibility period.

Project gameplay APIs continue to expose animation through controller-level
commands and copied views. They do not expose runtime skeleton or graph-player
component references across the game-module boundary.

## Consequences

- Humans, CLI adapters, and AI agents author one animation component instead
  of coordinating three components.
- Runtime animation, skinning, graph evaluation, IK, and scheduling remain
  independently queryable inside the engine.
- A Skin sub-asset is sufficient to identify both the immutable skeleton asset
  and the source needed to instantiate a per-character pose.
- Simple imported animation and state-machine animation follow the same graph
  evaluation path.
- Compatibility code remains until a future migration removes the legacy
  component schemas.

## Alternatives Considered

### Merge all runtime state into one ECS component

Rejected. Animation sampling, graph evaluation, skinning, and IK require
different read/write access and schedule positions. One large component would
create unnecessary mutable-access conflicts without improving authoring.

### Group the three components only in the Inspector

Rejected. Scene JSON, prefabs, CLI, validation, and AI authoring would still
need to maintain the same three-way dependency.

### Require a persisted graph asset for a single clip

Rejected. It creates derived one-state files in ordinary project content and
makes imported models unusable until an otherwise redundant graph is created.
The in-memory one-state graph preserves one runtime path without asset clutter.

## Compatibility and Migration

`engine.animation_controller` is a new stable component type with schema
version 1. Existing component IDs and schemas remain readable. Generated
prefabs change because they are disposable import artifacts and are rewritten
on reimport under ADR 0075.

Author-owned scenes and prefabs may migrate explicitly by replacing a
same-entity legacy Skeleton/Animator/Animation Graph Player set with one
Animation Controller. No automatic file rewrite occurs in this ADR.
