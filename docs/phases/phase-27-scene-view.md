# Phase 27 — Edit Mode Scene View / Editor Camera

## Goal

Render the authoring scene in a dedicated Scene View panel while in Edit Mode,
so designers can see entity positions without starting Play.  Separate Scene
View from Game View at the UI level.

## Why

Currently the only way to see the scene rendered is to press Play.  Edit Mode
has no visual canvas, making transform editing and entity placement opaque.
Phase 17 stabilised the edit/play/save loop; Phase 27 adds the visual editing
surface on top of that stable foundation.

**Prerequisite: Phase 17 regression checklist must be green before starting
this phase.**

## Scope

| Item | Location |
|------|----------|
| Scene View panel (Edit Mode rendering) | `crates/editor/src/scene_view.rs` (new) |
| Editor-only camera state (not serialized to scene) | `crates/editor/src/scene_view.rs` |
| Camera pan / orbit / zoom controls (mouse + keyboard) | `crates/editor/src/scene_view.rs` |
| Grid / axis / ground-plane overlay | `crates/editor/src/scene_view.rs` |
| Scene View vs. Game View tab / split UX | `crates/editor/src/app.rs` |
| Edit Mode render path (offscreen texture, shared wgpu device) | `crates/editor/src/scene_view.rs` |

## Key Constraints

- Editor camera state must **not** be serialized to the scene file.  It is
  purely editor-local state.
- Scene View rendering reuses the same wgpu device/queue as Game View
  (unified after Track W).
- **Game View role:** runtime preview (Play only).
- **Scene View role:** authoring canvas (Edit only).

## Completion Criteria

- Edit Mode renders the authoring scene in the Scene View panel.
- Editor camera state is editor-local and is not written to `*.scene.json`.
- Grid / axis / ground-plane overlays are visible.
- Scene View and Game View are visually distinguished in the UI.

## Feeds Into

Phase 28 (Scene Picking — needs the Scene View panel to receive mouse clicks
and report hit entity IDs).
