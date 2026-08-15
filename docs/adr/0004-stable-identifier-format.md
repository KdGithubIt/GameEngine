# ADR 0004: Stable Identifier Format

Status: Accepted
Date: 2026-06-04

## Context

The authoring model requires stable, persistent identifiers for entities,
assets, graphs, nodes, edges, and other project objects. The previous
specification described identifiers as human-readable slugs such as `player`
or `detect_player`, with a note that the engine should generate suffixed IDs
on conflict.

Human-readable slugs create two problems:

1. Multi-agent and multi-branch editing causes silent ID collisions when two
   contributors independently use the same name for different objects.
2. Slug-based IDs conflate the identifier (stable, opaque) with the display
   name (mutable, human-readable), making it easy to assume renaming is safe
   when it would invalidate all references.

## Decision

Stable identifiers use the format `<prefix>_<ULID>` where:

- The prefix identifies the object kind (`entity`, `asset`, `graph`, `node`,
  `edge`, `port`, `group`).
- The ULID (Universally Unique Lexicographically Sortable Identifier) provides
  monotonic, time-ordered uniqueness without coordination or a central registry.

Examples:

```
entity_01JZXYZ...
asset_01JZXYZ...
graph_01JZXYZ...
node_01JZXYZ...
```

Identifiers are generated once at object creation. Renaming an object changes
its `name` field only. The identifier never changes.

Human-readable and AI-readable information is stored in separate mutable
fields on each object:

- `name`: short lowercase slug for search and CLI reference.
- `display_name`: UI label.
- `description`: extended documentation and AI context.

AI agents and CLI tools SHOULD search by `name`, `type`, or description to
locate objects. They MUST use the `StableId` string for the final edit target.

Uniqueness scope:

| Identifier | Scope         |
| ---------- | ------------- |
| `EntityId` | Project-wide  |
| `AssetId`  | Project-wide  |
| `GraphId`  | Project-wide  |
| `NodeId`   | Graph-local   |
| `EdgeId`   | Graph-local   |
| `PortId`   | Node-schema-local |
| `GroupId`  | Graph-local   |

## Consequences

- ID collisions in multi-agent or multi-branch editing are effectively
  impossible.
- Renaming is safe and does not require reference updates.
- Human contributors cannot author IDs by hand; IDs must be generated.
- CLI and MCP tools need search-then-select workflows: find the object by
  name or type, then apply changes using its `StableId`.
- Serialized project files contain opaque ID strings that are not meaningful
  to humans but are stable and reviewable in diffs.
- Migration from any prior slug-based ID scheme requires a one-time ID
  generation pass.

## Alternatives Considered

### Human-readable slugs with conflict detection

Rejected because collision detection requires a coordinating authority.
In distributed editing, two contributors can independently create the same
slug in separate branches, and no tool catches the conflict until merge.

### UUID v4 (random)

Not selected over ULID. ULID has the same uniqueness properties but is
lexicographically sortable and time-ordered, which makes diffs and logs
more readable. Either format satisfies the requirements; ULID is preferred.

### Stable content-addressed identifiers

Rejected because content-addressed IDs change when content changes,
which defeats the purpose of a stable identifier.

## Compatibility and Migration

No persisted authoring format exists yet, so no data migration is required.

The previous specification guidance to use readable slugs is superseded by
this ADR. Section 7.1 of `AI_FRIENDLY_AUTHORING_SPEC.md` has been updated
to reflect this decision.
