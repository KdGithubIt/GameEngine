# ADR 0010: GraphView Document and Presentation Transaction Boundary

Status: Accepted
Date: 2026-06-05

## Context

Phase 3-A completed the semantic graph foundation in `authoring::graph`.
`Graph` is an authoring semantic document that stores graph identity, kind,
nodes, edges, port references, semantic groups, properties, annotations,
schema, structural validation, commands, diffs, transactions, and deterministic
semantic serialization.

ADR 0006 through ADR 0009 establish that graph presentation data must not be
mixed into semantic graph data. A semantic graph must remain valid without
pixel coordinates, layout state, or a graph view file. Node creation and graph
mutation must not require presentation coordinates. Runtime execution, Runtime
ECS integration, renderer integration, bridge changes, concrete graph domains,
and port value type compatibility diagnostics remain outside the graph
foundation.

Phase 3-B needs a presentation document boundary for saving and editing visual
graph state without changing semantic graph behavior.

## Decision

Phase 3-B introduces `authoring::graph_view` as the module for graph
presentation documents.

`GraphView` is a separate optional presentation document. It is not embedded in
`Graph`. A `GraphView` references its semantic graph by storing the semantic
`GraphId`. `Graph` does not store a reference to `GraphView`.

Omitting or deleting a `GraphView` document does not invalidate the semantic
`Graph`. Semantic graph mutation must not require pixel coordinates or graph
view state.

`GraphView` may store:

- Node position.
- Node collapsed state.
- Node pinned state.
- Group bounds.
- Group collapsed or display state.
- Viewport pan and zoom.
- Selection state for selected nodes, edges, and groups.
- Layout policy identifier.
- Presentation annotations.

`GraphView` must not store:

- Semantic node properties.
- Graph edges as semantic data.
- Port type compatibility decisions or diagnostics.
- Runtime execution data.
- Runtime ECS entities, renderer data, or bridge state.
- Behavior Tree, Shader Graph, Animation Graph, Blueprint-like visual
  scripting, or other domain-specific graph semantics.

`LayoutPolicyId` may be persisted as presentation data in Phase 3-B, but
auto-layout algorithm execution is out of scope.

In Phase 3-B, `LayoutPolicyId` is a validated dotted identifier such as
`layout.manual`, `layout.force_directed`, or `layout.hierarchical`. The graph
view foundation stores and exposes the identifier only. Phase 3-B does not
perform behavior lookup, auto-layout execution, or domain-specific layout
policy behavior.

## Consequences

- Semantic graph files are not changed by presentation-only edits.
- A graph can be edited by non-visual tools without creating or updating a
  graph view document.
- Visual tools can persist presentation state without changing graph behavior.
- Graph view validation can report stale or dangling presentation references
  without mutating the semantic graph.
- Atomic edits spanning `Graph` and `GraphView` remain deferred.

## Non-goals

Phase 3-B does not implement:

- Auto-layout execution.
- Semantic graph mutation.
- Atomic multi-document transactions across `Graph` and `GraphView`.
- Runtime graph execution.
- Runtime ECS, renderer, or bridge integration.
- Behavior Tree, Shader Graph, Animation Graph, Blueprint-like visual
  scripting, or any other concrete graph domain.
- Port value type compatibility diagnostics in the foundation.
- Editor, CLI, or MCP adapters.

## Phase 3-B Scope

Phase 3-B owns only the presentation document foundation:

- `authoring::graph_view` module placement.
- `GraphView` presentation document model.
- Node layout presentation data.
- Group layout presentation data.
- Viewport pan and zoom.
- Selection state for selected nodes, edges, and groups.
- Layout policy identifier storage.
- Presentation annotations.
- Deterministic graph view serialization.
- Graph view structural validation.
- Graph view commands, change records, preview diffs, rollback, commit, and
  single-document transaction conflict detection.

## Validation Boundary

GraphView validation is limited to presentation reference integrity and
presentation value validity.

The minimum Phase 3-B validation API is:

```rust
GraphView::validate(&self, graph: &Graph) -> Vec<Diagnostic>
```

GraphView validation verifies that `GraphView.graph` matches `graph.id`.
Node, edge, and group references are checked against the provided `Graph`.
Validation must not mutate the semantic `Graph`.

Repository-level validation, resolver traits, and multi-document validation
are deferred to Future Work.

GraphView validation may reject or report:

- A graph view whose `graph` field does not match the semantic graph being
  validated against.
- Node layout entries for missing semantic nodes.
- Group layout entries for missing semantic groups.
- Selection references to missing semantic nodes, edges, or groups.
- Non-finite positions, sizes, pan values, or zoom values.
- Non-positive zoom.
- Negative group bounds sizes.

GraphView validation must not perform:

- Semantic graph validation.
- Domain-specific graph validation.
- Port value type compatibility checks.
- Runtime execution validation.
- Auto-layout quality checks.

## Transaction Boundary

`GraphViewTransaction` modifies one `GraphView` document.

`GraphTransaction` modifies one semantic `Graph` document and must not modify
`GraphView`.

`GraphViewTransaction` must not modify `Graph`. It may validate references
against a `Graph`, but commit applies only to the target `GraphView`.

Phase 3-B does not support an atomic transaction that spans:

- A semantic `Graph` and its `GraphView`.
- Multiple semantic graphs.
- Multiple graph views.
- A scene and a graph view.

Future multi-document transaction support requires a separate ADR before
implementation.

## Serialization Boundary

GraphView serialization is separate from semantic graph serialization.

The initial JSON naming convention remains:

```text
<name>.graph.json
<name>.graph.view.json
```

The graph view document serializes presentation data only. It must not
serialize runtime identifiers, renderer resources, compiled graph artifacts,
semantic node properties, or domain semantics.

Graph view serialization must use deterministic field ordering, deterministic
map or set ordering, typed identifier deserialization, and explicit schema
versioning where migration is required.

## Selection Persistence Decision

Selection state may be persisted in `GraphView`.

Persisted selection state is presentation and editor convenience state. It is
not semantic graph state and must not affect graph validation, domain
validation, compilation, interpretation, or runtime execution.

Phase 3-B persisted selection is limited to selected nodes, selected edges, and
selected groups. Port selection is not included in Phase 3-B.

Deleting or clearing selection state must not change semantic graph behavior.
Selection references must still be validated as presentation references when a
graph view document is validated.

## Future Work

Future phases may add:

- Auto-layout execution that reads semantic graph data and writes graph view
  presentation data.
- Graph view layout constraints or routing hints, if they remain presentation
  data.
- Project-level document repository behavior.
- Atomic multi-document transactions across semantic and presentation
  documents, after a separate ADR.
- Repository or resolver based graph view validation.
- Port selection, if a later editor workflow requires it.
- Editor, CLI, or MCP adapters for graph view inspection and mutation.
- Domain-specific default layout policies owned by concrete graph domains.
