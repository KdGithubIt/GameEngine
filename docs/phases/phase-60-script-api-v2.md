# Phase 60: Script API v2

## Goal

Expose command-oriented Rhai APIs for entity lookup and lifetime, runtime
prefab spawning, animation, audio, UI bindings, lock-on control, scene
switching, timers, and deferred events without exposing raw engine state.

## Scope

- `find_entity` / `find_entities`, `spawn_prefab`, and `despawn`
- `play_anim` and `set_anim_condition`
- `play_se`, `play_bgm`, and `stop_bgm`
- `ui_set` and `ui_remove`
- `lock_target`, `cycle_target`, and `release_target`
- `request_scene`
- one-shot named timers
- targeted events and named subscriptions

Prefab overrides, async loading, arbitrary ECS queries, new lifecycle hooks,
and Audio v2 mixing are out of scope.

## Design Decisions

ADR 0049 defines the typed command queue, next-pass event delivery, runtime
identity, frame-boundary prefab processing, and queue caps. Existing Script API
functions remain compatible. Invalid commands log errors and do not panic.

## Completion Criteria

- Every listed API has focused tests.
- Prefab spawning reuses `PrefabAsset` and `spawn_from_authoring_scene`.
- Existing Phase 42-59 behavior remains compatible.
- fmt, clippy, test, and rustdoc workspace gates pass without warnings.
