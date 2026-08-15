# Phase 34 — Project Settings / Tags / Layers / Input Actions / Start Scene

## Goal

Define project-wide settings as a versioned document: Tags, Layers, Input
Actions (action-name-to-key bindings), and the Start Scene.  This phase is the
implementation home for Phase 12-D (Action Mapping), deferred per ADR 0028
§Decision 4.

## Why

`PlayerController` currently reads `Input<KeyCode>` directly; remapping
requires code changes.  Tags and Layers have no project-level definition,
making tag-usage validation impossible.  A centralized settings document lets
editor UI, runtime lookup, and validation share one source of truth.

## Scope

| Item | Location |
|------|----------|
| `project_settings.json` schema (Tags, Layers, Input Actions, Start Scene) | **ADR required before implementation** |
| Editor Project Settings panel | `crates/editor/src/project_settings.rs` (new) |
| `PlayerController` migrated from `KeyCode` direct to action-name lookup | `crates/engine/src/player.rs` |
| Start Scene shared by Play, Build, and Validation | `crates/editor/src/session.rs`, `crates/engine/` |
| Validation: Start Scene exists and is valid | `crates/authoring/src/validation.rs` |

## Key Constraints

- **An ADR is required before implementation.**  The binding data model,
  persistence format, defaults, editor UX, and runtime lookup contract must
  all be frozen before code is written (ADR 0028 §Decision 4 and §Decision 6).
- Phase 12-D in `IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` remains a deferral
  note pointing here until this phase is complete.
- The `project_settings.json` schema must carry `schema_version` (ADR 0020
  pattern) to support future migrations.

## Completion Criteria

- ADR is Accepted.
- Tags / Layers / Input Actions / Start Scene are stored in `project_settings.json`.
- `PlayerController` uses action mapping instead of `KeyCode` direct reference.
- Start Scene is shared by Play, Build (Phase 39), and Validation (Phase 30).

## Feeds Into

Phase 39 (Build / Packaging — Start Scene and input bindings are required for
packaging a runnable game).
