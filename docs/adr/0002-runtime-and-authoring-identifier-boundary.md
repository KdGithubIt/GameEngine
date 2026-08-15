# ADR 0002: Runtime and Authoring Identifier Boundary

Status: Accepted
Date: 2026-06-04

## Context

The runtime engine needs compact process-local identifiers for entities and
loaded assets. The future authoring model needs stable identifiers that remain
valid across editor sessions, builds, Git changes, and runtime world rebuilds.

Using the same type name for both scopes makes it easy for human contributors,
AI agents, serializers, or adapters to persist an ephemeral runtime value by
mistake.

## Decision

Runtime and authoring identifiers are distinct types with distinct names.

- Runtime ECS entities use `Entity`.
- Runtime asset storage uses `RuntimeAssetId`.
- Persisted authoring entities will use `EntityId`.
- Persisted authoring assets will use `AssetId`.

Runtime identifiers are ephemeral and must never be serialized as authoring
references. Authoring-to-runtime conversion must maintain an explicit mapping
when runtime entities or assets need to refer back to authoring objects.

## Consequences

- Type names communicate whether an identifier may be persisted.
- CLI, MCP, editor, and serialization code can reject runtime identifiers at
  their boundaries.
- Runtime storage remains free to use compact or generation-based IDs.
- Build and play pipelines must create explicit authoring-to-runtime maps.

## Alternatives Considered

### Use `AssetId` for both scopes and rely on documentation

Rejected because documentation does not prevent accidental serialization or
ambiguous API signatures.

### Make runtime IDs stable across sessions

Rejected because runtime storage and authoring data have different lifecycle,
performance, and migration requirements.

## Compatibility and Migration

The early runtime engine asset identifier was renamed from `AssetId` to
`RuntimeAssetId`. No persisted authoring format exists yet, so no data
migration is required.
