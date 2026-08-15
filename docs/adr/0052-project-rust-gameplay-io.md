# ADR 0052: Project Rust Gameplay Queries and Deferred Commands

Status: Accepted
Date: 2026-07-13

## Context

ADR 0050 established a safe native-library boundary for project-local Rust,
and ADR 0051 gave project systems stable identities and schedule ordering. The
current callback still receives a JSON snapshot containing every project
component store, then returns the complete snapshot. It cannot read engine
state such as transforms, input, time, collisions, animation, navigation, or
UI events. It also cannot request structural ECS changes or engine services.

Passing the host `World`, Rust references, or trait objects to a dynamically
loaded library would violate ADR 0050. Re-serializing an entire world for each
system is also unsuitable for an action game with multiple systems and hundreds
of entities.

## Decision

1. The next gameplay-capable module contract is ABI v3. ABI v2 modules receive
   an explicit mismatch diagnostic and must be rebuilt; the host never guesses
   the layout of an older callback.
2. Every project system exports a declarative access manifest as descriptor
   metadata. It lists queried project components, approved engine read views,
   writable game components, resources, event streams, and command families.
   Undeclared data is omitted and undeclared commands are rejected by the host.
3. The host builds one query-scoped input envelope per callback. The envelope
   contains only matching entities and requested frame/resource/event data.
   Read-only values are never echoed back. Multiple project component types
   may be requested by one entity query.
4. ABI records remain C-compatible fixed-width scalars, pointers, lengths, and
   function pointers. Variable data crosses as bounded host- or module-owned
   byte buffers. Editor Ready v1 uses deterministic JSON inside those buffers
   for inspectability; changing the payload encoding requires a later ABI.
5. Runtime entities cross the boundary as `(id: u32, generation: u32)`. Every
   command validates both fields at application time. A stale handle produces
   a diagnostic and cannot affect a newly reused entity ID.
6. Engine data is exposed through stable copied views rather than engine Rust
   components. ABI v3 initially defines views for frame/fixed time, resolved
   input actions, authoring identity, local/global transform, character state,
   collision transitions, animation state/events, lock-on state, navigation
   status, UI events/bindings, scene-flow state, and explicit save values.
   Entity views live inside query rows. Global scene-flow state is a separately
   declared host view so a late subscriber can recover current, pending, and
   failure state without replaying an already-consumed transition event.
7. A callback output contains only writable project-component patches,
   game-resource patches, deferred commands, emitted game events, and consumed
   event cursors. Project code never returns a replacement world snapshot.
8. Deferred commands are applied by the host after a callback succeeds and at
   the schedule boundary defined by the command family. The initial families
   cover transform/controller changes, prefab spawn and despawn, safe component
   changes, animation and graph control, attack hitboxes, audio, lock-on, UI,
   scene transitions, saves, timers, and game events. Spawn results are emitted
   later with the caller-provided request ID.
9. Game-owned global resources are host-owned values keyed by a stable dotted
   ID and validated against module-exported schemas. They are runtime-only,
   survive only according to declared scene policy, and enter save data only
   through an explicit save command. ABI v3 carries the module registry in the
   first deterministic system metadata document as a Serde-defaulted field;
   the root C descriptor layout therefore remains unchanged. Installing a new
   module generation replaces the host map with schema defaults.
10. Per system invocation, the host permits at most 16,384 entity rows, 4,096
    event records, 1 MiB of input payload, 1 MiB of output payload, and 1,024
    commands. The shared event history is capped at the same 4,096 records and
    uses per-system consumption cursors. Exceeding an invocation cap rejects it
    with a structured diagnostic; event-log overflow drops the oldest record
    with a host diagnostic instead of growing without bound.
    Project save persistence additionally uses a 64-request host queue and
    save keys are limited to 256 UTF-8 bytes with no control characters.
    Gameplay timers are capped at 1,024 entries and their source event log is
    capped at 1,024 records before delivery into the shared host event log.
    Project prefab spawning uses a 256-request exclusive-world queue and a
    256-record result source log.
11. The host records callback duration, matched row count, input/output byte
    counts, command count, and the latest error for the profiler and Systems
    panel.
12. Module generation changes are adopted only while Play is stopped. Editor
    Play and packaged Player use the same access compiler, callback dispatcher,
    validators, command applier, and limits.

## Ordering and Failure Semantics

- Input/event snapshots are captured immediately before the callback from the
  state visible at that schedule position.
