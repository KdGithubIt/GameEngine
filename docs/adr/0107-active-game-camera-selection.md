# ADR 0107: Active Game Camera Selection

Status: Accepted
Date: 2026-08-13

Amendment: ADR 0115 removes the version-1 camera compatibility reader described
below. The current `engine.camera` authoring shape requires both `enabled` and
`priority`; missing selection fields are rejected instead of being interpreted
as `true` and `0`. The original Compatibility and Verification sections are
retained here as the historical decision that applied when this ADR shipped.

## Context

The Game View, packaged player, camera-relative movement, and LOD selection
previously took the first camera yielded by an ECS query. Query order follows
runtime archetype storage and is not an authoring contract, so adding a second
camera could change presentation and gameplay behavior without an explicit
choice by the author.

Other engines separate camera projection data from selection. Godot maintains
one current camera per viewport, Unreal centralizes the final view in its
player camera manager, and Unity Cinemachine arbitrates enabled cameras by
priority. Bevy exposes active state, order, and render targets for its broader
multi-output rendering model.

This engine currently has one game output and no camera stacking, split-screen,
or render-texture authoring. It needs deterministic single-camera selection
without prematurely defining those larger rendering features.

## Decision

`Camera3D` owns two runtime and authorable selection fields:

- `enabled: bool`, defaulting to `true`;
- `priority: i32`, defaulting to `0`.

Every consumer of the game camera applies the same ordering:

1. Exclude cameras whose `enabled` value is `false`.
2. Prefer the greatest `priority`.
3. Resolve equal priorities by ascending runtime `Entity` ID and generation.

The final tie-breaker is deterministic within a runtime world. Authoring
validation warns when multiple effectively enabled cameras share the greatest
priority, because runtime identity must not be used as persisted intent.

Selection is implemented as a pure shared resolver rather than a cached world
resource. Frame systems, fixed systems, rendering, and host snapshots can call
it at different times without observing a stale derived selection.

The selected camera is used by:

- Game View and packaged-player rendering, including shadow-camera input;
- camera-relative player movement;
- LOD distance selection;
- lock-on target-selection parameters; and
- the primary camera exposed by the project physics-query snapshot.

Camera controller systems continue updating non-selected cameras. This keeps a
standby follow, orbit, or lock-on camera ready before it becomes active.
`camera_aspect_system` also continues updating every camera because all cameras
target the same game viewport in the current single-output model.

The editor-owned Scene View camera remains explicit and bypasses game-camera
selection, preserving ADR 0103's non-mutating observation contract.

## Missing-Camera Behavior

When a scene contains no camera component, Editor Play and the packaged player
retain the existing temporary default-camera fallback.

When camera components exist but all are disabled, hosts do not insert a
temporary camera. Disabling every camera is explicit authoring intent, and
validation reports `validation.no_active_camera` instead.

## Compatibility and Migration

The `engine.camera` authoring schema advances from version 1 to version 2.
Version-1 values omit both selection fields and are interpreted as
`enabled = true` and `priority = 0`. No eager scene migration is required.

A pre-existing scene with one camera behaves unchanged. A pre-existing scene
with multiple cameras receives `validation.camera_priority_tie` until the
author assigns distinct priorities or disables standby cameras. Runtime
selection is deterministic even while that warning remains.

The project physics snapshot adds the two fields to its engine-owned transient
camera value. Its decoder accepts snapshots that omit them with the same
compatibility defaults.

No scene schema version, stable identifier, prefab format, asset manifest, or
authoring command semantics change.

## Alternatives Considered

**Store only `main: bool`.** Rejected because multiple main cameras still need
an arbitration rule, and a temporary camera override must mutate two cameras
to switch and restore the view safely.

**Persist a scene-level camera entity reference.** This is strict but expands
the scene schema, transaction surface, prefab semantics, and additive-scene
rules. It is larger than the current single-output requirement.

**Cache an `ActiveGameCamera` resource.** Rejected because the resource is
derived state whose update order would have to remain synchronized across
frame systems, fixed systems, host snapshot compilation, and rendering.

**Render every enabled camera by order.** Rejected for this phase because it
defines camera stacking, clear behavior, viewports, and render targets that the
renderer and authoring model do not yet support.

## Verification

- A higher-priority enabled camera wins regardless of spawn or archetype order.
- A disabled camera never wins, even with a greater priority.
- Equal priorities choose the lower runtime entity deterministically and emit
  an authoring warning at the greatest priority.
- Rendering, camera-relative movement, LOD, lock-on, and physics queries resolve
  the same camera.
- Version-1 camera values spawn as enabled at priority zero.
- A scene with cameras that are all disabled does not gain a temporary camera.
