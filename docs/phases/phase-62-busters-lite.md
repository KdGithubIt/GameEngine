# Phase 62: Vertical Slice - busters_lite

Status: Implemented (2026-07-12)

## Goal

Prove the M1 action-RPG line with an editor-openable project and no engine-core
changes.

## Deliverable

- `examples/busters_lite/` is the authoring source of truth.
- `crates/engine/examples/busters_lite.rs` is a game-specific runtime bridge.
- The bridge loads the project through `ProjectRoot`, `ProjectSettings`,
  `SceneLoader`, `AssetManifest`, and `spawn_from_authoring_scene`.
- Scene roles are declared by `example.busters_lite_role`; the example maps
  those authoring roles to example-only runtime components.

## Mission Loop

1. Title screen.
2. Mission briefing and sortie.
3. Arena combat with one player, two AI allies, and three enemies.
4. Mission-clear or defeat result.
5. Successful results persist clear count and clear time to save slot 0.

Combat includes WASD movement, lock-on target cycling, a three-step melee
combo, ally pursuit/attacks, enemy pursuit/attacks, HUD, and pause overlay.

## Boundary

The phase adds no new engine component, system, serialized engine format, or
third-party dependency. Game-specific state and systems remain in the example.
The project remains inspectable and editable in the normal editor.

## Automated Acceptance

- The example test loads the authoring project and verifies exactly one player,
  two allies, and a three-member enemy group.
- The mission-state test verifies title -> briefing -> combat progression.
- `cargo check -p engine --example busters_lite` compiles the runnable slice.

## Run

```text
cargo run -p engine --example busters_lite
```
