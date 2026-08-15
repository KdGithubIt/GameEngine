# Phase 33 — Prefab / Reusable Entity Template

## Goal

Allow designers to save a selected entity (and its children) as a
`.prefab.json` asset and instantiate it multiple times in a scene with no
EntityId collisions.  Prefab instance override (changing individual fields of
an instance) is explicitly out of scope for v1.

## Why

Without prefabs every game object that appears more than once must be manually
re-created.  Prefabs are the baseline for reusable content in game editors.

## Scope

| Item | Location |
|------|----------|
| "Save as Prefab" action from selection | `crates/editor/src/prefab.rs` (new) |
| `.prefab.json` schema v1 (entity tree + component values) | **ADR required before implementation** |
| Prefab instantiation with EntityId remap | `crates/authoring/src/prefab.rs` (new) |
| Multiple instantiations with no ID collision | `crates/authoring/src/prefab.rs` |
| Prefab missing / invalid diagnostics | `crates/authoring/src/validation.rs` |
| Prefab drop from Asset Browser (Phase 32 infrastructure) | `crates/editor/src/prefab.rs` |

## Key Constraints

- **An ADR is required before any implementation.**  The `.prefab.json` schema
  touches serialized project data, and EntityId remap is a shared-contract
  decision (ADR 0028 §Decision 6).
- `EntityId::generate()` must be called for every instantiated entity; the
  prefab file must not store live runtime IDs.
- Prefab overrides (instance-level field changes) are deferred; the v1 design
  must not make them harder to add later.

## Completion Criteria

- ADR is Accepted and implementation follows it.
- A selection can be saved as a `.prefab.json` asset.
- A prefab can be instantiated multiple times without EntityId collision.
- Missing / invalid prefab diagnostics appear in the Problems panel (Phase 30).

## Feeds Into

Phase 34 (Project Settings — the Start Scene may reference prefabs as
entity templates).
