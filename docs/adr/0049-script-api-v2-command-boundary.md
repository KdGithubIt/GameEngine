# ADR 0049: Script API v2 Command Boundary

## Status: Accepted

Date: 2026-07-12

## Context

Phase 60 expands the sandboxed Rhai context across runtime entities, prefabs,
animation, audio, UI, targeting, scenes, timers, and events. These operations
must not expose the ECS `World`, asset stores, or filesystem to scripts.

## Decision

Script calls append typed `ScriptApiCommand` values to a bounded FIFO. A
runtime adapter applies ordinary resource and component commands after script
dispatch. Structural prefab spawning is deferred to the existing frame
boundary where `App` has exclusive world access. Script-originated events are
delivered on the next scripting pass, preventing reentrant event loops.

Runtime scene entities receive a `RuntimeEntityIdentity` component containing
their session-local authoring ID and searchable name. Runtime entity strings
may be returned to a script for immediate commands but are never serialized.

Command and event queues are capped at 256 entries. Overflow and individual
command failures are logged and do not stop later commands. Rhai's ADR 0037
maximum-operation limit remains unchanged.

## Consequences

- Rhai retains a narrow command-oriented boundary.
- Prefab spawning reuses authoring validation and the scene bridge.
- Events emitted during a hook cannot recursively invoke another hook in the
  same scripting pass.
- Gameplay-spawned prefab entities follow ADR 0047 and survive scene switches
  until explicitly despawned.

## Alternatives

Direct world access and immediate structural mutation were rejected because
they break the sandbox and ECS query stability. A separate prefab runtime
converter was rejected because it would duplicate the component registry and
scene bridge.
