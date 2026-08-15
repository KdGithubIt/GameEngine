# ADR 0012: Phase 4 First Domain Behavior Tree

Status: Accepted
Date: 2026-06-05

## Context

Phase 4 requires the first concrete graph domain to be selected in an ADR
before implementation begins. The domain must prove that the Phase 3 graph
foundation can support real domain semantics without moving domain-specific
rules into the shared graph layer.

The candidate domains are Behavior Tree, Shader Graph, Animation Graph, and
Blueprint-like visual scripting. The first domain should exercise typed ports,
domain validation, deterministic compilation or interpretation, and layout
policy behavior while keeping implementation risk low.

## Decision

Phase 4 will implement Behavior Tree as the first concrete graph domain.

The Phase 4 Behavior Tree domain will support:

- Root node.
- Sequence node.
- Selector node.
- Condition node.
- Action node.
- Decorator node.
- Explicit child ordering.
- Typed behavior-tree ports and domain-owned connection rules.
- Domain-specific diagnostics with stable `behavior_tree.*` codes.
- A deterministic compiled or interpreted runtime representation.
- A top-down default layout policy.

The first implementation will not include arbitrary scripting inside nodes.
Action and condition behavior will be represented by stable identifiers or
properties owned by the Behavior Tree domain, not by runtime script execution.

The domain must be implemented without requiring shared graph foundation
changes. If Phase 4 reveals a missing foundation capability, that capability
must be captured in a separate ADR before changing shared graph behavior.

## Consequences

- Phase 4 has a concrete implementation target.
- The common graph foundation will be tested against hierarchical domain
  semantics, typed ports, child ordering, reachability, and deterministic
  runtime representation.
- Shader Graph, Animation Graph, and visual scripting remain future domains.
- Behavior Tree diagnostics own the `behavior_tree.*` diagnostic namespace.
- The default layout policy can be simple and deterministic: parent nodes
  above child nodes, with execution order left to right.

## Alternatives Considered

### Shader Graph

Rejected for Phase 4 because it requires richer type compatibility, coercion,
and likely expression or material compilation policy. Those are valuable, but
they add complexity before the first concrete domain proves the foundation.

### Animation Graph

Rejected for Phase 4 because blending, state transitions, time semantics, and
runtime animation integration introduce domain behavior that is broader than
the first graph-domain proof needs.

### Blueprint-like Visual Scripting

Rejected for Phase 4 because general control flow and data flow would require
a much larger validation and execution model than a first domain should own.

## Compatibility and Migration

No persisted Behavior Tree graph files exist yet, so no migration is required.

This ADR does not change the existing graph foundation serialization format.
It reserves the Behavior Tree domain contract for Phase 4 and leaves shared
graph storage, transactions, and validation boundaries unchanged.
