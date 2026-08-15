# ADR 0093: Runtime-only GameComponent Fields

- Status: Superseded by ADR 0095
- Date: 2026-07-27
- Superseded: 2026-07-28
- Deciders: GameEngine authoring/editor owners
- Related: ADR 0027 (component registry), ADR 0052 (game module safety contract), ADR 0066 (project component sidecar metadata)

## Historical decision

This ADR introduced `#[game_field(skip)]` while fields without the attribute
remained authored by default. It allowed runtime-only caches and handles to be
omitted from the Inspector and persisted scene or prefab data.

## Superseding decision

ADR 0095 replaces the opt-out rule with explicit authoring:

```rust
#[derive(Default, engine::GameComponent)]
pub struct MoveRule {
    #[game_field]
    speed: f64,

    runtime_cache: RuntimeCache,
}
```

Only bare `#[game_field]` fields are now authored. Unmarked fields are
runtime-only and restored from `Default`. `#[game_field(skip)]` is no longer
accepted.

The explicit rule was selected because adding an ordinary Rust field should not
silently change a component's persisted schema. Rust visibility remains
independent from Inspector and persistence behavior.
