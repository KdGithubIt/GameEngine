# ADR 0047 — Runtime Scene Management and Switch Semantics

## Status: Accepted

Date: 2026-07-11

## Context

The engine can load one scene at startup (`SceneLoader`, Phase 18-A) and
reload it in place (18-B), but nothing can switch scenes at runtime: a game
cannot go from a title screen to a mission and back. Phase 18-C left a
`SceneManager` as optional scope, and the M1 milestone (Phases 53-63) makes
it mandatory: mission-loop games need title → hub → mission → result flow,
driven by game logic (UI events, scripts, BTs).

Constraints discovered in the current architecture:

- Frames are driven by `Ecs::update()` / `run_fixed_update()`; systems never
  get exclusive `&mut World` access, so a switch cannot run inside a system.
- Both hosts (standalone `EngineRunner`/player and editor Play) drive the
  same `App`, so the switch step must be a single `App`-level call.
- `spawn_from_authoring_scene` already returns the complete
  authoring-to-runtime entity map for a spawned scene.

## Decision

### 1. Requests are data; the switch runs at the frame boundary

A `SceneManager` world resource accepts `request_switch(relative_path)` from
any system (interior mutability is unnecessary: game systems access it via
`ResMut`). The actual switch happens in `App::process_scene_requests()`, a
host-called step that runs once per frame **before** schedule execution with
exclusive `&mut World` access. Both hosts call it; games never do.

### 2. Scene membership is tracked by the manager, not by components

The manager records the entity list returned by the scene bridge for the
current scene. A switch despawns exactly those entities; every other entity
(spawned by scripts or game systems) persists. This avoids a marker component
and gives a simple, explainable rule. A `generation: u64` counter increments
per successful switch so game systems can detect transitions.

### 3. Resources persist; assets stay cached

World resources (`UiBindings`, input, save data, asset stores) survive
switches. Meshes/materials loaded by previous scenes remain in their asset
stores as cache; eviction is future work and must not be silent (a future
`clear_unused` API). Rationale: correctness first — dangling handles are
worse than retained memory at M1 scale.

### 4. Load-then-despawn ordering; failures keep the old scene when possible

Switch order: read + parse + validate the new scene file first; only after
that succeeds, despawn the old scene and spawn the new one. File/parse/
validation errors therefore leave the current scene running untouched, with
the error recorded in a `SceneSwitchState` resource (`Idle` or
`Failed { path, message }`) and a `log::error!`. A mid-spawn bridge error
(rare: blocking asset error) leaves the new scene partially absent; the
manager despawns whatever the failed spawn created and reports `Failed` —
the game can request another switch (e.g. back to a menu scene). Switching
never panics.

### 5. Scenes are referenced by project-relative path

Scenes are `*.scene.json` files under the project root (ADR 0022/0023), not
manifest assets; the existing `SceneLoader` resolves relative paths. The
switch request therefore takes the same relative path string that
`ProjectSettings.start_scene` uses. Hosts insert `SceneLoader` (and the
manager) as world resources at startup.

### 6. Synchronous v1

Switches are synchronous within one frame. M1 scene sizes (arena missions)
load in milliseconds; async/streamed loading with a loading screen is a
future ADR. `SceneSwitchState` exists partly so a future async version can
add a `Loading` variant without breaking consumers.

## Consequences

- Title/hub/mission/result flow becomes possible from any game system; Rhai
  exposure arrives with Phase 60's script API.
- The editor Play host and player binary share one switch implementation and
  one set of semantics ("editor Play == packaged game" is preserved).
- Entities spawned by gameplay code deliberately survive switches; games
  that want them removed must despawn them on `generation` change. This is
  documented behavior, not an oversight.
- Asset memory grows monotonically across switches until an eviction API
  exists; acceptable at M1 scale and visible in `InstanceStats`/store sizes.

## Alternatives Considered

- **Marker component (`SceneMember`) on spawned entities** — rejected:
  duplicates state the bridge already returns, and mutating every spawn call
  site is a wider change than storing one list in the manager.
- **Multiple concurrently loaded scenes (additive loading)** — rejected for
  v1: M1 needs sequential flow only; additive loading changes entity-map and
  lighting-adoption rules and deserves its own ADR.
- **Despawn-then-load ordering** — rejected: a typo'd path would destroy the
  running scene and leave an empty world; validating first makes the common
  failure (bad path / bad JSON) harmless.
- **World-clearing switch (drop all entities and resources)** — rejected:
  would destroy persistent game state (score, save data, UI bindings) and
  the input/asset infrastructure that hosts installed.

## Compatibility and Migration

Additive: new `scene_manager` module, two new resources, one new `App`
method, host call-site additions. No serialized formats change. Phase 18's
`SceneLoader`/`reload_from_path` remain; `reload` becomes expressible as a
switch to the current path but is not removed.
