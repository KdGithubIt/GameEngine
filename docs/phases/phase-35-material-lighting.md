# Phase 35 — Material / Lighting / Environment Editor

## Goal

Allow designers to create, edit, and assign material assets from the editor.
Texture assets are resolved via the manifest / import settings (Phase 31).
Lighting and environment settings are reflected in both Scene View and Game
View.

## Why

Materials are currently created programmatically.  There is no editor workflow
to assign textures, change base color, or adjust lighting without writing code.
This phase completes the texture-manifest resolution gap deferred from Phases
14-D and 16.

**Prerequisite: Phase 31 (Asset Database v2) for texture import settings.**

## Scope

| Item | Location |
|------|----------|
| Material asset schema (base color, roughness, texture refs) | `crates/authoring/src/material_asset.rs` (new) |
| Material editor panel | `crates/editor/src/material_editor.rs` (new) |
| Texture asset resolution via manifest / import settings | Phase 31 prerequisite |
| GPU texture upload from manifest-resolved path | `crates/engine/src/asset.rs` |
| Lighting settings panel (`AmbientLight`, `DirectionalLight`) | `crates/editor/src/environment.rs` (new) |
| Environment settings reflected in Scene View (Phase 27) | scene view render path |

## Key Constraints

- Texture manifest resolution for GPU upload was deferred from Phases 14-D and
  16; this phase is the implementation target for that gap.
- Depends on Phase 31 (Asset Database v2) for the import settings contract.

## Completion Criteria

- A material asset can be created, edited, and assigned from the editor.
- A texture asset is resolved via the manifest and applied to a material in the
  Game View and Scene View.
- Lighting / environment settings are reflected in both views.

## Feeds Into

Phase 36 (glTF import — imported materials and textures use this asset model).
