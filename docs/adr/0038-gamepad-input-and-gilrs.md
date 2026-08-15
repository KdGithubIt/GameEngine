# ADR 0038: Gamepad Input and gilrs Desktop Backend

Status: Accepted
Date: 2026-06-14
Target Phase: Phase 43

## Context

The input layer already has keyboard, mouse, and virtual input commands. It also
has reserved gamepad command variants, but those variants were intentionally
accepted as no-ops until Phase 43.

The engine needs controller input for runtime play, scripting, and editor test
harnesses without leaking platform-specific device APIs into gameplay systems.

## Decision

Desktop gamepad input uses `gilrs`, gated to non-wasm targets. Runtime systems
consume engine-owned input types:

- `GamepadId`
- `GamepadButton`
- `GamepadAxis`
- `Input<GamepadButton>` for the merged button state
- `GamepadAxisState` for per-device analog axes
- `InputCommand::GamepadButton` and `InputCommand::GamepadAxis` for virtual
  injection and backend events

The `gilrs` backend translates platform events into `VirtualInputQueue`
commands. Gameplay systems do not depend directly on `gilrs`.

WASM gamepad support is not part of this decision and must be decided in the
web-build phase.

## Consequences

- `gilrs` is added only for desktop targets.
- Tests and AI/replay tools can inject gamepad commands through the existing
  virtual input queue.
- Gameplay and scripting APIs can read the engine-owned gamepad resources
  without coupling to platform crates.
- Multi-controller policy remains intentionally simple for the MVP: button
  input is merged, while axis values keep their `GamepadId`.

## Alternatives Considered

### Direct `gilrs` access from gameplay systems

This would be simple initially but would make gameplay systems platform-specific
and harder to replay or test.

### Winit-only controller support

`winit` does not provide the full cross-platform controller event model needed
for desktop gameplay.

### Custom OS backends

Custom backends would increase maintenance cost and delay useful runtime input.

## Compatibility and Migration

Existing keyboard and mouse input behavior is unchanged. Previously no-op
gamepad virtual commands now update gamepad input resources when those resources
are present.
