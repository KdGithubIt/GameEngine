# ADR 0011: Graph Domain Validation Boundary

Status: Accepted
Date: 2026-06-05

## Context

Phase 3-A completed the semantic graph foundation in `authoring::graph`.
Phase 3-B completed the optional presentation graph view foundation in
`authoring::graph_view`.

ADR 0006 through ADR 0010 establish that:

- `Graph` is an authoring semantic document.
- `GraphView` is an optional presentation document.
- `Graph` and `GraphView` have separate single-document transaction
  boundaries.
- The graph foundation stores and exposes `PortValueTypeId`.
- The graph foundation does not decide port value type compatibility.
- Runtime graph execution, Runtime ECS integration, renderer integration,
  bridge changes, and production Behavior Tree, Shader Graph, Animation Graph,
  or Blueprint-like visual scripting domains remain out of scope.

Phase 3-C needs to prove that concrete graph domain semantics can be attached
to the shared graph foundation without moving domain rules into the
foundation.

## Decision

Domain validation is a separate layer from foundation structural validation.

Foundation structural validation owns only domain-neutral checks:

- Node existence.
- Port existence.
- Edge structure.
- Endpoint direction rules.
- Port arity rules.
- Graph and schema key-to-embedded-ID consistency.

Domain validation owns domain-specific semantic checks:

- Port value type compatibility.
- Root rules.
- Cycle rules.
- Reachability rules.
- Node property semantics.
- Domain-specific connection semantics.
- Future compilation or interpretation readiness.

The foundation continues to store and expose `PortValueTypeId`, but it does
not decide whether two port value types are compatible and does not emit port
value type compatibility diagnostics.

`GraphTransaction` remains domain-agnostic. `GraphTransaction::commit` does
not automatically run domain validation. Domain validation is invoked
explicitly by a helper or caller that has selected a graph domain.

Phase 3-C introduces `authoring::graph_domain` as the candidate module for
domain validation integration. The module may define:

```rust
pub trait GraphDomain {
    fn graph_kind(&self) -> &GraphKind;
    fn schema_registry(&self) -> &dyn GraphSchemaRegistry;
    fn validate_domain(&self, graph: &Graph) -> Vec<Diagnostic>;
}
```

`GraphDomain` provides the schema registry and domain validation entry point.
Diagnostics use the existing `Diagnostic` type. The foundation does not add a
separate `GraphDomainDiagnostic` type.

In Phase 3-C, a `GraphDomain` targets one `GraphKind`. The
`GraphDomain::graph_kind()` method returns the single graph kind supported by
that domain stub. Production domains or aggregate domains that support
multiple graph kinds are Future Work.

Phase 3-C uses a test-only or fixture graph domain such as `TestGraphDomain`
to prove the boundary. `TestGraphDomain` is a single-`GraphKind` fixture
domain. It is not a production Behavior Tree, Shader Graph, Animation Graph,
or visual scripting implementation.

GraphView is not required for domain validation. Domain validation may inspect
`Graph`, but it must not mutate `Graph` or `GraphView`.

## Consequences

- The graph foundation remains domain-neutral.
- Concrete graph domains can define compatibility, root, cycle, reachability,
  and property semantics without changing `authoring::graph`.
- Structural validation remains usable even when no concrete domain is
  selected.
- A structurally valid graph may still be domain-invalid until explicit domain
  validation is run.
- Tooling that wants domain guarantees must call the domain validation helper
  explicitly after or alongside foundation structural validation.
- Graph transactions remain reusable across domains.

## Non-goals

Phase 3-C does not implement:

- Runtime graph execution.
- Compiled graph artifacts.
- Runtime ECS, renderer, or bridge integration.
- A production Behavior Tree domain.
- A production Shader Graph domain.
- A production Animation Graph domain.
- A production Blueprint-like visual scripting domain.
- GraphView-dependent domain validation.
- Auto-layout execution.
- Multi-document transactions.
- Editor, CLI, or MCP adapters.

