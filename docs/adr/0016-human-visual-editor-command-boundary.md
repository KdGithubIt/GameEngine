# ADR 0016: Human Visual Editor Command Boundary

Status: Accepted
Date: 2026-06-06

## Context

Phase 8 introduces a human visual editor for graph authoring. The editor needs
graph rendering, selection, property editing, command-backed interactions,
pinning, and incremental layout.

The graph foundation already separates semantic graph data from presentation
graph view data:

- ADR 0007 defines `Graph` and `GraphView` as separate document boundaries.
- ADR 0010 defines `GraphView` as optional presentation data with its own
  transaction type.
- ADR 0011 keeps domain validation independent from `GraphView`.
- ADR 0015 keeps MCP tools as adapters over shared authoring services.

The editor must not become a second implementation of graph mutation,
validation, layout, or compilation. It also must not silently introduce atomic
multi-document transactions across `Graph` and `GraphView`, because ADR 0010
defers that design.

## Decision

The human visual editor is an adapter over the authoring model. It emits the
same command types and observes the same results as CLI and MCP.

Editor interactions that change semantic graph state MUST produce
`GraphCommand` values and apply them through the selected domain authoring
service or shared graph transaction API.

Editor interactions that change presentation state MUST produce
`GraphViewCommand` values and apply them through `GraphViewTransaction`.

The editor may group commands into a user-facing operation, but the operation
result MUST keep semantic and presentation results separate:

- `semantic_result` contains semantic diagnostics, semantic changes, and the
  updated `Graph` when semantic commit succeeds.
- `presentation_result` contains presentation diagnostics, graph view changes,
  and the updated `GraphView` when presentation commit succeeds.

The editor MUST NOT expose a single success state that implies atomic commit
across both documents.

For interactions that naturally touch both documents, such as creating a node
at a pointer position, the semantic transaction commits first. The presentation
transaction then applies the matching layout or selection change against the
latest semantic graph. If the presentation transaction fails, the semantic
change remains committed and the editor reports presentation diagnostics. The
user can then rerun layout or adjust the view.

## Editor State Ownership

Persisted editor state belongs in `GraphView` only when it is presentation data
already allowed by ADR 0010:

- Node positions.
- Collapsed state.
- Pinned state.
- Group bounds.
- Viewport pan and zoom.
- Selected nodes, edges, and groups.
- Layout policy identifier.
- Presentation annotations.

Transient UI state MUST remain outside persisted documents. Examples include:

- Hovered item.
- Active drag gesture.
- Rubber-band selection rectangle.
- Unsaved text field buffer.
- Context menu position.
- Temporary edge preview.
- Clipboard contents.

Semantic node properties, domain settings, and edge topology MUST remain in the
semantic `Graph`.

## Property Editing

The property inspector is command-backed.

Editing a semantic node property emits a semantic graph command that replaces
or removes the relevant property value on the node. Editing presentation fields
such as node position, collapsed state, pinned state, viewport, or selection
emits a graph view command.

The editor may debounce pointer movement or text input, but the command applied
to authoring state MUST represent the final accepted user edit. Intermediate
preview state may remain transient.

## Selection

Selection is presentation state. Persisted selection uses `GraphView.selection`
for selected nodes, edges, and groups.

Port selection, hover state, focus rings, and drag targets are transient until a
future ADR expands persisted selection.

Selection changes must not change semantic graph validation, domain
validation, compilation, interpretation, or runtime execution.

## Incremental Layout and Pinning

Phase 8 uses the existing domain layout policy as the source of deterministic
candidate positions. No external layout library is introduced for the Phase 8
editor merge policy. The broader graph layout library versus custom layout
decision remains open for future graph domains.

Incremental layout is a deterministic merge from:

1. The current semantic `Graph`.
2. The current optional `GraphView`.
3. A candidate `GraphView` produced by the selected domain layout service.

The merge rules are:

- Existing node layouts with `pinned == true` keep their position and pinned
  flag.
- Existing collapsed state and presentation annotations are preserved when the
  node still exists.
- Unpinned existing nodes may move to the candidate layout position.
- New nodes use the candidate layout position.
- Layout entries for missing semantic nodes are dropped.
- Viewport and selection are preserved when still valid against the semantic
  graph.
- The resulting graph view validates against the semantic graph before commit.

This policy provides stable behavior for manual pinning without making the
visual editor own graph layout algorithms.

## Rendering Boundary

The editor renderer is not an authoring data owner.

Rendering code may cache geometry, hit-test data, text metrics, and edge paths,
but those caches are derived from `Graph`, `GraphView`, domain schemas, and
diagnostics. Caches must be discardable and must not become serialized project
state.

## Required Phase 8 Adapter Operations

The first human editor adapter must provide operations equivalent to:

- Load or receive a `Graph` and optional `GraphView`.
- Query domain schemas for node palette and property inspector metadata.
- Validate the semantic graph and graph view.
- Create, delete, and edit semantic nodes through `GraphCommand`.
- Create and delete semantic edges through `GraphCommand`.
- Edit semantic node properties through `GraphCommand`.
- Select nodes, edges, and groups through `GraphViewCommand`.
- Move, collapse, pin, and unpin nodes through `GraphViewCommand`.
- Set viewport pan and zoom through `GraphViewCommand`.
- Apply incremental layout through the merge policy above.
- Return structured diagnostics and preview diffs for both documents.

CLI, MCP, and editor tests MUST be able to compare command effects for shared
semantic operations.

## Consequences

- Human editing reuses the same semantic commands as AI and CLI workflows.
- Editor-specific persistence stays in `GraphView`.
- The project does not need a multi-document transaction system before Phase 8.
- Node creation at a pointer position can leave a committed semantic node even
  if presentation persistence fails; this is explicit and recoverable.
- Incremental layout can ship with deterministic behavior while the broader
  layout-library decision remains open for future graph domains.

## Alternatives Considered

### Make the editor mutate `Graph` directly

Rejected. Direct mutation would duplicate validation and transaction behavior
already owned by the authoring model.

### Embed presentation state into semantic graph nodes

Rejected. ADR 0008 and ADR 0010 require semantic graph validity without
presentation data.

### Add atomic `Graph` plus `GraphView` transactions now

Rejected. Multi-document transactions are deferred by ADR 0010 and are larger
than the first human editor needs.

### Use an external layout library in Phase 8

Rejected for Phase 8. Behavior Tree already has a deterministic domain layout,
and the first editor needs predictable pin-preserving merges more than a
general layout engine. A future ADR may select a layout library for additional
graph domains.

## Compatibility and Migration

No persisted schema changes are introduced.

Existing semantic graph files and graph view files remain valid. The editor
uses existing `GraphCommand` and `GraphViewCommand` serialization contracts.

If a future multi-document transaction ADR is accepted, editor operation
results may gain an atomic operation mode while preserving the existing
separate semantic and presentation result fields for compatibility.
