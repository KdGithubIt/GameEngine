# ADR 0009: Graph Schema and Port Type Compatibility

Status: Accepted
Date: 2026-06-05

## Context

The common graph foundation must support graph domains with very different
semantics:

- Behavior Trees use execution-oriented connections.
- Shader Graphs use typed data flow and coercion rules.
- Animation Graphs use state and transition semantics.
- Blueprint-like visual scripting may combine control flow and data flow.

Hard-coding domain-specific value types, connection rules, root rules, cycle
rules, or compilation behavior into the shared foundation would prevent the
foundation from remaining domain-neutral.

At the same time, tools need schemas before creating nodes or connecting
ports, and the shared layer must be able to perform general structural
validation.

## Decision

The graph foundation owns domain-neutral schema storage and structural checks
only.

The shared schema model includes:

- `NodeSchema`.
- `PortSchema`.
- `PortDirection`.
- Port arity or multiplicity limits.
- Stable `NodeTypeId`.
- Stable `PortId`.
- Stable port value type identifiers.
- Node property schemas backed by authoring `Value`.

`NodeTypeId` and port value type identifiers are validated stable dotted
strings, not ULID-based authoring object IDs. `PortId` remains a typed stable
identifier as defined by ADR 0004.

Every edge has an explicit output endpoint and input endpoint. The foundation
may validate:

- Referenced nodes exist in the graph.
- Referenced ports exist in the applicable node schemas.
- The source port is an output.
- The destination port is an input.
- Duplicate connections are rejected when required by the shared model.
- Port arity limits are not exceeded.

The foundation stores and exposes port value type identifiers, but it does not
decide whether two value types are compatible. A concrete graph domain may use
exact identifier equality as its simplest rule, or may define coercion,
subtyping, inference, or other compatibility behavior.

All other compatibility and semantic rules belong to the concrete graph
domain, including:

- Type coercion and conversion.
- Generic type inference.
- Subtyping.
- Control-flow versus data-flow meaning.
- Root count and root selection.
- Cycle permission or prohibition.
- Reachability.
- Domain-specific required connections.
- Compilation or interpretation.

The graph foundation owns only storage, schema, structural validation,
commands, diffs, transactions, and serialization. Domain-specific semantics
are outside foundation scope.

Phase 3-A may implement the schema model and structural validation, but it does
not implement Behavior Tree, Shader Graph, Animation Graph, Blueprint-like
visual scripting, or any other concrete graph domain.

Implementation note: Phase 3-A stores and exposes `PortValueTypeId` but does
not emit port value type compatibility diagnostics. Foundation structural
validation is limited to graph and schema reference integrity, endpoint
direction, duplicate edge, and arity checks.

## Consequences

- The shared graph model can represent multiple graph domains without adding
  domain-specific variants.
- Tools can query node and port schemas before authoring graph changes.
- Structural validation remains useful even when no concrete domain is loaded.
- Concrete domains must provide their own semantic validation and advanced
  compatibility rules.
- Port value type identifier naming becomes part of each domain's stable
  authoring contract.

## Alternatives Considered

### Use a closed enum of all port value types

Rejected because future domains would require modifying the shared foundation
for every new type.

### Put all connection compatibility in the foundation

Rejected because coercion, inference, control flow, and domain semantics differ
substantially between graph domains.

### Allow domains to replace the shared graph storage model

Rejected because duplicated storage, commands, diffs, and serialization would
defeat the purpose of a common graph foundation.

## Compatibility and Migration

No persisted graph schemas or graph documents exist yet, so no migration is
required.

Concrete domains must treat their node type IDs, port IDs, and port value type
IDs as stable persisted contracts once project files use them.