## Phase 3-C Scope

Phase 3-C owns only the domain validation boundary proof:

- `authoring::graph_domain` module placement.
- `GraphDomain` trait.
- Explicit domain validation helper.
- Test-only or fixture domain schema registry.
- Test-only or fixture domain validation rules.
- Domain-owned port value type compatibility diagnostic.
- Stable diagnostic code tests for the fixture domain.
- Tests proving foundation transactions remain domain-agnostic.

## Validation Boundary

Validation is layered:

1. Foundation structural validation checks graph structure and schema
   references.
2. Domain validation checks selected-domain semantics.
3. GraphView validation checks presentation references and presentation value
   validity.

The recommended explicit helper is:

```rust
pub fn validate_graph_with_domain(
    graph: &Graph,
    domain: &dyn GraphDomain,
) -> Vec<Diagnostic>
```

The helper should run `graph.validate(domain.schema_registry())` first. If
foundation structural validation produces blocking diagnostics, the Phase 3-C
helper must skip domain validation. Domain validation assumes a structurally
valid graph. Running domain validation on a graph with missing nodes, missing
ports, or other structural errors can produce duplicate or unstable
diagnostics.

The helper does not emit graph kind mismatch diagnostics itself. Graph kind
support, including unsupported graph kind diagnostics, belongs to
`domain.validate_domain(graph)` so diagnostic code namespaces remain
domain-owned. A Phase 3-C fixture domain should use a stable code such as
`test_domain.unsupported_graph_kind`.

Domain diagnostic code namespaces belong to the domain. A Phase 3-C fixture
domain should use a stable namespace such as `test_domain.*`.

## Transaction Boundary

`GraphTransaction` modifies one semantic `Graph` document and remains
domain-agnostic.

`GraphViewTransaction` modifies one presentation `GraphView` document and is
not involved in domain validation.

Domain validation does not mutate `Graph`, `GraphView`, or transaction working
copies. It is a read-only validation pass.

Domain validation is not automatically part of `GraphTransaction::commit`.
Future editor, repository, CLI, or MCP layers may decide when to require
domain validation before accepting a graph edit, but that policy is outside
the foundation transaction boundary.

## Test Domain Boundary

The Phase 3-C fixture domain may define a small set of node schemas and port
value types solely to prove the boundary. Example fixture rules may include:

- Exact-match compatibility for fixture port value type identifiers.
- An unsupported graph kind diagnostic such as
  `test_domain.unsupported_graph_kind`.
- A required root node rule.
- A cycle rule, if needed to prove domain-owned graph traversal diagnostics.
- A domain-owned node property check.

These rules must remain fixture/test-domain behavior. They must not become
foundation behavior.

## Future Work

Future phases may add:

- A production first graph domain selected by a separate ADR.
- Domain compilation or interpretation.
- Runtime execution integration.
- Project repository policies that require domain validation before save.
- CLI, MCP, or editor adapters that expose domain validation.
- Domain-specific layout policy behavior.

## Alternatives Considered

### Run domain validation inside GraphTransaction::commit

Rejected because `GraphTransaction` belongs to the domain-neutral foundation.
Making commit depend on a selected domain would couple semantic graph storage
and transaction behavior to domain policy.

### Add type compatibility to foundation structural validation

Rejected because ADR 0009 assigns compatibility, coercion, inference, and
other semantic rules to concrete graph domains.

### Make GraphView part of domain validation

Rejected because GraphView is optional presentation data. Domain validity must
not depend on node coordinates, viewport state, selection state, or layout
documents.

## Compatibility and Migration

No persisted domain graph files exist yet, so no migration is required.

The `GraphDomain` trait and fixture diagnostics introduced in Phase 3-C become
public authoring API contracts. Diagnostic codes used by the fixture domain
must remain stable for tests, but production domains will own their own stable
diagnostic namespaces.
