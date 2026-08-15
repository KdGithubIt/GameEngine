# ADR 0064: Deterministic Input Replay Format

Status: Accepted

Date: 2026-07-19

## Context

The engine already funnels human, AI, and replay input through
`VirtualInputQueue`, but it has no persisted deterministic recording shared by
Game View and headless tests.

## Decision

`*.replay.json` format version 1 stores the engine version, fixed simulation
step, ordered tick records, virtual input commands, optional named checkpoints,
and an optional expected final authoring-visible state. Input commands use the
existing engine command vocabulary; operating-system injection is never
recorded or replayed.

Recording assigns commands to the next fixed tick. Playback rejects unknown
format versions, non-positive or non-finite steps, unordered ticks, and engine
major-version incompatibility. Playback uses the recorded fixed step and feeds
commands through `InputSource::Replay` before the same input-drain boundary
used by normal Play.

## Consequences

Editor and headless tests consume one deterministic artifact. The format does
not promise deterministic rendering or external I/O; checkpoints and expected
state make gameplay outcomes explicit and reviewable.

