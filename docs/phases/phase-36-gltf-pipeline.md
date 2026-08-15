# Phase 36 — glTF / GLB Asset Pipeline

## Goal

Import `.gltf` and `.glb` files as static mesh assets, including their
embedded materials and textures.  Imported asset dependencies are recorded in
the Asset Database.  Animation data is deferred to Phase 37.

## Why

OBJ (Phase 14) covers basic geometry, but modern game assets use glTF for
materials, textures, and animations.  glTF is the industry-standard
interchange format and is required for the Animation phases (37–38).

**Prerequisite: Phase 35 (Material / Lighting Editor) for the material and
texture asset model.**

## Scope

| Item | Location |
|------|----------|
| `gltf` crate integration | `crates/engine/Cargo.toml` |
| Static mesh import (vertices, normals, UVs, indices) | `crates/engine/src/asset.rs` |
| Material and texture import from glTF extras | `crates/engine/src/asset.rs` |
| Deterministic sub-asset IDs within a single glTF file | design constraint |
| Import diagnostics (missing texture, unsupported extension) | `crates/authoring/src/validation.rs` |
| Dependency recording in Asset Database (Phase 31) | `crates/authoring/src/asset_manifest.rs` |

## Key Constraints

- Sub-asset IDs (mesh, material, texture within one file) must be
  **deterministic**: re-importing the same file produces the same IDs so that
  scene references remain stable across reimports.
- Animation data present in glTF files is silently ignored until Phase 37.
- Depends on Phase 31 (Asset Database v2) for dependency recording.

## Completion Criteria

- A `.gltf` / `.glb` static mesh is importable and visible in Scene View and
  Game View.
- Imported material / texture dependencies are recorded in the Asset Database.
- Sub-asset IDs are deterministic; import diagnostics are tested.

## Feeds Into

Phase 37 (Animation Runtime — consumes glTF animation tracks from the same
import infrastructure).
