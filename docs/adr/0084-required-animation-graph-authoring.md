# ADR 0084: Required Animation Graph Authoring

Status: Accepted
Date: 2026-07-22

## Context

ADR 0082 introduced `engine.animation_controller` as the single public
animation authoring component. Its original compatibility design allowed a
controller to select one clip directly and synthesize an in-memory graph.
That path makes direct clip playback and graph-controlled playback behave as
two authoring models even though the runtime always evaluates graph state.

## Decision

An animation controller that has a `clip_source` MUST also have an authored
`anim.graph` asset. The graph's Entry State selects the initial clip and all
subsequent state changes go through `AnimGraphPlayer`.

`clip_name` and the implicit one-state runtime graph are removed from the new
Animation Controller authoring contract. A one-state `anim.graph` file remains
valid because it is an explicit graph asset.

A controller with neither `clip_source` nor `graph` remains valid for a
skinned rig that is intentionally kept in its rest pose. A controller with
only one of those references is invalid.

Generated model prefabs do not select or autoplay their first imported clip.
They create the rig in its rest pose until an author assigns both a Clip Source
and an Animation Graph.

The Asset Browser exposes `Create → Animation Graph` and creates the semantic
graph, its editor view, and its manifest entry in the currently selected asset
folder.

## Consequences

- Every authored animation playback path is graph-driven and inspectable.
- Existing scenes that used only `clip_source` require an explicit graph.
- Imported models no longer create implicit playback state.
- Runtime `Animator` and `AnimGraphPlayer` remain separate ECS components.
- Legacy `engine.skeleton`, `engine.animator`, and
  `engine.animation_graph_player` remain load-only compatibility schemas.

## Alternatives Considered

### Generate a persisted one-state graph for every imported model

Rejected for this change. It replaces an implicit graph with generated project
content and still hides the controller design from the author.

### Remove the runtime Animator

Rejected. Graph evaluation selects states, while Animator owns clip time,
sampling, events, looping, and crossfade playback state.

## Compatibility and Migration

The Animation Controller schema advances to version 2. Existing values with a
persisted `clip_name` remain readable as data, but `clip_name` is ignored. An
existing controller with `clip_source` and no `graph` produces a blocking
dependency diagnostic and must be migrated by assigning a graph. No automatic
scene rewrite is performed.
