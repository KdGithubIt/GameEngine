# ADR 0095: Explicit GameComponent Authoring Fields

- Status: Accepted
- Date: 2026-07-28
- Deciders: GameEngine authoring/editor owners
- Supersedes: ADR 0093
- Related: ADR 0027 (component registry), ADR 0052 (game module safety contract), ADR 0066 (project component sidecar metadata)

## Context

A project component can contain two different kinds of data:

- authored settings that belong in the Inspector, scenes, and prefabs;
- runtime-only state such as timers, current targets, caches, and handles.

Treating every unannotated field as authored made an ordinary Rust field addition
silently change the persisted component schema. Authors also had to remember to
add `#[game_field(skip)]` to every runtime-only field.

Rust visibility is not persistence metadata. A `pub` runtime value may need to
be read by another module without being persisted, while a private tuning value
may still need Inspector authoring.

## Decision

Only a field marked with bare `#[game_field]` is authored:

```rust
#[derive(Default, engine::GameComponent)]
pub struct Enemy {
    #[game_field]
    max_health: f64,

    #[game_field]
    move_speed: f64,

    current_health: f64,
    current_target: Option<engine::game_io::GameEntityHandle>,
    cooldown_seconds: f64,
}
```

The rules are:

1. `#[game_field]` fields appear in the exported schema and Inspector and are
   persisted in scenes and prefabs.
2. Unmarked fields are runtime-only, do not need to implement `GameField`, and
   are restored from `Default` when authoring data is decoded.
3. `pub` and private visibility do not affect authoring or persistence.
4. `#[game_field(skip)]`, options, values, empty parentheses, and duplicate
   `game_field` attributes are compile errors.
5. A component with no `#[game_field]` fields is valid and has an empty authored
   object while retaining its runtime fields.

## Migration

Existing components must add `#[game_field]` to every field that must remain in
the Inspector or persisted scene and prefab data. Previous
`#[game_field(skip)]` attributes are removed; those fields become runtime-only
simply by remaining unmarked.

The editor's **Create Rust Script → Component** template marks its example
`enabled` setting explicitly. Existing source files are not rewritten
automatically because field persistence is a deliberate schema decision.

## Consequences

### Positive

- Adding an ordinary runtime field no longer changes the persisted schema.
- Persistence intent is visible at the field declaration.
- Rust visibility and editor authoring remain independent concerns.
- Runtime-only types are not forced through the authoring ABI.
- The rule has one marker and no component-level modes.

### Negative

- Authored fields require one explicit attribute each.
- Updating from the previous behavior requires a deliberate annotation pass.
- Removing `#[game_field]` is a persisted-schema change and must be handled like
  any other scene or prefab migration.

## Alternatives considered

- **Author every field by default and opt out with `skip`** — rejected because
  omission mistakes silently persist runtime state and field additions change
  schemas by default.
- **Use `pub` as the authoring rule** — rejected because module visibility and
  persistence intent are unrelated.
- **Add authoring/runtime modes at the component level** — rejected as extra
  policy and migration complexity when one explicit field marker is sufficient.
