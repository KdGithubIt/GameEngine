# Phase 32 — Drag & Drop Authoring UX

## Goal

Allow designers to drag assets from the Asset Browser onto the Scene View,
Hierarchy, or Inspector to assign or instantiate them without opening menus.

## Why

Menu-driven asset assignment is tedious for repeated operations (placing
meshes, assigning materials).  Drag-and-drop is the standard workflow in
production game editors and significantly reduces friction for content
authoring.

**Prerequisite: Phase 31 (Asset Database v2) for stable asset metadata.**

## Scope

| Drop source | Drop target | Result |
|-------------|-------------|--------|
| Mesh / material asset | Inspector `AssetRef` field | Assign asset to component field |
| Mesh asset | Scene View | Create new entity with Mesh + Transform at drop point |
| Mesh asset | Hierarchy | Create new entity as child of the entity under the cursor |
| Prefab asset | Scene View / Hierarchy | Instantiate prefab (requires Phase 33) |

All drop operations route through `AuthoringCommand` and are undoable.

## Key Constraints

- Every drop operation must produce an `AuthoringCommand` (undoable; see
  Phase 29 for the undo coalescing mechanism).
- Prefab drop is gated on Phase 33; it must not be implemented before that
  phase is complete.
- Depends on Phase 31 (Asset Database v2) for stable asset metadata.

## Completion Criteria

- An asset dropped onto an Inspector `AssetRef` field assigns the asset.
- A mesh / material asset dropped onto Scene View / Hierarchy creates an entity.
- All drop operations are undoable (one undo step per drop).

## Feeds Into

Phase 33 (Prefab — the drop-to-instantiate infrastructure is reused for
prefab placement).
