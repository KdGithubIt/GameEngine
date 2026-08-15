# Phase 37 — Animation Runtime / Clip Import

## Goal

Import animation clip assets from glTF.  An `Animator` component can play and
loop a clip on a skinned or transform-driven mesh entity at runtime.

## Why

Static scenes cover current use cases.  Simple animations (patrol paths, door
swings, character walk cycles) are the next content milestone and are required
before Animation Graph authoring (Phase 38) is meaningful.

**Prerequisite: Phase 36 (glTF Pipeline) for the import infrastructure.**

## Scope

| Item | Location |
|------|----------|
| Animation clip asset schema (keyframes, sampler, duration) | `crates/engine/src/animation.rs` (new) |
| glTF animation track import | `crates/engine/src/asset.rs` |
| `Animator` component with `play` / `loop` / `stop` | `crates/engine/src/animation.rs` |
| Sampler interpolation — linear only in v1 | `crates/engine/src/animation.rs` |
| Deterministic clip update (fixed-timestep schedule) | `crates/engine/src/animation.rs` |

## Key Constraints

- Sampler interpolation: **linear only** in v1.  Step and cubic-spline are
  deferred; the implementation must not make them harder to add.
- Clip playback runs on the fixed-update schedule for determinism
  (Phase 21 fixed timestep is the prerequisite).
- Depends on Phase 36 for the import path; glTF animation tracks share the
  file-level import pipeline.

## Completion Criteria

- Animation clip asset is importable from a glTF file.
- `Animator` component can play and loop a clip.
- Sampler interpolation (linear) and deterministic update are tested.

## Feeds Into

Phase 38 (Animation Authoring / Animation Graph — the authoring layer over
this runtime clip system).
