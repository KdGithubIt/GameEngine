# Phase 28 — Scene Picking / Selection / Multi-select

## Goal

Allow a designer to click on an entity in the Scene View to select it.
Selection state syncs with the Hierarchy panel and the Inspector.  Shift/Ctrl
extends the selection.

## Why

Without picking, entity selection is limited to clicking in the Hierarchy list.
A visual editor that requires navigating a flat list to select objects is
unusable for scenes with many entities.

**Prerequisite: Phase 27 (Scene View panel) must be complete.**

## Scope

| Item | Location |
|------|----------|
| Ray-cast or GPU pick pass on Scene View click | `crates/editor/src/scene_view.rs` |
| `selected_entity: Option<EntityId>` shared state | `crates/editor/src/session.rs` |
| Scene View entity highlight (outline or debug axes) | `crates/editor/src/scene_view.rs` |
| Hierarchy panel scrolls to / highlights selected entity | `crates/editor/src/hierarchy.rs` |
| Inspector shows selected entity on click | existing inspector path |
| Shift / Ctrl multi-select (additive / toggle) | `crates/editor/src/selection.rs` (new) |
| Pick diagnostics (no-hit, ambiguous) | existing diagnostics pipeline |

## Key Constraints

- Picking strategy (CPU ray-AABB vs. GPU object-ID pass) is chosen at
  implementation time based on scene complexity targets; both are valid.
- Multi-select v1: additive selection only.  Group transform operations come
  in Phase 29.
- Selection state lives in the editor; the authoring scene is not mutated by
  selection changes.
- Picking must not panic on missing or deleted entities.

## Completion Criteria

- Clicking an entity in Scene View selects it.
- Selection syncs with Hierarchy, Inspector, and Scene View highlight.
- Shift / Ctrl extends or toggles selection.
- Pick diagnostics exist and are tested.

## Feeds Into

Phase 29 (Transform Gizmo — operates on the selected entity established by
this phase).
