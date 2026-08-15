# Project Rust Gameplay API (ABI v3)

Project gameplay sources live below `assets/scripts/rust/`. The project-local
`game/` crate is a generated Cargo build host. A system receives a
copied, query-scoped `GameInvocation` and writes only patches or deferred
requests to `GameInvocationOutput`. It never receives the host ECS `World`.

## Source layout and module paths

Everything below `assets/scripts/rust/` is one Rust module tree, and the folder
structure *is* the module path. Organize sources by feature however you like:

```text
assets/scripts/rust/
├─ player/
│  ├─ health.rs
│  ├─ movement/
│  │  └─ move_rule.rs
│  └─ attack.rs
├─ enemy/
│  ├─ health.rs
│  └─ ai.rs
└─ common/
   └─ math.rs
```

`assets/scripts/rust/player/movement/move_rule.rs` is reached as
`crate::player::movement::move_rule::MoveRule`. Two folders may each contain a
`health.rs`; only two entries in the *same* folder that would generate the same
module name are rejected.

Rules that still apply:

- `components/`, `resources/`, `systems/`, and `shared/` are created for new
  projects and are the default destinations of **Create Rust Script**. They are
  recommendations, not categories: any folder may hold any kind of source.
- Folder and file names must be usable as Rust module names — ASCII letters,
  digits, and `_`, starting with a letter or `_`, and not a Rust keyword.
  `player`, `player_movement`, `player2`, and `_player` are fine;
  `player-movement`, `Player Movement`, and non-ASCII names are rejected.
- `.rs` sources may be moved anywhere below `assets/scripts/rust/` but never
  outside it.
- **Do not create `mod.rs` below `assets/scripts/rust/`.** The engine owns module
  declarations and regenerates `game/src/project_modules.rs`, which
  `game/src/lib.rs` includes at the crate root, after every add, move, rename,
  or delete. A `mod.rs` below the Rust script root is reported as an error.

## How a source's kind is decided

The folder never decides what a source is. The declarations in the source do:

| Declaration | Kind |
| --- | --- |
| `#[derive(engine::GameComponent)]` or `#[game_component(...)]` | Component |
| `#[derive(engine::GameResource)]` or `#[game_resource(...)]` | Resource |
| `#[engine::game_system(...)]` | System |
| none of the above | ordinary compiled module |

An ordinary module needs no attribute and works in any folder:

```rust
pub fn calculate_damage() -> f32 {
    10.0
}
```

A component keeps its stable ID and every scene, prefab, and Inspector reference
when it moves, because the editor moves the adjacent `.rs.meta.json` sidecar in
the same operation and the ID lives only in that sidecar.

## Moving a source does not rewrite `use` paths

Moving `components/move_rule.rs` to `player/movement/move_rule.rs` changes its
module path, so a line such as `use crate::components::move_rule::MoveRule;`
stops resolving. The editor moves the files, regenerates the module index, and
warns that module paths changed; fixing the affected `use` lines is manual and
the next game build reports each one. Automatic reference rewriting is a
separate future refactoring feature.

## Declaring access

Every non-empty system defines a zero-argument access function. The manifest is
exported with module metadata and validated before Play starts.

```rust
use crate::components::health::Health;
use engine::game_io::{
    EngineViewKind, GameAccessMode, GameCommandFamily, GameComponentAccess,
    GameEngineViewAccess, GameQueryAccess, GameSystemAccess,
};

fn damage_access() -> GameSystemAccess {
    GameSystemAccess {
        input_actions: vec!["attack".to_owned()],
        queries: vec![GameQueryAccess {
            id: "game.query.damage_targets".to_owned(),
            components: vec![GameComponentAccess {
                component_type: <Health as engine::game_module::GameComponent>::schema().type_id,
                mode: GameAccessMode::Write,
                required: true,
            }],
            engine_views: vec![GameEngineViewAccess {
                view: EngineViewKind::Transform,
                required: true,
            }],
        }],
        command_families: vec![GameCommandFamily::Audio],
        ..GameSystemAccess::default()
    }
}
```

Use dotted, stable query and resource IDs. A write declaration allows patches
only for entity rows actually returned by that query. Remembered or guessed
runtime handles cannot be used to patch an entity omitted from the current
invocation.

## Implementing a callback

```rust
#[engine::game_system(
    id = "game.damage",
    display_name = "Damage",
    schedule = "fixed_update",
    access = damage_access,
)]
fn damage(
    input: &engine::game_io::GameInvocation,
    output: &mut engine::game_io::GameInvocationOutput,
) {
    let attack = input.input_actions["attack"];
    if !attack.just_pressed {
        return;
    }

    // Read query rows and append complete component replacement values to
    // output.component_patches. Host validation is atomic: if any patch or
    // command is invalid, none of this callback's output is applied.
    let _ = output;
}
```

