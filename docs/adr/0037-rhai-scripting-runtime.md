# ADR 0037: Rhai Scripting Runtime

Status: Accepted
Date: 2026-06-14
Target Phase: Phase 42

## Context

Phase 42 adds a scripting layer for scene-specific behavior and designer-facing
iteration. The engine already has Rust-native ECS components and systems; the
missing piece is a lightweight script layer that can be authored and adjusted
without rebuilding Rust code.

The scripting layer should keep the engine easy to build across native and
wasm32 targets, preserve a narrow sandbox boundary, and integrate cleanly with
Rust-owned ECS types and editor-defined component schemas.

## Decision

The MVP scripting language is **Rhai via the `rhai` crate**.

Rhai does not replace Rust native gameplay code. Rust native components and
systems remain first-class and continue to own formal ECS component types,
high-performance processing, reusable gameplay modules, and engine architecture.

Rhai is treated as an Entity-attached `ScriptComponent` layer. Its role is
similar to Unity's `MonoBehaviour`: scene-specific behavior, triggers,
cutscene events, UI flow, and prototype gameplay.

Unlike Unity, the MVP does not add a new Rust `DoorControllerComponent`,
`EnemyAIComponent`, or similar bespoke component for every behavior. The common
runtime shape is:

```text
ScriptComponent { script: AssetRef, enabled, state }
```

The `script` field references a `.rhai` script asset. Adding or editing `.rhai`
script assets does not require `cargo build`. A Rust build is required only when
the engine/editor implementation changes, a Rust native component/system is
added, or the Rust-side `ScriptContext` API is extended.

`ScriptComponent` may keep private per-script-instance state. That state is
isolated from other entities and scripts unless explicitly exposed through the
approved API.

Rhai scripts may read and write approved Rust/editor-defined components through
`ScriptContext`. The API surface is limited to a command/event-oriented
`ScriptContext` facade.

Defining new ECS component types directly in Rhai is out of scope for the MVP.
Editor-defined data-only components may be considered in a later phase, but
they are not part of Phase 42.

## Lifecycle Hooks

The MVP supports only these `ScriptComponent` lifecycle hooks:

- `on_start(ctx)`
- `on_update(ctx, dt)`
- `on_event(ctx, event)`

The runtime skips a hook when the script does not define that function. Missing
hooks are not errors.

The following hooks are future/optional and not part of the MVP contract:

- `on_collision_enter(ctx, other)`
- `on_trigger_enter(ctx, other)`
- `on_enable(ctx)`
- `on_disable(ctx)`

## ScriptContext Boundary

The MVP `ScriptContext` API is deliberately small and command/event-oriented.
It may expose APIs shaped like:

- `ctx.self()`
- `ctx.log(message)`
- `ctx.input_pressed(action)`
- `ctx.send_event(target, event_name)`
- `ctx.get_component(entity, component_name)`
- `ctx.set_component(entity, component_name, value)`
- `ctx.param(name)`

Rhai scripts must not receive direct access to:

- raw ECS `World`
- raw `AssetStorage`
- filesystem APIs
- network APIs
- process APIs
- arbitrary module/package loading
- arbitrary script-side `require` / `import`
- free-form heavy ECS queries inside script code

## Script Assets, State, and Hot Reload

When a `.rhai` file is saved, the script asset is reloaded and recompiled into a
Rhai AST. Scripts must not compile during the frame update path.

Compiled ASTs are cached per script asset. When the same script asset is
attached to multiple entities, the AST is shared and each entity receives its
own `ScriptInstance` and private state.

For the MVP, hot reload may reset private script state. State that is important,
serialized, inspectable, or part of stable gameplay behavior should live in a
Rust native component or in a future editor-defined data component. Script
private state should be limited to temporary timers, local flags, and small
phase/progress values.

## Diagnostics and Profiler Policy

The scripting runtime must make `ScriptComponent` cost visible. The MVP records:

- total script time per frame
- per-script execution time
- per-entity script execution time
- per-hook execution time for `on_start`, `on_update`, and `on_event`
- script compile time
- last / average / max execution time
- slow script warnings
- script parse/runtime error diagnostics

