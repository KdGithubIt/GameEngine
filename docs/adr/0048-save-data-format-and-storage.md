# ADR 0048 — Save Data Format, Storage Location, and Script Access

## Status: Accepted

Date: 2026-07-11

## Context

Nothing in the engine persists player progress: a packaged game (ADR 0045)
loses all state on exit. The M1 milestone requires save slots (mission
progress, unlocked content, settings) writable from game logic including
Rhai scripts, in both editor Play and packaged players.

Design questions: what the save format is, who owns it, where files live on
disk without adding a platform-directories dependency, and how sandboxed
scripts (ADR 0037: no direct file IO from scripts) read and write saves.

## Decision

### 1. Save data is an engine-owned, schema-versioned key-value document

`SaveData` is a flat ordered map of `String -> SaveValue` where `SaveValue`
is `Text(String) | Number(f64) | Flag(bool)`, serialized as `*.save.json`
with a `schema_version` field (v1). The engine owns the format
(`crates/engine/src/save.rs`): saves are produced and consumed by the
runtime and packaged games, never by the authoring pipeline, so `authoring`
is not involved. The key-value shape mirrors `UiBindings` (ADR 0046 §4) and
maps 1:1 onto Rhai types; games define their own key conventions.

Game-structured data (inventories, party rosters) is expressed by key
convention (`"party.0.id"`) or JSON-in-`Text` at the game's discretion; a
nested value tree is a compatible future extension (new `SaveValue` variant
+ schema bump).

### 2. `*.save.json` is a persisted stable format

Missing `schema_version` reads as v1; a newer version than the build
supports is a typed error (same rules as scenes / ADR 0020 and materials /
ADR 0029). Unknown keys are preserved by construction (the document IS a
map). Changing `SaveValue`'s JSON encoding requires a version bump and a
migration test.

### 3. Storage root is host-provided; files are slot-per-file

A `SaveStore` world resource owns a root directory and slot operations
(`write_slot(u32, &SaveData)`, `read_slot(u32)`, `list_slots()`,
`delete_slot(u32)`). Files are `<root>/slot_<n>.save.json`. Writes are
atomic (write temp file, then rename — the asset-manifest pattern from
Phase 16-B). Hosts choose the root:

- player binary: `<package root>/saves/` (portable-game layout; the package
  root is already the player's self-resolved data directory, ADR 0045)
- editor Play: `<project root>/saves/`

No `dirs`-style dependency is added; OS-standard save locations
(`%APPDATA%`, `~/Library`, XDG) are a future ADR if demanded. wasm32 gets
the same no-op stub treatment as `SceneLoader` (desktop-only IO).

### 4. The active save is a world resource; scripts access it via commands

The currently loaded save lives in the world as a `SaveData` resource that
any system can read/mutate directly. Rhai scripts never touch files:
following the ADR 0037 command pattern (`ComponentSetCommand`), the script
context exposes

- `save_get(key)` — reads from a snapshot of the `SaveData` resource taken
  before the call,
- `save_set(key, value)` — queues a mutation command,
- `save_write(slot)` / `save_load(slot)` — queue persistence commands,

and the dispatching system applies queued commands after the script runs:
mutations update the `SaveData` resource; write/load commands go through
`SaveStore`. IO failures are logged and surfaced as script-visible state on
the next frame (a `last_error` field on `SaveStore`), never panics.

Project Rust systems use the ABI v3 boundary from ADR 0052 rather than direct
resource or filesystem access. A system declares each `save_keys` entry it may
observe; missing keys are omitted and undeclared keys never cross the module
boundary. Typed deferred commands set/remove scalar values and request
numbered-slot write/load operations. Persistence requests enter a bounded
64-item host queue. A write captures the active document at command-application
time, then reuses `SaveStore`'s atomic replacement at the later service
boundary. A failed load leaves the active document unchanged. Missing host
resources and IO failures are diagnostics and never panic.

## Consequences

- Editor Play and packaged games share one save implementation and format;
  a save written in Play is byte-compatible with the packaged game's.
- Saves in `<project root>/saves/` may end up in VCS; projects should
  gitignore it (documented; the engine does not write ignore files).
- A single flat map per slot is O(save size) to write; fine for M1-scale
  saves (KBs). Partial/streamed saves would be a new ADR.
- Slot files are human-readable JSON — easy to debug and hand-edit, no
  tamper resistance (out of scope for M1; note for future).

## Alternatives Considered

- **Serializing world state (full ECS snapshot)** — rejected: fragile
  across schema changes, saves engine internals instead of game intent, and
  M1 games need progress data, not world dumps.
- **Authoring-owned format** — rejected: saves never pass through the
  authoring pipeline, editors don't edit them, and the authoring crate must
  not grow runtime-only concerns.
- **OS-standard directories via `dirs`/`directories` crate** — deferred:
  new dependency for M1-irrelevant polish; portable layout also simplifies
  packaged-game testing (delete folder = fresh state).
- **Direct file IO from Rhai** — rejected: violates the ADR 0037 sandbox
  (max-operations safety, no ambient authority); the command pattern already
  exists and keeps IO on the engine side.
- **Binary format (bincode/postcard)** — rejected for v1: needs a new
  dependency, loses debuggability, and save sizes don't justify it.

## Compatibility and Migration

Additive: new `save` module, resources, script-context additions, host wiring,
and ABI v3 scoped access. Introduces the persisted format `*.save.json` (v1);
no existing formats change. The ABI v3 `save_keys` and `save_values` fields use
deserialization defaults, so libraries rebuilt against an earlier v3 SDK keep
their empty-access behavior.
