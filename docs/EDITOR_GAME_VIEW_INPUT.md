# Editor Game View input forwarding

The embedded Game View forwards the same non-reserved keyboard names accepted by
Project Settings:

- `KeyA` through `KeyZ`
- `Digit0` through `Digit9`
- `ArrowUp`, `ArrowDown`, `ArrowLeft`, `ArrowRight`
- `Space`, `Enter`, and `Tab`
- `ShiftLeft`, `ShiftRight`, `ControlLeft`, and `ControlRight`

Mouse buttons and the desktop gamepad adapter continue to use their existing
runtime paths. Input is forwarded only while the Game View owns focus, and the
runtime receives releases when the window or Game View loses focus.

`Escape` remains reserved by the editor for **Stop Play**. Projects that need a
pause action in both Editor Play and the packaged Player should bind another
keyboard key or a gamepad button for Editor Play testing.