Profiler/debug mode may additionally record:

- Rhai operation count
- `ScriptContext` API call counts for `get_component`, `set_component`,
  `send_event`, `input_pressed`, and `log`
- max operations exceeded
- per-frame top N slow scripts

The intended presentation is:

```text
Script Profiler

Total script time: 1.42 ms / frame

Top Scripts:
1. enemy_ai.rhai        0.82 ms   120 calls   45,000 ops
2. door_interact.rhai   0.12 ms    30 calls    3,200 ops

Entity:
Slime_023 / enemy_ai.rhai
  on_update last: 0.031 ms
  avg: 0.018 ms
  max: 0.071 ms
  ctx calls:
    get_component: 3
    send_event: 1
```

## Safety Limits

The runtime must configure Rhai with `Engine::set_max_operations` so runaway
scripts are stopped. Operation limit violations are reported to Console /
Diagnostics.

Detailed operation profiling has overhead, so it may be limited to
editor/debug/profiler mode. Release runtime should prioritize wall-clock timing
and the maximum operation limit.

## Workload Boundary

Rhai is appropriate for:

- triggers
- simple state machines
- cutscene events
- UI flow
- scene-specific behavior
- prototype gameplay

The following remain Rust responsibilities:

- pathfinding
- physics
- collision
- animation sampling
- massive ECS queries
- many-entity updates
- renderer
- reusable gameplay modules

## Promotion Policy

Rhai is for prototyping, lightweight behavior, and scene-specific logic. Heavy
behavior should not stay in Rhai indefinitely. The script profiler and
diagnostics must make it possible to identify heavy scripts, heavy entities,
and heavy lifecycle hooks.

The `ScriptContext` API stays small and command/event-oriented. Rhai scripts do
not receive raw ECS `World` access. Rhai and Rust should be able to use the same
engine-level commands and events where practical, so heavy logic can move
behind a Rust native function, Rust system, or Rust component without changing
the script-facing workflow more than necessary.

When script behavior becomes performance-critical, reusable, or part of stable
gameplay logic, it should be promoted to a Rust native component/system.
Promotion is a manual or AI-assisted port to Rust implementation, not a fully
guaranteed automatic conversion.

A one-click `.rhai` to Rust component/system converter is not an MVP goal, and
the design must not depend on automatic conversion. A future helper such as
`engine-cli promote-script` may be considered to generate Rust scaffolding, but
behavior-preserving, fully guaranteed conversion remains a non-goal.

## Consequences

- `ScriptEngine` is a Rhai / `rhai` runtime wrapper.
- Script assets use `.rhai`; the canonical example path is
  `assets/scripts/enemy_ai.rhai`.
- `ScriptComponent` is the common attach point for script behavior.
- The engine must define a narrow, documented `ScriptContext` API before script
  code can mutate game state.
- Sandbox policy is part of the runtime contract, not an editor-only concern.
- Script diagnostics and profiling are part of the MVP contract, not an
  optional later tool.
- Phase 46 / ADR 0041 still verifies wasm32 build behavior and feature gates,
  but Rhai is the scripting runtime baseline.

## Alternatives Considered

### Lua 5.4 via `mlua`

Lua has strong game-scripting precedent and broad familiarity, but `mlua`
adds a native scripting runtime dependency and a larger wasm32/build-gating
surface. It was not selected for the MVP because the project now prioritizes
Pure Rust integration, dependency simplicity, and a tighter sandbox boundary.

### Wren

Wren is small and game-oriented, but it adds a less common scripting language
for designers and has a smaller Rust ecosystem than Rhai.

### Rust-only gameplay

Keeping all gameplay in Rust preserves type safety and performance, but it does
not solve the editor-side iteration problem for scene-specific behavior,
cutscenes, triggers, UI flow, and prototype gameplay.

## Compatibility and Migration

No existing serialized scene, graph, asset manifest, or command format changes
are made by this ADR alone. Phase 42 implementation must introduce
`ScriptComponent` and `.rhai` script assets with explicit schema/version
handling where persistence is added.
