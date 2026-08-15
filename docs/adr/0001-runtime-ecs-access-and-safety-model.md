# ADR 0001: Runtime ECS Access and Safety Model

Status: Accepted
Date: 2026-06-04

## Context

The initial ECS prototype allowed `SystemParam` values to be created from the
same `&mut World` through raw pointers without recording or validating their
access. This made signatures such as two `ResMut<T>` parameters, two
`Query<&mut T>` parameters, or `Query<(&mut T, &mut T)>` possible.

Those signatures can create multiple mutable references to the same value and
cause undefined behavior even when the schedule runs on a single thread.

The prototype also exposed `Storage` and `Archetype` mutation publicly, used
untyped string errors, allowed stale entity handles to reach reused entities,
and silently ignored some internal column mismatches.

The runtime ECS must be a reliable foundation before authoring, graph, editor,
CLI, MCP, or parallel scheduling features are added.

## Decision

### World-bound system parameters

`Query`, `Res`, `ResMut`, and `Commands` carry a lifetime bound to the `World`
from which they were fetched. They cannot safely escape that world borrow.

`Res` and `ResMut` wrap normal Rust references. They do not use raw pointers.

### Declared and validated access

Every built-in `SystemParam` declares its component and resource read/write
access when a system is constructed.

System construction rejects conflicting access, including:

- Read and write access to the same component or resource
- Multiple write accesses to the same component or resource
- Conflicts inside one query tuple
- Conflicts across multiple system parameters

The scheduler remains sequential initially, but each system retains its access
metadata so a future scheduler can determine which systems are compatible for
parallel execution.

`System` and `IntoSystem` implementations are restricted to constructors owned
by the ECS crate. External code may create stateful systems through closures,
but it cannot provide access metadata that does not match runtime behavior.
A future parallel scheduler may only trust access metadata from this validated
construction path.

### Query iteration rules

Read-only query data may be iterated from `&Query`.

Query data that may yield mutable references may only be iterated from
`&mut Query`. This prevents multiple mutable iterators from being created from
one query through safe code. Yielded references are tied to the iterator's
query borrow rather than the full world lifetime, so one mutable item cannot be
retained while the same query is reborrowed for another iterator.

Queries prepare raw pointers to validated, disjoint component columns before
yielding any component references. Query iteration does not repeatedly borrow
an entire `Archetype` while previously yielded references may still exist.
Component columns use an internal `UnsafeCell` boundary so mutable access is
granted per validated column rather than by repeatedly creating mutable
references to an entire archetype.

Unsafe operations remain internal to the ECS and require explicit safety
comments and focused tests.

### Structural mutation ownership

`World` is the only public owner of structural ECS mutation.

`Storage`, `Archetype`, their mutable accessors, fixed-ID spawning, command
queue internals, and entity allocator internals are not public runtime APIs.

Structural changes requested by systems use deferred runtime commands and are
applied after system execution.

### Typed failures and invariants

Expected runtime failures use typed errors rather than `String`.

Entity lookup validates both ID and generation. A stale entity handle cannot
read, mutate, or despawn an entity that later reused the same numeric ID.

Archetypes maintain these invariants:

- Every component column has the same length as the entity column.
- The declared component ID set matches the component column set.
- Each component ID is associated with exactly one Rust type.
- Entity metadata points to the correct archetype row.

Internal invariant violations are reported explicitly and are never silently
ignored.

### Runtime reflection boundary

The ECS `TypeRegistry` remains a minimal runtime type registry. Authoring
descriptions, serialized field schemas, validation constraints, and migration
metadata belong to a future authoring schema registry outside the runtime ECS
crate.

## Consequences

- Some early prototype APIs change before they become widely depended upon.
- Invalid system signatures fail during system construction instead of causing
  undefined behavior during execution.
- Direct world queries prevent other world access until the query is dropped,
  so callers must include required optional components in the query or collect
  owned data before accessing the world again.
- Mutable query iteration requires a mutable query binding.
- The ECS gains the metadata needed for future parallel scheduling without
  enabling parallel execution prematurely.
- External crates cannot implement custom `System` or `IntoSystem` traits
  directly. Stateful behavior remains supported through captured closure state.
- Query construction has a small preparation cost to resolve matching
  archetypes and component columns. Per-entity iteration avoids hash map
  lookups and repeated archetype borrowing.
- Runtime command application can report failures instead of silently
  discarding them.

## Alternatives Considered

### Rely on comments around raw pointers

Rejected because comments do not prevent conflicting system signatures or
multiple mutable iterators.

### Keep parameters lifetime-free for simpler function type inference

Rejected because lifetime-free parameters can escape system execution and make
the safety contract impossible to enforce with Rust's type system.

### Expose mutable storage and require callers to preserve invariants

Rejected because external structural mutation can invalidate entity metadata,
query assumptions, and archetype column lengths.

### Enable parallel scheduling immediately

Rejected because access metadata and sound parameter fetching must be proven
under sequential execution first. Parallel execution can be added later using
the same access model.

## Compatibility and Migration

The project is in an early prototype stage, so this ADR permits breaking API
changes in the runtime ECS.

Callers may need to:

- Make mutable query bindings explicit.
- Iterate mutable queries through `&mut query`.
- Handle typed `WorldError` values.
- Use `World` query and entity APIs instead of `Storage` or `Archetype`.
- Avoid accessing `World` while a direct query is alive.

Persisted authoring data is not affected because runtime ECS entities are not a
persisted format.
