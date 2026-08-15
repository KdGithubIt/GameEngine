# ADR 0018: Phase 8-B Undo/Redo Snapshot Strategy

Status: Accepted
Date: 2026-06-06

## Context

ADR 0005 deferred the persistent undo storage strategy, requiring only that
a future ADR select one before exposing undo history to callers. ADR 0005
explicitly permits a session-local in-memory undo stack as the minimum for
Phase 2 completion and states that an in-memory stack satisfies the spec goal
while the storage strategy ADR is pending.

Phase 8-B introduces the first user-facing undo/redo capability in the human
visual editor. Three strategies were considered: inverse command, snapshot, and
patch/diff.

The editor session holds two documents: a `Graph` (semantic) and an optional
`GraphView` (presentation). Both have full `Serialize + Deserialize`
implementations. Neither derives `Clone`.

## Decision

Phase 8-B implements **session-local snapshot undo** using JSON serialization.

Before each user-visible operation (`add_behavior_node`, `delete_node`,
`connect_child`, `move_node`, `set_node_pinned`, `set_node_property`,
`apply_incremental_layout`) the current `Graph` and `Option<GraphView>` are
serialized to JSON strings and stored as one `UndoEntry`. On undo, the entry
is deserialized and replaces the current session state. On redo, the displaced
state is saved as a new undo entry.

History is session-local and does not survive process restarts.

The undo depth limit is 100 entries. Entries beyond the limit are evicted
from the front of the undo stack (oldest first).

Diagnostics are not part of the snapshot. After undo or redo, domain
validation is re-run against the restored graph to produce fresh diagnostics.

Compound operations (`add_behavior_node`, `delete_node`) push exactly one
checkpoint before executing all their sub-steps. Internal primitives
(`apply_graph_command`, `apply_graph_view_command`, `set_node_layout`,
`select_node`) do not push checkpoints.

`select_node` is not undoable. Selection is a transient presentation state
used for navigation and does not represent a content change.

## Consequences

- Undo and redo work for all user-visible content operations.
- A single undo step reverts a compound operation atomically.
- History is lost on editor exit. Users must save before closing.
- Memory cost: each entry stores two JSON strings; for typical Behavior Tree
  graphs with tens of nodes the cost is small.
- No change to `Graph`, `GraphView`, or authoring command APIs.
- A future ADR may replace this strategy with inverse commands or patch/diff
  without changing the `EditorSession` public interface, because undo is
  exposed only as `undo() -> bool` and `redo() -> bool`.

## Alternatives Considered

### Inverse command strategy

Rejected for Phase 8-B. `GraphViewCommandResult.inverse` already carries
inverse view commands, but `GraphTransaction` does not yet produce inverses
for semantic commands. Implementing inverse semantic commands before they are
needed for undo would add scope without a direct requirement. Snapshot undo
requires no authoring API changes.

### Patch/diff strategy

Rejected for Phase 8-B. A structural diff over `Graph` and `GraphView` is
compact but requires a general-purpose diff algorithm across the full data
model. This complexity is not justified for the prototype stage.

### Clone-based snapshot

Considered. Adding `Clone` to `Graph` and `GraphView` would be more
efficient than JSON roundtrip. Rejected because: (1) JSON roundtrip is
already verified to work via `to_canonical_json` / `serde_json::from_str`;
(2) modifying `engine-authoring` types solely for editor convenience is not
required; (3) for Behavior Tree graphs of prototype scale the difference is
not user-visible.

## Compatibility and Migration

No persisted authoring data or serialized format is affected. Undo history is
in-memory only. This ADR does not affect CLI, MCP, ECS, or runtime APIs.
