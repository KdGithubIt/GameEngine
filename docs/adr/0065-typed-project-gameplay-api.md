# ADR 0065: Typed Project Gameplay API

Status: Accepted

Date: 2026-07-19

## Context

ADR 0052 correctly keeps host ECS layouts and Rust references out of the
dynamic-library ABI, but the first ABI v3 SDK exposed its serialized transfer
objects directly to ordinary game systems. Project code consequently repeated
stable-ID lookup, generic-value matching, object-field lookup, array indexing,
default fallback, event acknowledgement, and output patch construction.
Access declarations were separate functions and could drift away from the
callback logic.

## Decision

1. The ABI v3 envelopes, stable IDs, bounded deterministic JSON, and host-side
   validation remain the module boundary. They are hidden implementation APIs,
   not the recommended game-programming surface.
2. Project systems use engine-owned typed system parameters. The
   `game_system` macro derives `GameSystemAccess` from the exact function
   parameter types and rejects a separate access function on typed systems.
3. Typed parameters cover time, named input actions, named save keys, project
   resources, project queries, copied engine views, host views, event streams,
   project events, and deferred commands.
4. Query specifications declare components and engine views by Rust type.
   Query rows decode project components and copied views through schema-owned
   adapters. Writable component patches are available only when the query
   specification declares write access.
5. Missing required values, undeclared access, type mismatches, and malformed
   host payloads return `GameApiError`. They never become implicit defaults.
   ABI callback atomicity discards the complete output on such an error.
6. Writable project resources encode their complete typed value when the
   wrapper is dropped. Typed event readers acknowledge successfully decoded
   stream records. Typed command methods construct the internal command
   payload and do not expose field names or generic values.
7. Engine-owned typed system parameters are sealed. Project code may define
   query, input, save-key, and project-event marker types, but cannot implement
   an unchecked parameter that lies about host access.
8. Generated projects remain standalone Cargo workspaces as required by ADR
   0050. Project initialization writes a VS Code linked-project setting, and
   the engine repository links its nested example game manifests in supported
   IDE configuration so language services resolve the selected SDK.

## Consequences

- Game logic primarily contains domain decisions over concrete Rust values.
- Access metadata and callback acquisition cannot silently diverge.
- Adding an engine view or event requires one strict host encoder and one typed
  SDK decoder, making schema drift visible in tests.
- Raw ABI modules remain public for macro expansion and host/editor integration
  but are hidden from generated API documentation and new scaffolds.
- The `game_system` macro no longer accepts raw ABI callbacks or a separate
  `access` function. Host-side ABI helpers remain implementation details only.

## Alternatives Considered

Passing `World`, ECS references, or plugin-defined component layouts across the
dynamic-library boundary remains rejected by ADRs 0050 and 0052. Moving only
the repeated lookups into project helper functions was rejected because it
would preserve split access declarations and duplicate the same unsafe
assumptions in every game. Generating one monolithic game-specific host ABI was
rejected because it would complicate hot rebuilds, packaging, and editor/player
parity.