Newly generated systems contain an empty access manifest and therefore cannot
observe or mutate gameplay accidentally. Add only the data and command families
the system needs.

## Project-wide runtime resources

Use `GameResource` for state shared by several systems but not attached to an
entity, such as mission phase or the current encounter state:

```rust
#[derive(Debug, Clone, Default, engine::GameResource)]
#[game_resource(id = "game.mission", display_name = "Mission State")]
pub struct MissionState {
    pub phase: i64,
    pub boss_active: bool,
}
```

In the editor choose **Create Rust Script → Resource**. The generated source
lives under `assets/scripts/rust/resources/` by default and may be moved
anywhere below the Rust script root afterwards; the generated module index is
rebuilt from the resulting tree. Resource and system registration IDs must stay
unique across the whole project even though file names need not be.

The derive exports a stable ID, field schema, and complete default value. A
system must still declare a `GameResourceAccess` as `Read` or `Write`; declared
values appear in `input.resources`, and a writer returns a complete
`GameResourcePatch`. Unknown resource IDs prevent the module from loading, and
malformed patches reject the complete callback output before any mutation.

Resources are host-owned and runtime-only. Installing a module generation
starts them from their exported defaults. They do not enter scenes, prefabs, or
save files automatically; copy deliberate persistence values through the Save
command family.

## Input actions and clocks

`input.input_actions` is resolved from `project_settings.json` immediately
before the callback. Digital bindings expose `pressed`, `just_pressed`, and
`just_released`; analog bindings expose deadzone-filtered `scalar` and the first
two configured axes as `vector`. A declared but missing action resolves to a
default inactive state; undeclared actions are omitted.

Keyboard names use `KeyA` through `KeyZ`, `Digit0` through `Digit9`, arrow keys,
Space, Enter, Escape, Tab, Shift, and Control names. Gamepad button indices 0–7
map to south/east/west/north, shoulders, select, and start. Axis indices 0–5 map
to left X/Y, right X/Y, and left/right triggers. Invalid bindings are ignored
with an Editor or Player diagnostic.

Mouse button names are `Left`, `Right`, `Middle`, `Back`, and `Forward`.
Gamepad axes apply their per-binding deadzone, optional inversion, and scale;
the first two valid axes fill the resolved vector components. `key_axes`
compose named negative and positive keys into X (`vector_component: 0`) or Y
(`vector_component: 1`) with an independent scale. Contributions are summed
and clamped to `[-1, 1]`. Older settings that omit these fields retain scale
`1`, non-inverted axes, and empty mouse/key-axis lists.

The invocation clock is copied from the runtime `Time` and `FixedTime`
resources. Catch-up fixed passes increment `fixed_step_index` before each pass,
so every fixed callback observes the exact simulation step it is running.

## Engine views and event streams

Entity queries can copy authoring identity, local/global Transform, character,
animation, lock-on, navigation, and UI-binding views. `UiBindings` is encoded
as a stable name-keyed object with string, number, and boolean values; the
engine resource itself never crosses the module boundary.

Declared event streams receive host-owned records with monotonically
increasing per-stream sequences. Collision records contain `enter`, `stay`, or
`exit`; animation records identify the firing entity and event name; UI records
contain the relayed document event name; scene records report completed or
failed transitions; emitted project events appear on the `Game` stream. Source
resources carry producer generations, so a project system ordered before a
producer cannot mistake stale data for a new fixed step or frame.

Return the highest sequence actually handled in
`consumed_event_sequences`. Cursors are isolated by system ID—one subscriber
cannot consume another's records—and acknowledging a sequence that was not in
the invocation rejects the complete output. The host retains at most 4,096
records and logs any overflow instead of allowing an unbounded queue.

Global engine state is declared separately in `host_views`. Request
`GameHostViewKind::SceneState` when a system needs the current scene path,
pending path, successful-switch generation, or latest switch failure even when
it did not observe the original Scene event. The view appears in
`input.host_views`; generation is a decimal string so all `u64` values cross
JSON without precision or signedness ambiguity. Event streams remain the right
choice for reacting once to transitions, while the host view is the source of
truth for joining or recovering state.

## Transform and despawn commands

Declare `GameCommandFamily::Transform` or `GameCommandFamily::Despawn`, then use
the typed constructors instead of assembling payload objects manually:

