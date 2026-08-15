# Phase 39 — Build / Packaging / Distribution

## Goal

Generate a runnable desktop package from an editor project.  The Start Scene
(Phase 34 Project Settings) and all transitively referenced assets are bundled.
Missing required assets block the build and appear in the Problems panel
(Phase 30).

## Why

The editor has no export path.  A game currently exists only as source files
runnable through `cargo run`.  Distribution requires a self-contained
executable with bundled assets.

**Prerequisites: Phase 34 (Start Scene in Project Settings) and Phase 31
(Asset Database v2 for reachability analysis).**

## Scope

Exact scope depends on the ADR.  Likely items:

| Item | Location |
|------|----------|
| Asset reachability walk from Start Scene | `crates/editor/src/build.rs` (new) |
| Package layout: executable + `assets/` directory | `crates/editor/src/build.rs` |
| Missing-asset detection → build blocked → Problems panel | `crates/editor/src/build.rs` |
| `cargo build --release` invocation | `crates/editor/src/build.rs` |

## Key Constraints

- **An ADR is required before implementation.**  Packaging touches the CLI/MCP
  surface (build invocation), the asset embedding strategy, and potentially the
  runtime asset-loading contract.  These are shared-contract decisions
  (ADR 0028 §Decision 6).
- Depends on Phase 34 (Start Scene) and Phase 31 (Asset Database v2).
- **WASM packaging is out of scope for v1.**

## Completion Criteria

- ADR is Accepted and implementation follows it.
- An editor project generates a runnable desktop package.
- The Start Scene and all transitively referenced assets are included.
- Missing required assets block the build and appear in the Problems panel.

## Feeds Into

Phase 40 (AI Agent Bridge — the packaged runtime is a target for AI
observation and virtual input injection).
