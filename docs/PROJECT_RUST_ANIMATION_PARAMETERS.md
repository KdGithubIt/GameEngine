# Typed animation parameters and Blend1D

## Animation Graph parameter authoring

Animation Graph parameters are authored directly in the editor. Open an Animation Graph and use the right-hand **Animation Parameters** section to create, rename, change the kind of, or delete declarations.

The supported kinds are:

- `Bool`: persistent state such as `grounded` or `guarding`.
- `Float`: finite scalar state such as movement speed or aim pitch.
- `Trigger`: one-shot state such as `attack`, `hurt`, or `dodge`.

A new graph starts without declarations. Add every parameter used by its transitions before selecting those transitions. Parameter names use Rust-style identifiers: a letter or underscore first, followed by letters, digits, or underscores.

The declarations are saved inside the Animation Graph. `Bool` values read `false`, `Float` values read `0.0`, and Triggers are not pending until project gameplay writes them.

## Typed transition Inspector

Select a State-to-State transition edge. The Inspector no longer accepts a free-text condition. Instead:

1. Choose **Unconditional** or one declared parameter.
2. For `Bool`, choose the expected `true` or `false` value.
3. For `Float`, choose `<`, `<=`, `>`, `>=`, `==`, or `!=` and enter a finite threshold.
4. For `Trigger`, no additional comparison is required.
5. Configure the transition fade or keep the Animation Controller default.

Renaming a declaration rewrites transitions that use it. Changing its kind or deleting it clears affected transition conditions to Unconditional so a stale condition cannot silently keep the previous type.

Legacy bare Bool text such as `moving` or `!grounded` is no longer editable in the Inspector. When an older free-text condition is encountered, the Inspector marks it as removed legacy data and requires selecting a declared parameter or Unconditional.

## Project Rust commands

Project gameplay writes parameter values through the normal deferred `Commands` API:

```rust
use engine::prelude::*;

fn update_animation(entity: Entity, mut commands: Commands) {
    commands.set_animation_bool(entity, "grounded", true);
    commands.set_animation_float(entity, "speed", 4.5);
    commands.trigger_animation(entity, "attack");
}
```

The name passed by gameplay must match the declaration in the Animation Graph. Use `set_animation_bool`, `set_animation_float`, and `trigger_animation` so the intended type is explicit.

A parameter keeps the type established by its first runtime write. Accidentally writing a boolean over a speed float therefore returns an error instead of silently changing graph semantics. Triggers remain pending until their matching transition is selected and then reset to `false`.

## Animation Motion Designer and Blend1D

The standalone Animation Motion Designer remains the tool for named Blend1D definitions:

- Windows: `./GameEngine/scripts/open-animation-motion-designer.ps1`
- Linux/macOS: `bash GameEngine/scripts/open-animation-motion-designer.sh`
- Cargo: `cargo run -p engine-editor --bin animation_motion_designer`

The Blend1D tab selects a Float parameter, edits threshold/motion rows, and shows live lower/upper motion weights while the parameter slider is moved. Validation prevents duplicate names, non-finite values, missing Float parameters, blank motions, and duplicate thresholds from being saved.

`engine::Blend1d` validates and sorts motion thresholds and samples the two neighbouring motions with normalized weights. Values outside the authored range clamp to the nearest endpoint:

```rust
use engine::prelude::*;

let locomotion = Blend1d::new(vec![
    Blend1dPoint {
        threshold: 0.0,
        motion: "idle".into(),
    },
    Blend1dPoint {
        threshold: 2.0,
        motion: "walk".into(),
    },
    Blend1dPoint {
        threshold: 6.0,
        motion: "run".into(),
    },
])?;

let sample = locomotion.sample(speed);
```
