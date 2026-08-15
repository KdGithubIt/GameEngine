# Typed per-entity Rust systems

`#[engine::game_system(each)]` derives one entity query from the callback
signature and invokes the callback once for every matching runtime entity. It
is the concise default for behavior that operates independently on each entity.
The existing `#[engine::game_system]` plus explicit `Query<MyQuery>` API remains
available for algorithms that must compare several entity sets or control query
iteration manually.

## Basic movement

```rust
use crate::components::move_rule::MoveRule;
use engine::prelude::*;

#[engine::game_system(each)]
fn move_system(time: Time, rule: &MoveRule, transform: &mut Transform) {
    if rule.enabled {
        transform.translation.y += rule.move_y as f32 * time.delta_seconds();
    }
}
```

Normal references are required and combine as an AND filter. The example runs
only for entities containing both `MoveRule` and a local transform. Project
components are copied through the game-module boundary, and mutable values are
returned as validated component patches. `Transform` is likewise a local copy;
a mutable value becomes a validated `set_transform` command after the callback.
The project module never receives a live host-ECS reference.

## Signature rules

| Parameter | Meaning |
| --- | --- |
| `&T` | Require and read project component `T` |
| `&mut T` | Require, read, and replace project component `T` |
| `Option<&T>` | Read `T` when present without filtering the entity out |
| `Option<&mut T>` | Mutably expose `T` when present |
| `&Transform` / `&mut Transform` | Require a copied local transform |
| `Option<&Transform>` / `Option<&mut Transform>` | Optional copied local transform |
| `With<T>` | Require `T` without passing its value |
| `Without<T>` | Exclude entities that contain `T` |
| `AnyOf<(&A, &B, ...)>` | Require at least one listed component |
| `Entity` | Pass the generation-checked runtime entity handle |
| `Time`, `Action<T>`, `Commands`, etc. | Fetch the existing system parameter for each callback |

## Optional data

```rust
#[engine::game_system(each)]
fn movement_speed(
    rule: &MoveRule,
    boost: Option<&SpeedBoost>,
    transform: &mut Transform,
) {
    let multiplier = boost.map_or(1.0, |boost| boost.multiplier);
    transform.translation.y += rule.move_y as f32 * multiplier;
}
```

`SpeedBoost` does not participate in the required filter. Matching entities
without it receive `None`.

## OR and exclusion filters

```rust
#[engine::game_system(each)]
fn living_character(
    role: AnyOf<(&Player, &Enemy)>,
    _alive: Without<Dead>,
    entity: Entity,
) {
    if role.has::<Enemy>() {
        // Enemy-specific behavior.
    }

    if role.has::<Player>() {
        // Player-specific behavior.
    }

    let _ = entity;
}
```

The callback runs for `(Player OR Enemy) AND NOT Dead`. `AnyOf::get::<T>()`
returns a decoded owned copy when the value is needed instead of only a presence
check.

## Required marker without decoding

```rust
#[engine::game_system(each)]
fn regenerate(_player: With<Player>, health: &mut Health, time: Time) {
    health.value += 2.0 * time.delta_seconds();
}
```

Use `With<T>` when `T` is only a filter. A normal `&T` is clearer when the value
is also used.

## When to keep an explicit query

Use the existing regular system form when one callback must inspect multiple
entity collections, perform nested iteration, select nearest targets, or keep
manual control over patch timing:

```rust
#[engine::game_system]
fn targeting(
    attackers: Query<AttackersQuery>,
    targets: Query<TargetsQuery>,
    mut commands: Commands,
) -> Result<(), GameApiError> {
    // Compare the two deterministic query result sets.
    Ok(())
}
```

`each` is additive. Existing `GameQuerySpec` declarations and regular systems do
not change behavior.
