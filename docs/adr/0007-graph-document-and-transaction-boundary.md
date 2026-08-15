# ADR 0007: Graph Document and Transaction Boundary

Status: Accepted
Date: 2026-06-05

## Context

Graph semantic data, graph presentation data, and scene data have different
lifecycles and validity rules.

A semantic graph must remain valid without node coordinates or other
presentation state. A graph view may be deleted and regenerated without
changing graph behavior. Scene entities may reference graphs in the future,
but scene editing and graph editing are separate document concerns.

Supporting atomic transactions across multiple documents would require a
project document store, multi-document identity and revision tracking,
conflict resolution, save ordering, rollback behavior, and undo history rules.
Those facilities do not exist in Phase 3.

## Decision

`Graph` is an authoring semantic document.

`GraphView` is a separate optional presentation document. It is not embedded in
the semantic graph document and is not required for the graph to be valid.

`AuthoringScene`, `Graph`, and `GraphView` are separate transaction boundaries.

Phase 3 supports only single-document transactions:

- A scene transaction modifies one `AuthoringScene`.
- A graph transaction modifies one semantic `Graph`.
- A future graph view transaction modifies one `GraphView`.

Phase 3 does not support an atomic transaction that spans:

- A scene and a graph.
- A graph and its graph view.
- Multiple graphs.
- Multiple graph views.

Graph commands must not require a graph view or pixel coordinates. Creating a
node modifies only the semantic graph document.

Phase 3-A implements semantic graph transactions only. `GraphView`,
multi-document transactions, and project-level graph repository commands such
as creating or deleting graph files are out of scope.

Implementation note: Phase 3-A graph transactions use private working
snapshots of a single semantic `Graph`. The snapshot preserves the source
graph's in-memory identity and revision for validation, while commit still
requires the target graph to have the same identity and base revision. A graph
loaded from serialized data receives a new in-memory identity and starts at
revision zero.

## Consequences

- Semantic graph edits remain valid when no view document exists.
- Node creation can be performed by AI, CLI, MCP, or tests without calculating
  pixel coordinates.
- Graph transactions can reuse the existing single-document transaction
  lifecycle: begin, apply, validate, preview diff, commit, or rollback.
- A semantic graph edit and a view update may be committed independently.
- Future multi-document editing requires a separate ADR before implementation.

## Alternatives Considered

### Embed `GraphView` in `Graph`

Rejected because presentation changes would alter semantic graph files, and
deleting presentation data could invalidate or damage semantic data.

### Require every graph command to update a view

Rejected because node creation must not require pixel coordinates and semantic
editing must work without presentation data.

### Implement multi-document transactions in Phase 3

Rejected because the required project document store, conflict model, save
ordering, and undo semantics are larger than the common graph foundation.

## Compatibility and Migration

No persisted graph or graph view documents exist yet, so no migration is
required.

Future multi-document transaction support must preserve the validity of
existing single-document graph transactions and must not make `GraphView`
mandatory.
