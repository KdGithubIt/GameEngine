# Phase 29 — Transform Gizmo / Duplicate / Copy Paste

## Goal

Add interactive Translate / Rotate / Scale gizmos to the Scene View so
designers can reposition entities without typing numbers in the Inspector.
Duplicate and copy/paste produce entities with correct new StableIds.

## Why

Without direct-manipulation tools every transform change requires field-level
number entry.  Gizmo editing is the baseline workflow for 3D content creation.

**Prerequisite: Phase 28 (Scene Picking) must be complete.**

## Scope

| Item | Location |
|------|----------|
| Translate / Rotate / Scale gizmos (3-axis handles) | `crates/editor/src/gizmo.rs` (new) |
| Gizmo mode toggle (T / R / S shortcuts) | `crates/editor/src/app.rs` |
| Snap-to-grid option | `crates/editor/src/gizmo.rs` |
| Gizmo operation → `AuthoringCommand` (undoable) | `crates/editor/src/gizmo.rs` |
| Duplicate selected entity (`Ctrl+D`) | `crates/editor/src/commands.rs` |
| Copy / Paste entities with new StableIds | `crates/editor/src/commands.rs` |
| Persistence: save → reload retains edits | existing save path |

## Key Constraints

- Each gizmo drag produces **one** undo step, using the undo coalescing
  mechanism from Phase 15-E (drag preview is local; commit on release).
- Duplicate and copy must call `EntityId::generate()` — never reuse existing
  StableIds.
- Gizmo renders into the Scene View panel established in Phase 27.

## Completion Criteria

- Translate / Rotate / Scale gizmos act on the selected entity.
- One gizmo drag produces exactly one undo step.
- Duplicate / copy / paste generate new stable IDs with no collision.
- Save and restart preserves all edits.

## Feeds Into

Phase 30 (Console / Problems) and Phase 32 (Drag & Drop, which needs the
Scene View canvas established by Phases 27–29).
