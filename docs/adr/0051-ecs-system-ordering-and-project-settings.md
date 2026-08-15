# ADR 0051: ECS System Identity, Ordering, and Project Settings

Status: Accepted
Date: 2026-07-13

## Context

The runtime ECS previously stored anonymous `System` trait objects directly in
registration order. Editor Play and the generic player repeated host-specific
registrations, while native GameModule systems ran as a second batch after the
corresponding Engine schedule. Project data could not inspect, disable, or
reorder the actual systems that executed.

Persisting Rust `type_name` values would make project settings depend on module
paths and refactors. Ordering comments also could not prevent a user or future
registration change from violating required producer-consumer relationships.

## Decision

1. `engine-ecs` owns `SystemId`, `SystemDescriptor`, `ScheduleEntry`, enabled
   state, and before/after constraint resolution. It remains independent from
   authoring and GUI crates.
2. Persistable systems use explicit dotted ASCII IDs such as
   `engine.transform_propagation` and `game.health_decay`. Compatibility
   registrations receive a deterministic legacy ID but are marked unsuitable
   for persistence.
3. Existing unnamed registration APIs remain available. Named variants add
   metadata and detect duplicate canonical IDs and aliases.
4. Final order is a stable Kahn topological sort. Saved preference order is the
   tie breaker, so systems unrelated by constraints retain user order. Missing
   targets are diagnostics; cycles are blocking errors.
5. Disabled systems remain registered and retain their position. Schedule
   execution skips them without mutating entries during a run.
6. `project_settings.json` gains an optional nested `system_settings` document
   with its own schema version and separate Update/FixedUpdate order and
   disabled-ID arrays.
7. Native GameModule metadata crosses ABI v2 as JSON. Each Game callback is
   registered as one fallible ECS system with validated
   `Query<&mut GameComponentStore>` access, allowing Engine/Game interleaving
   without exposing raw `World` across the library boundary.
8. Editor changes are atomic project-setting writes and take effect when the
   next Play runtime is created. The active `RuntimePlayState` is never edited.
9. Rust `UiSystem` entries remain outside this feature because they execute in
   an egui-specific presentation phase rather than Update or FixedUpdate.

## Consequences

- Runtime execution and the Systems panel share the same descriptors.
- New systems absent from old settings are merged in registration order and
  then constrained.
- Removed IDs and missing constraint targets remain visible as diagnostics.
- GameModule ABI v1 libraries must be rebuilt; the loader reports an explicit
  ABI mismatch and the editor remains usable with Engine systems only.
- Game callback failures participate in normal Schedule failure and deferred
  command discard behavior.
- Profiling can later extend `ScheduleEntry` without changing persisted order.

## Alternatives Considered

Persisting Rust type names was rejected because module moves and renames would
silently invalidate settings. Keeping Game systems as a post-schedule batch was
rejected because Engine and Game entries could not share one truthful order.
Giving callbacks raw `World` access was rejected because it would bypass ADR
0001 and ADR 0050 safety boundaries. UI systems remain separate because their
egui presentation phase has different inputs and timing.

## Compatibility and Migration

Old `project_settings.json` files deserialize with empty system settings.
Aliases normalize previous IDs on the next successful edit. Unknown IDs are
ignored at runtime but reported during merging.

Existing unnamed registration APIs remain valid. Project-native
`#[engine::game_system]` declarations may omit `id` for source compatibility,
but new editor scaffolds emit an explicit stable ID.
