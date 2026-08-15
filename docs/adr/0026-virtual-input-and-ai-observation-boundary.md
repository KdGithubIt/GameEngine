# ADR 0026: Virtual Input and AI Observation Boundary

Status: Accepted
Date: 2026-06-11

## Context

The runtime engine reads input through three ECS resources: `Input<KeyCode>`,
`Input<MouseButton>`, and `MouseInput` (`crates/engine/src/input.rs`). Today
these resources are updated in exactly one place: `EngineRunner` translates
winit window events directly into resource mutations
(`crates/engine/src/app.rs`). The editor Play mode (`crates/editor/src/runtime.rs`)
ticks a runtime world that contains these resources but currently never feeds
them.

The project wants AI agents to play the game: an agent receives a screenshot
of the current Game View / Player frame, decides an action, and injects
keyboard, mouse, and eventually gamepad input back into the running world.
The same injection path should serve replay playback and automated tests.

Decisions are needed on:

1. Where input injection lives (OS level vs engine level).
2. How injected input and human winit input coexist.
3. Where frame observation (screenshot capture) lives, and where AI API
   communication lives.
4. How gamepad support can be added later without breaking the input contract.

## Decision

### 1. Input injection is an engine-level virtual input layer

The engine defines a unified input command model in `crates/engine/src/input.rs`
(or a sibling module):

```rust
/// Identifies who produced an input command.
#[non_exhaustive]
pub enum InputSource {
    Human,
    AiAgent,
    Replay,
    Test,
}

/// A device-independent input event that can be injected into the runtime.
#[non_exhaustive]
pub enum InputCommand {
    Key { key: KeyCode, pressed: bool },
    MouseButton { button: MouseButton, pressed: bool },
    MouseMove { position: (f32, f32) },
    MouseDelta { delta: (f64, f64) },
    MouseScroll { amount: f32 },
    // Reserved for gamepad support; no device backend exists yet.
    GamepadButton { gamepad: GamepadId, button: GamepadButton, pressed: bool },
    GamepadAxis { gamepad: GamepadId, axis: GamepadAxis, value: f32 },
}
```

A `VirtualInputQueue` ECS resource accepts `(InputSource, InputCommand)`
pairs. A drain step applies queued commands to the existing resources
(`Input<KeyCode>`, `Input<MouseButton>`, `MouseInput`) once per tick, at the
same point in the frame where winit events are applied today: after the
previous frame's `clear_transitions()` and before the schedule runs. This
preserves `just_pressed` / `just_released` semantics for injected input.

The drain is an explicit engine function (callable by `EngineRunner` and by
the editor's `RuntimePlayState::tick`), not an auto-registered ECS system,
because system ordering relative to user systems and to
`clear_transitions()` must be guaranteed.

Mouse coordinates in `InputCommand::MouseMove` are physical pixels in the
render-target space — the same space as `MouseInput.position` and the same
pixel grid as captured frames, so an agent can click coordinates taken
directly from a screenshot.

### 2. The engine MUST NOT synthesize OS-level input

No `enigo`-style OS mouse/keyboard automation, and no window message
injection. AI input exists only inside the runtime world. This keeps agent
play sandboxed, deterministic, and safe to run while the developer uses the
machine.

### 3. Human winit input converges on the same path (future step)

`EngineRunner` will eventually translate winit events into
`InputCommand` values tagged `InputSource::Human` and push them through the
same queue, making the queue the single write path for input resources. This
unification is planned but not required for the first virtual-input
implementation; the direct winit mutation path may remain until then.

### 4. Observation is frame capture only; AI transport lives outside engine

Observation is synchronous read-back of an offscreen render target:

```rust
pub struct FrameCapture {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, row-major, top-left origin.
    pub rgba8: Vec<u8>,
}
```

Read-back lives with the owner of the render target. Today the only capture
target is the editor-owned Game View texture (ADR 0024), so `FrameCapture`
and the read-back implementation live in `crates/editor`
(`RuntimePlayState::capture_game_view`); the Game View color texture adds
`COPY_SRC` usage. A shared engine-side read-back helper is extracted only
when a second consumer appears (e.g. a standalone Player capture in the AI
Agent Bridge phase) — a single-use abstraction in the engine is not created
ahead of need.

Capture returns raw pixels only. PNG encoding, prompt construction, and AI
API communication are owned by an outer layer (CLI / MCP / a future agent
crate), mirroring how `crates/cli` and `crates/mcp` are thin adapters over
`crates/authoring`.

### 5. Gamepad is reserved as data, not implemented as a device

`GamepadId`, `GamepadButton`, and `GamepadAxis` are defined as plain data
types when `InputCommand` is introduced, so replay files, tests, and AI
agents can use gamepad commands before any physical device backend exists.
`Input<GamepadButton>` resources and a device backend (e.g. `gilrs`) are
added only when a phase needs real hardware. `InputCommand` and
`InputSource` are `#[non_exhaustive]` so later variants (touch, additional
sources) are not breaking changes.

## Consequences

- AI agents, replay playback, and tests share one injection contract instead
  of three ad-hoc mechanisms.
- Game systems remain unchanged: they keep reading `Input<KeyCode>`,
  `Input<MouseButton>`, and `MouseInput` and cannot tell input sources apart.
  Source-aware filtering (e.g. ignoring human input during replay) becomes a
  queue-level policy if ever needed.
- The editor Game View texture gains `COPY_SRC` usage; capture cost is paid
  only when requested.
- The engine gains no network, AI SDK, or image-codec dependencies.
- `Input::press` / `Input::release` (currently `pub(crate)`) get an
  engine-internal application path for queued commands; their visibility does
  not widen.
- The replay file format is explicitly out of scope and requires its own ADR
  before `InputCommand` gains serialization.

## Alternatives Considered

### OS-level input automation (enigo, SendInput)

Rejected. Couples agent play to window focus and OS timing, can hijack the
developer's real mouse and keyboard, and is untestable in CI.

### Apply injected commands immediately on push

Rejected. Mid-frame application makes `just_pressed` visibility depend on
system execution order. Draining once at the frame boundary matches how winit
events are batched today.

### Auto-registered ECS system for the drain

Rejected for now. The ECS has no ordering constraints between systems, so a
drain system could run after user systems and delay input by one frame.
An explicit pre-update call site is deterministic.

### Embed AI API communication in the engine

Rejected. Violates the thin-adapter layering used by CLI/MCP, adds heavy
dependencies to every game, and ties the engine to one AI provider.

### Defer gamepad types entirely until a gamepad phase

Rejected. Adding enum variants later is cheap (`#[non_exhaustive]`), but the
*shape* of gamepad commands (id + button/axis + analog value) affects the
`InputCommand` design now — axis values are the first analog input, which is
why `InputCommand` is not a pure button-state model.

### Capture frames via OS screen capture

Rejected. Captures editor chrome, depends on window placement and DPI, and
breaks the 1:1 mapping between screenshot pixels and `MouseInput.position`
coordinates.

## Compatibility and Migration

No persisted file format, stable ID, or authoring command changes. New types
(`InputCommand`, `InputSource`, `VirtualInputQueue`, gamepad ID/button/axis
types) are additive public engine API; `FrameCapture` is additive public
editor API until a second capture consumer motivates moving it into the
engine. Existing `Input<KeyCode>` / `Input<MouseButton>` / `MouseInput`
consumers are unaffected.

Serializing `InputCommand` for replay files is deferred; that format freeze
requires a separate ADR. The winit-to-`InputCommand` unification (Decision 3)
is an internal `EngineRunner` refactor with no public API impact.
