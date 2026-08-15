# ECS System Scheduling

The runtime has two managed ECS schedules: **Update** runs once per rendered
frame, and **FixedUpdate** runs zero or more times using the fixed timestep.
Rust `UiSystem` overlays are not included; they run later in the egui phase.

## System IDs and registration

Persistable systems use an explicit stable ID independent from their Rust
module path and display name. Engine IDs use `engine.` and project-native IDs
normally use `game.`.

```rust
#[engine::game_system(
    id = "game.health_decay",
    display_name = "Health Decay",
    schedule = "update",
    after = ["engine.player_controller"]
)]
fn health_decay(world: &mut engine::game_module::GameWorld) {
    let _ = world;
}
```

Use `aliases = ["game.previous_id"]` when renaming a persisted ID. Unnamed
`add_system` APIs remain available for tests and experiments, but their legacy
generated IDs should not be treated as long-lived project contracts.

## Final execution order

1. Keep saved IDs that still exist, in saved order.
2. Append newly registered systems in default registration order.
3. Ignore removed IDs and report diagnostics.
4. Resolve aliases to current IDs.
5. Apply before/after edges with a stable topological sort.

Preferred user order breaks ties between eligible systems. Missing targets are
warnings; cycles block runtime startup. Disabled systems remain registered and
retain their positions.

## Project settings and editor

Preferences are stored in project-root `project_settings.json` under a nested
`system_settings` object containing schema version 1 and separate `update` and
`fixed_update` `order` / `disabled` arrays.

Open **View → Systems** or the left-dock **Systems** tab to switch schedules,
search, filter Engine/Game entries, toggle enabled state, move entries, inspect
constraints, and reset defaults. The left dock shares its tab strip with the
scene **Hierarchy**, while the right **Inspector** remains visible. Writes are
atomic. Changes apply when the next Play runtime is created, never by mutating
the active Play world.

When a Game module cannot load, Engine systems remain visible with the load
diagnostic. Editing is read-only so unavailable Game IDs are not discarded.
Standalone packaged players apply the same settings after registration and
before their first update.
