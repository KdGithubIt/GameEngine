# ADR 0045 — Player Binary and Package Execution

## Status: Accepted

Date: 2026-07-05

## Context

ADR 0034 (Phase 39) defined the package layout (`game` executable plus an
`assets/` directory) and a pure `analyze_build` step, but left two gaps that
block an end-to-end packaging flow:

1. Editor projects are data-only (`project.json`, `project_settings.json`,
   `asset_manifest.json`, `assets/`). They have no `Cargo.toml`, so the
   `cargo build --release --manifest-path <project>/Cargo.toml` invocation
   in `build_project` cannot produce the packaged executable.
2. `MissingAsset` diagnostics are non-blocking in the v1 analysis, but the
   OSS pre-public plan (Workstream 3, Option B) requires missing required
   assets to block packaging.

## Decision

### 1. A generic `player` binary inside `crates/engine`

The packaged executable is a data-driven player, added as a `[[bin]]` target
`player` in the existing `engine` crate (`src/bin/player.rs`). No new crate
and no new third-party dependency are introduced; `engine` already depends
on `engine-authoring` for scene loading.

At startup the player resolves the package root (first CLI argument, or the
executable's directory when omitted) and:

1. Opens the project via `ProjectRoot::open` (validates `project.json`).
2. Loads `ProjectSettings`; a missing `start_scene` is a fatal startup error
   (exit code 2) because packaging guarantees it exists.
3. Loads `asset_manifest.json` (missing file degrades to an empty manifest
   with a warning, matching editor Play).
4. Loads the start scene through `SceneLoader`, inserts
   `AssetServer::with_assets_root` and the manifest, spawns via
   `spawn_from_authoring_scene`, and registers the same gameplay systems as
   editor Play (behavior trees, player controller, camera controllers,
   sample-game bridges, animation on the fixed schedule).
5. Runs the windowed `engine::App` loop.

### 2. Package planning is pure and copies are explicit

`plan_package(config, manifest) -> PackagePlan` (editor `build.rs`) reuses
the ADR 0034 analysis and additionally:

- **escalates `MissingAsset` to blocking** (packaging refuses to produce a
  package with holes; the editor-side analysis view keeps the v1
  non-blocking behavior),
- emits the concrete copy list `(source relative to project root →
  destination relative to output dir)`.

`package_project(config, manifest, player_binary)` executes the plan:

```text
<output_dir>/
  game.exe            (copy of the prebuilt player binary; `game` on POSIX)
  project.json
  project_settings.json
  asset_manifest.json
  assets/scenes/<all authored scene documents>
  assets/<manifest paths, directory structure preserved>
```

All regular files below `assets/scenes/` are copied recursively, including
scenes other than `start_scene`. A Rust GameModule can request a scene switch
by project-relative path at runtime, so copying only manifest entries or only
the start scene would allow packaging to report success while producing a
game that fails after its first transition. The start-scene path is still
added explicitly when the scenes directory is absent so package execution
reports the exact missing source file.

The caller supplies the prebuilt player binary path (in the source workspace
that is `target/<profile>/player[.exe]`). Building the player is a cargo
concern outside `package_project`, keeping the function testable with a
placeholder file.

### 3. Relationship to ADR 0034

This ADR extends ADR 0034: the layout gains the three project data files,
and `MissingAsset` becomes blocking **in the packaging path only**.
`analyze_build` and `build_project` keep their documented behavior for
compatibility; `build_project`'s cargo invocation is retained for source
projects that do carry a `Cargo.toml`.

## Consequences

- A clean end-to-end flow exists: build `player` once, then package any
  data project into a self-contained runnable directory.
- The player reproduces editor Play behavior because it registers the same
  systems; drift between "runs in editor" and "runs packaged" is limited to
  windowing differences.
- Packaged games require the player binary to be built for the target
  platform; cross-compilation is out of scope.
- `plan_package` is pure and unit-testable; `package_project` does file I/O
  only (no subprocess) and is integration-testable with temp directories.

## Alternatives Considered

- **Generate a Cargo project per package and build it** — rejected: slow,
  requires a Rust toolchain and the engine source tree at packaging time.
- **A separate `player` crate** — rejected for v1: adds a workspace crate
  and dependency edge for what one `[[bin]]` target provides; revisit if
  the player grows editor-independent features.
- **Embedding assets into the executable** — remains out of scope
  (ADR 0034).

## Compatibility and Migration

Additive: no persisted format changes, no public API removals. The
`BuildConfig`/`BuildReport` contracts are unchanged. Existing packages can be
rebuilt without migration; rebuilt output now contains every authored scene
needed by runtime switching.
