# Data-driven abilities in project Rust

## Ability Designer GUI

Authors do not need to edit ability JSON by hand. Open a project in Engine Editor,
select **Authoring Tools**, then open **Ability Designer**. The designer appears as
a modeless window inside the current editor process.

The designer provides:

- New, Open, Save, and Save As
- add, duplicate, delete, and select ability definitions
- typed non-negative timing fields for startup, active, recovery, and cooldown
- duplicate stable-ID and invalid-value validation before saving
- a scrub-able timeline preview showing the active phase and relative duration

Files are saved as pretty JSON using `engine::AbilityLibrary`. Project Rust loads
the same type, so the GUI and runtime share one schema and one validation path.

## Runtime use

`engine::AbilityMachine` is the reusable timing core for attacks, dodges, skills,
and interactions. It deliberately owns only startup, active, recovery, and
cooldown timing. Project systems still decide which input activates an ability
and which engine commands enable hitboxes, movement, animation parameters,
audio, particles, or UI.

A project component marks only its authored tuning fields. The unmarked machine
is runtime-only state:

```rust
use engine::prelude::*;

#[derive(Debug, Clone, Default, GameComponent)]
pub struct LightAttack {
    #[game_field]
    pub startup_seconds: f64,

    #[game_field]
    pub active_seconds: f64,

    #[game_field]
    pub recovery_seconds: f64,

    #[game_field]
    pub cooldown_seconds: f64,

    machine: AbilityMachine,
}
```

Build one `AbilityDefinition` when the attack starts, call `activate`, and then
call `tick(time.fixed_delta_seconds())` from a fixed-update system. React to
phase transitions instead of duplicating timers:

```rust
for event in attack.machine.tick(time.fixed_delta_seconds()) {
    match event.to {
        AbilityPhase::Active => commands.enable_hitbox(hitbox.handle()),
        AbilityPhase::Recovery => commands.disable_hitbox(hitbox.handle()),
        AbilityPhase::Ready => {
            // The ability may be activated again.
        }
        _ => {}
    }
}
```

The helper is deterministic when supplied with the fixed simulation delta. One
tick may cross several zero or short phases and returns every transition in
order, so low render rates do not skip an active or recovery boundary.