```rust
output.commands.push(engine::game_io::GameCommand::translate(
    row.entity,
    [0.0, 0.0, -input.clock.fixed_delta_seconds],
));
// GameCommand::set_transform(...), GameCommand::rotate(...), and
// GameCommand::despawn(...) use the same generation-checked handle.
```

The host decodes and preflights the complete command list before applying any
component/resource patch. Missing transforms, malformed or non-finite values,
stale targets, unsupported families, and commands after a despawn reject the
whole callback output. Valid commands execute in callback order while the host
has exclusive world access; the module never receives that access.

## Character and lock-on commands

`GameCommand::set_character_motion(handle, velocity, facing)` updates a live
`KinematicCharacterController` and its Transform facing only after both
components and both finite vectors have been preflighted. Facing is a non-zero
world-space direction; local `-Z` is rotated toward it. This keeps movement and
orientation in one atomic command instead of allowing a half-applied state.

Lock-on uses the targetless `acquire_lock_on`, `cycle_lock_on`, and
`release_lock_on` constructors. They queue the existing `TargetLock` service
request, preserving the engine rule that the last request before
`lock_on_system` runs wins. A missing service resource or malformed operation
rejects the complete callback output.

## Animation commands

`play_animation` resumes the current clip, `stop_animation` resets it, and
`crossfade_animation` switches to a clip runtime ID copied from an
`AnimationState` view. The ID is a decimal string in the copied `Value` object
so JSON cannot ambiguously normalize `u64` into `i64` across the ABI. Crossfade
IDs are deliberately process-local and are
validated against the current `Assets<AnimationClip>` store; project files
must never persist them. Duration must be finite and non-negative. Looping is
set in the same prepared operation as playback so invalid later commands cannot
leave a partially changed animator.

`set_animation_condition` writes a named boolean condition on a live
`AnimGraphPlayer`. Empty names, missing graph/animator components, stale
targets, and unloaded clips reject the entire callback output before mutation.

## UI commands

Use `set_ui_text`, `set_ui_number`, `set_ui_flag`, and `remove_ui_binding` for
the host-owned binding table. Names must be non-empty and numeric values must
be finite. These targetless commands are reflected by the copied `UiBindings`
view on the next callback and by UI document rendering on the next frame.

`set_ui_document_visible` targets a generation-checked entity containing
`UiDocumentRef`. The host attaches a runtime-only `UiDocumentVisibility`
override (absence remains visible for compatibility), so showing or hiding a
menu does not dirty or silently persist into the authored scene. Multiple
visibility commands in one callback are preflighted as a virtual batch and
apply in order.

## Scene commands

`request_scene("scenes/mission_02.scene.json")` records a transition in the
existing `SceneManager`; loading, validation, despawn, and spawn remain at the
next host frame boundary. The callback therefore cannot replace its own world
re-entrantly. Paths must be non-empty, project-relative `*.scene.json` paths
without parent/current-directory components. Completion and failure are
delivered on the declared `Scene` event stream with the resulting path and,
for success, scene generation.

## Audio commands

`play_sound_effect`, `play_background_music`, and
`crossfade_background_music` use stable `asset_...` IDs registered in the
project asset manifest. `stop_background_music` and the master/BGM/SE volume
constructors control the existing mixer. Asset IDs, finite fades/volumes,
manifest membership, and bounded queue capacity are checked during atomic
callback preflight.

Successful requests enter a dedicated 256-entry queue and are resolved by the
regular audio effect system, never by calling the device backend re-entrantly
inside the project DLL callback. The queue drains even in headless runs or when
the machine has no audio device, logging the external-service failure instead
of growing forever or failing unrelated gameplay state.

## Boundary and limits

- Entity handles contain both runtime ID and generation; stale handles fail.
- Input and output are deterministic JSON byte buffers, not Rust references.
- One invocation is limited to 16,384 total query rows, 4,096 event records,
  1 MiB input, 1 MiB output, and 1,024 commands.
- Read-only project components and engine views are never written back.
- Component and resource patches are validated completely before mutation.
- Commands are fully preflighted and applied through the exclusive host bridge;
  emitted events enter the bounded host log only after the callback succeeds.
  A callback cannot mutate engine services re-entrantly.
- `GameHostRuntime` records attempts, failures, callback duration, query-row
  count, input/output bytes, command count, and the latest error per system.
- ABI v1/v2 libraries are rejected with a rebuild diagnostic.

See ADR 0052 for the complete safety and ordering contract. Command-family
payload schemas and additional engine views are added as their ER-1 service
processors are implemented.
