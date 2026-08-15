# ADR 0005: Authoring Transaction and Undo Strategy

Status: Accepted
Date: 2026-06-04

## Context

Phase 2 introduces authoring commands, transactions, preview diff, commit,
rollback, and undo/redo. Before implementing these features, the undo storage
strategy must be clarified, because the choice shapes the `CommandResult`,
`Change`, and `Transaction` API surfaces.

Three common undo strategies have different trade-offs:

1. **Inverse command**: each applied command returns an inverse command that
   reverses the mutation. Simple to implement. Scales poorly for compound
   operations, large graph edits, or complex move operations where the inverse
   is as expensive to store as the original state.

2. **Snapshot**: the full authoring state is captured before a mutation and
   restored on undo. Easy to reason about. Memory cost scales with project
   size and history depth, making it impractical for large projects.

3. **Patch/diff**: a structural diff between before and after states is
   stored. Compact and content-addressable. Requires a robust diff algorithm
   over the authoring data model that handles nested components, graph
   topology, and reference changes.

4. **Hybrid**: different strategies applied per operation class. For example,
   inverse commands for simple property changes and snapshots for large graph
   restructuring.

A complicating factor is schema migration. If a component schema version
changes after undo history exists, applying an old inverse command or patch
against the new schema version may fail or produce invalid data. The strategy
must either handle schema-versioned history or limit undo depth across
migration boundaries.

## Decision

### Phase 2 scope

Phase 2 implements the following:

- `AuthoringCommand` enum with command variants.
- `CommandResult` carrying `changes`, `diagnostics`, and an optional inverse
  or equivalent undo token — whichever the chosen strategy requires.
- `Change` records that describe the semantic effect of a command.
- A `Transaction` type with isolated state that supports:
  - Applying one or more commands.
  - Previewing a semantic diff before commit.
  - Committing to persisted project data.
  - Rolling back completely when validation fails or the caller requests it.
- Structured diagnostics for validation failures.

### What is deferred

The persistent undo history storage strategy is intentionally not fixed in
this ADR. The three alternatives (inverse command, snapshot, patch, hybrid)
and their memory management policies (history limits, eviction) require
measurement and design work that is separate from the transaction commit and
rollback mechanism.

A future ADR MUST select the undo storage strategy before persistent undo
history is exposed to callers. Until that ADR is accepted, undo/redo history
MUST remain session-local and MUST NOT be persisted across process restarts.

### API constraints that preserve future options

The `CommandResult` and `Change` types MUST be designed so that none of the
three undo strategies require a breaking API change later:

- `CommandResult` MAY carry an `inverse: Option<AuthoringCommand>` field if
  the inverse-command strategy is chosen later, but the field SHOULD remain
  `Option` to permit strategies that do not use per-command inverses.
- `Change` records MUST describe affected identifiers and old and new values
  at a semantic level, so that a patch or diff strategy can derive its state
  from the change records without re-reading the full project.
- `Transaction` MUST NOT expose a method for accessing persisted undo history
  until the storage strategy ADR is accepted.

### Phase 2 minimum

The following is the minimum required to complete Phase 2:

1. Rollback: discarding uncommitted transaction state, leaving persisted data
   unchanged.
2. Preview diff: returning a `Change` list before commit so callers can
   inspect or display what will change.
3. Validation: all validation errors block commit and are returned as
   structured diagnostics.

Persistent undo history (surviving transaction commit) MAY be deferred to a
later phase. The Phase 2 completion criteria in the specification require only
that a committed transaction can be undone and redone; an in-memory
session-level undo stack satisfies this while the storage ADR is pending.

### Compound and large operations

Commands that affect many nodes or edges in a single logical operation
(for example, auto-layout, graph refactor, or bulk schema migration) MUST
produce a single undo history entry. They MUST NOT create one entry per
affected node, as this makes undo indistinguishable from pressing undo
hundreds of times.

The undo strategy MUST account for this requirement. An inverse-command
approach must either batch the individual inverses into a compound inverse, or
the strategy must be reconsidered for bulk operations.

### Schema migration boundary

Operations that change the persisted schema version of a component type are
treated as migration boundaries. Undo history entries created before a
migration boundary MUST be clearly marked. The undo strategy MAY choose to
discard history across a migration boundary rather than risk applying an
inverse that is incompatible with the new schema version. This policy MUST be
documented in the storage strategy ADR.

## Consequences

- Phase 2 delivers transactional editing with commit, rollback, and preview
  diff.
- Persistent undo/redo across committed transactions is deferred until the
  storage strategy ADR is accepted.
- The `CommandResult` and `Change` API is designed to accommodate all three
  storage strategies without breaking changes.
- Callers can undo and redo committed transactions within one in-memory
  authoring session, but cannot assume history survives a process restart.
- Compound and bulk operations explicitly produce a single undo entry to
  prevent unusable undo history.

## Alternatives Considered

### Fix inverse-command as the undo strategy now

Rejected because inverse commands do not scale well for large graph operations
and may fail silently after schema migrations. Committing to this strategy now
forecloses better options.

### Fix snapshot as the undo strategy now

Rejected because snapshot undo stores a full copy of the authoring state per
committed transaction. For projects with large scenes and many committed
operations, this is impractical. Deferring allows profiling before selecting
a strategy.

### Defer all undo/redo consideration to Phase 5+

Rejected because `CommandResult` and `Transaction` API design must account for
undo now. Implementing commands without undo in mind leads to API breaks when
undo is added. The deferral in this ADR applies to the persistent store, not
to the API design.

## Compatibility and Migration

No persisted authoring data is affected. The transaction strategy operates on
in-memory state during an editing session. Project files after a commit do not
contain undo history; this is an in-editor concern only.

When the storage strategy ADR is accepted, it may introduce new persisted
session state (for example, a `.undo` file alongside a scene file) if durable
undo across editor restarts is desired. That decision is out of scope for this
ADR.
