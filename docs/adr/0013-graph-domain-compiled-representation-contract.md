# ADR 0013: Graph Domain Compiled Representation Contract

Status: Accepted
Date: 2026-06-05

## Context

The graph domain contract needs a deterministic compilation or interpretation
boundary before Phase 4 begins. The existing Phase 3-C `GraphDomain` trait
proves schema and validation boundaries, but it intentionally does not define
the concrete compiled graph contract.

Different graph domains produce different runtime representations. Behavior
Trees produce ordered trees, Shader Graphs may produce shader artifacts, and
Animation Graphs may produce state machines or blend graphs. A single concrete
`CompiledGraph` type in the shared foundation would either be too generic to
be useful or would leak domain semantics into domain-neutral code.

## Decision

Compiled graph artifacts are domain-owned associated types.

Production graph domains should use a contract equivalent to:

```rust
pub trait GraphDomain {
    type Compiled;

    fn graph_kind(&self) -> &GraphKind;
    fn schema_registry(&self) -> &dyn GraphSchemaRegistry;
    fn validate_domain(&self, graph: &Graph) -> Vec<Diagnostic>;
    fn compile(&self, graph: &Graph) -> Result<Self::Compiled, Vec<Diagnostic>>;
    fn layout_policy(&self) -> LayoutPolicyId;
}
```

`compile` must be deterministic for a given graph and domain implementation.
The returned `Compiled` type is owned by the concrete domain crate or module.
The shared graph foundation must not inspect, serialize, execute, or interpret
compiled artifacts.

`compile` must not mutate `Graph` or `GraphView`. It may call layered
validation first and must return structured diagnostics when compilation
cannot proceed. Diagnostic codes are domain-owned.

For Phase 4 Behavior Tree, the compiled representation will be a deterministic
domain-owned tree or tree-like structure containing stable node references,
ordered children, and behavior identifiers or properties needed by the first
runtime interpretation target.

## Consequences

- Shared graph storage remains domain-neutral.
- Each graph domain can expose a useful typed compiled artifact without
  forcing unrelated domains into the same runtime shape.
- Tests can assert deterministic compilation by comparing domain-owned
  compiled values or stable serialized test snapshots.
- CLI, MCP, and editor layers must treat compiled artifacts as domain
  outputs, not as generic graph foundation data.
- Cross-domain artifact exchange, if needed later, requires a separate ADR.

## Alternatives Considered

### One shared `CompiledGraph` enum

Rejected because it would require the shared foundation to know every
production graph domain and would make adding new domains a foundation change.

### Opaque byte buffer

Rejected for the Phase 4 contract because it would hide useful structure from
tests and adapters. A domain may later serialize its compiled artifact to
bytes, but that is an artifact format decision, not the core compile contract.

### Foundation-owned interpreted graph

Rejected because runtime execution semantics differ substantially by domain.
The foundation should not own behavior-tree ticking, shader evaluation,
animation blending, or visual scripting control flow.

## Compatibility and Migration

The Phase 3-C `GraphDomain` trait remains a validation-boundary proof and may
evolve when the first production domain is implemented. No persisted compiled
artifacts exist yet, so no migration is required.

This ADR resolves the Phase 4 prerequisite for the `CompiledGraph` concrete
type and `GraphDomain::compile` return contract by assigning compiled
representation ownership to each graph domain.
