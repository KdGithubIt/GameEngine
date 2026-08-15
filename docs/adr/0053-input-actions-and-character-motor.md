# ADR 0053: Input Actions and Kinematic Character Motor

Status: Accepted
Date: 2026-07-13

## Context

Project Settings currently compiles keyboard keys, gamepad buttons, and raw
gamepad axes, but mouse buttons, axis scaling/inversion, digital positive and
negative composition, and analog transitions are absent. Gamepad disconnect
does not release held state. The built-in player controller also converts four
actions into booleans and writes `Transform` once per rendered frame, bypassing
the fixed-step kinematic controller and discarding analog magnitude.

Editor Ready v1 requires Project Settings to be the single binding source for
Editor Play, virtual-input tests, and the packaged player.

## Decision

1. Existing `project_settings.json` version 1 documents remain readable. New
   input binding fields use Serde defaults; missing fields preserve the old
   keyboard/gamepad behavior. Saving writes the complete current shape without
   renaming existing action IDs.
2. An action may declare keyboard keys, named mouse buttons, gamepad buttons,
   scaled/inverted gamepad axes, and explicit digital axis pairs. A digital
   pair subtracts its negative sources from its positive sources. Multiple
   analog contributions combine deterministically and clamp to `[-1, 1]` per
   vector component.
3. Deadzones are applied in the action compiler, not in the OS adapter, so the
   same raw virtual-input recording resolves identically in Editor Play and the
   player. Non-finite scale/deadzone values and unknown physical names produce
   non-blocking, action-linked diagnostics and are ignored.
4. Physical state advances once at the host frame boundary. Resolved actions
   expose `pressed`, `just_pressed`, and `just_released` for digital or analog
   activation. Project systems only read that frame snapshot; one callback
   cannot consume another callback's transition.
5. Window/Game View focus loss releases every forwarded keyboard and mouse
   button. A gamepad disconnect releases that device's button and axis state;
   if the current merged storage cannot distinguish devices, releasing all
   gamepad state is the conservative compatibility behavior until per-device
   player assignment exists. Connect/disconnect state and ignored bindings are
   visible to runtime diagnostics.
6. The action-RPG controller consumes a two-dimensional move action when
   configured, with compatibility fallback to the four legacy movement action
   names. It preserves analog magnitude, applies camera-relative XZ movement,
   acceleration/deceleration, facing policy, and optional sprint/dodge
   requests, and writes desired velocity to `KinematicCharacterController` on
   the fixed-step path. It does not independently integrate `Transform`.
7. Editor and player build the same `InputActionMap`, run the same resolver,
   and accept the same `VirtualInputQueue` commands. Tests compare resolved
   frame recordings, not just final positions.

## Consequences

- Rebinding movement changes gameplay without recompiling project Rust.
- Existing four-action projects remain valid while new projects can use one
  vector action.
- Character movement and collision have one integration owner, eliminating
  rendered-frame/fixed-step disagreement.
- A future local-multiplayer design must replace conservative merged gamepad
  disconnect handling with explicit player/device assignment; local
  multiplayer is outside Editor Ready v1.

## Safety and Failure Semantics

- Input values crossing GameModule ABI v3 remain copied scalars and fixed-size
  vectors; no platform handles cross the boundary.
- Invalid binding entries are skipped individually. Valid entries in the same
  action continue to resolve.
- Non-finite or out-of-range physical axis input is clamped or discarded before
  gameplay observes it.
- Focus loss and disconnect favor releasing state over preserving an uncertain
  held input, preventing stuck movement or actions.
