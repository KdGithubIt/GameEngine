# ADR 0006: Graph Foundation Placement

Status: Accepted
Date: 2026-06-05

## Context

Phase 3 introduces a domain-neutral graph foundation for future Behavior Tree,
Shader Graph, Animation Graph, Blueprint-like visual scripting, and other graph
domains.

The graph foundation needs stable authoring identifiers, generic authoring
values, structured diagnostics, deterministic serialization, commands, diffs,
and transaction behavior. These facilities already belong to the `authoring`
crate.

Creating a separate `crates/graph` crate during Phase 3 would either make that
crate depend on `authoring` while `authoring` also needs graph transaction
integration, or require an additional shared-types crate before the ownership
boundary is justified.

Graph documents are editable project source data. They are not runtime ECS
entities, renderer resources, compiled graph artifacts, or runtime execution
state.

## Decision

The Phase 3 graph foundation is implemented as `authoring::graph` inside
`crates/authoring`.

The graph foundation may use the authoring crate's existing:

- `GraphId`, `NodeId`, `EdgeId`, `PortId`, and `GroupId` stable identifier
  types.
- `Value` type for persisted node properties and annotations.
- `Diagnostic` and `DiagnosticTarget` types for structured validation output.
- Command, diff, transaction, conflict detection, and session-level undo
  conventions.

The graph foundation owns only:

- Domain-neutral graph storage.
- Domain-neutral graph schema types.
- Structural validation.
- Graph authoring commands.
- Graph semantic change records and preview diffs.
- Single-document graph transaction behavior.
- Deterministic graph serialization.

The graph foundation does not own:

- Runtime ECS or renderer data.
- Runtime graph execution.
- Compiled graph artifacts.
- Behavior Tree, Shader Graph, Animation Graph, or visual scripting semantics.
- Domain-specific validation, type coercion, interpretation, or compilation.
- Editor, CLI, or MCP adapter behavior.

Phase 3-A implements only the semantic graph document in `authoring::graph`.
It does not implement `GraphView`, auto-layout, a concrete graph domain, or a
separate `crates/graph` crate.

Implementation note: Phase 3-A is complete with the semantic graph document,
schema model, structural validation, commands, change records, single-document
transactions, and deterministic semantic serialization in `authoring::graph`.
`Graph` does not expose public `Clone`; transaction working copies are created
only by a private helper so the in-memory document instance identity is not
duplicated through the public API.

Future graph domain crates may depend on `authoring::graph`. The shared graph
foundation must not depend on any concrete graph domain.

## Consequences

- Graph editing reuses existing authoring primitives without circular crate
  dependencies.
- Runtime ECS and renderer crates remain independent from authoring graph data.
- Domain-specific graph implementations can be added without changing the
  shared storage model.
- Extracting a separate graph crate later requires an explicit ownership
  review and may require extracting shared authoring primitives first.

## Alternatives Considered

### Create `crates/graph` in Phase 3

Rejected because the graph foundation needs authoring-owned primitives and
transaction behavior. A separate crate would create an unclear dependency
boundary before there is a demonstrated reuse case outside authoring.

### Store graph data in `engine`

Rejected because graph documents are persisted authoring source data, not
runtime engine state.

### Implement separate graph models in each domain

Rejected because duplicated storage, commands, serialization, and structural
validation would prevent a common graph foundation.

## Compatibility and Migration

No persisted graph format exists yet, so no data migration is required.

This ADR defines the initial public ownership boundary for Phase 3. Moving the
foundation to another crate later requires preserving the one-way dependency
from concrete graph domains to the shared foundation.
