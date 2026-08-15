# Phase 61: Audio v2

## Goal

Extend the Phase 23 desktop audio runtime with independently controllable BGM
and sound-effect buses and smooth transitions between looping BGM tracks.

## Scope

- independent master, BGM, and SE volume controls
- effective per-bus volume calculated as `master * bus`
- infinitely looping BGM playback, preserving Phase 23 behavior
- linear BGM crossfades with configurable duration
- fade-in from silence when crossfade playback starts without an active BGM
- immediate replacement for zero, negative, or non-finite fade durations
- active SE and BGM sinks updated when their bus volume changes
- target-compatible WASM stubs for the expanded API

Distance-attenuated positional sound effects remain optional in the M1 plan
and are not implemented in this phase.

## Design Decisions

Audio playback remains owned by the Phase 23 worker thread. The worker retains
one sink for each active sound effect so bus changes affect sounds already in
progress. BGM playback retains either one looping sink or an outgoing/incoming
pair while a crossfade is active.

The worker uses a short timed receive interval while a crossfade is active or
idle. This keeps interpolation off the game thread without adding a new
dependency or requiring a frame system to drive audio fades. Crossfade gain
calculation and volume sanitization are pure functions covered by tests.

Starting another crossfade during an existing transition keeps the louder of
the two current tracks as the new outgoing track and stops the quieter one.
This bounds BGM playback to two simultaneous sinks.

## Public API

- `AudioSystem::set_bgm_volume` / `AudioSystem::bgm_volume`
- `AudioSystem::set_se_volume` / `AudioSystem::se_volume`
- `AudioSystem::crossfade_bgm`
- Rhai `crossfade_bgm`, `set_bgm_volume`, and `set_se_volume`

Existing `play_bgm`, `stop_bgm`, `play_se`, and master-volume APIs remain
compatible.

## Completion Criteria

- BGM and SE bus volumes are clamped independently and multiplied by master
  volume.
- Existing and newly started sinks use the correct effective bus volume.
- BGM assets continue looping until stopped or replaced.
- Positive-duration crossfades progress from the old track to the new track.
- Invalid or non-positive durations replace BGM immediately without panicking.
- Focused audio tests and all four workspace quality gates pass.