- A callback either yields a fully decoded and validated output or has no
  effect. Panics, malformed output, cap violations, stale required handles, and
  unauthorized commands discard the complete output.
- Valid outputs are applied in command order. A later command may observe a
  spawn only through a subsequent spawn-result event, never by assuming an ECS
  ID during the same callback.
- Save reads include only existing keys named by the system's `save_keys`
  declaration. Set/remove operations update the active document in command
  order. A slot write captures that exact point-in-order document; load is
  performed later and replaces the active document only after a successful,
  schema-validated read.
- Timer IDs are project-wide stable dotted IDs. Timers advance on fixed steps;
  set replaces an ID, cancel removes it, and query emits a later `Timer` event
  with the caller's request ID. Completion is emitted exactly once and remains
  visible under the normal per-system event cursor contract.
- Project game events use the output envelope's dedicated `emitted_events`
  collection rather than a generic command payload. Broadcast events carry no
  target; targeted events carry a generation-checked entity handle. Because a
  project system is not entity-owned, all `Game` stream subscribers receive
  the record and match its optional target against their declared query rows.
- Prefab spawn requests accept only safe project-relative `*.prefab.json`
  paths and run through the same authoring-to-runtime conversion bridge as
  script and scene content. The later `SpawnResult` record includes the caller
  request ID plus either a generation-checked runtime root or an error message.
- Safe structural component commands are limited to project components stored
  in `GameComponentStore`. Add uses the active module schema's copied default;
  disable retains the value but excludes it from queries; enable restores it;
  remove deletes it. These changes are runtime-only and never write scene or
  prefab authoring data. Arbitrary engine-component construction is not
  exposed through this family.
- Hitbox commands configure box, sphere, or Y-capsule trigger colliders only
  on carrier entities whose collision components are unowned. The command owns
  and later removes that complete collider set, so it never overwrites scene
  physics. Disable excludes the hitbox from detection; re-enable starts a new
  monotonic activation and clears one-hit history. Owner, team, damage,
  one-hit policy, enabled state, and activation are available through the
  copied `HitboxState` view. ER-7 builds hit-result and invulnerability
  processing on this runtime primitive.
- Fixed-update systems receive the fixed-step delta and current fixed-step
  index. Update systems receive the rendered-frame delta. Both clocks are
  copied values and cannot be mutated by project code.

## Safety Invariants

- No host allocation is freed by the module and no module allocation is freed
  by the host except through the module's exported free callback.
- No callback unwinds across the C boundary.
- The library handle outlives every descriptor, callback, and returned buffer.
- The access manifest is validated before a system is registered.
- Project code cannot obtain raw ECS storage, raw resource pointers, or an
  unvalidated structural command path.

## Consequences

- Project Rust can implement complete gameplay without linking its Rust layout
  into host ECS archetypes.
- Systems with narrow access declarations transfer substantially less data than
  the v2 whole-project snapshot path.
- Adding a new engine view or command family is an explicit SDK/ABI design task.
- JSON has measurable overhead, but scoped payloads and recorded byte counts
  make that cost visible. A future ABI may adopt a binary encoding after real
  vertical-slice profiling.
- Existing v1/v2 native modules require a rebuild and receive actionable ABI
  mismatch diagnostics.

## Alternatives Considered

- Pass `&mut World` through the library boundary: rejected because Rust layout,
  lifetimes, allocator ownership, and engine invariants would become unsound.
- Expose one function per engine operation directly over C ABI: rejected because
  re-entrant mutation would bypass schedule ordering and atomic failure.
- Keep whole-world snapshot replacement: rejected because read-only data is
  repeatedly encoded and returned, structural operations remain impossible,
  and access intent cannot be inspected.
- Use Rhai as the integration layer: rejected for this roadmap because the
  approved scope requires project-local Rust and excludes new Rhai work.

## Compatibility and Migration

Data-only projects continue to work. Scene, prefab, component IDs, and project
settings formats do not change. Native game projects rebuild against ABI v3;
the editor keeps the project open and reports a blocking Play diagnostic when
an older library is found. New project scaffolds declare explicit access lists,
while compatibility scaffolds may start with an empty access manifest that
cannot query data or issue commands.
New optional access/input fields within v3 use Serde defaults. This preserves
empty behavior when a descriptor or invocation produced by an earlier v3 SDK
does not contain those fields; it does not permit old ABI versions to load.
The optional module-resource registry follows the same rule. A module with no
project systems cannot use a runtime resource and therefore exports no carrier
metadata.
