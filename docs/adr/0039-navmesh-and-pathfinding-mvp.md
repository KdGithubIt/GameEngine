# ADR 0039: NavMesh and Pathfinding MVP

Status: Accepted
Date: 2026-06-14
Target Phase: Phase 44

## Context

Phase 44 needs pathfinding for simple enemies, scripted movement, and editor
testing. The engine does not yet have offline geometry baking, runtime obstacle
carving, or a dependency boundary for Recast-style navigation.

The first implementation should provide a stable gameplay API without forcing
the full bake pipeline to land in the same change.

## Decision

The MVP uses an engine-owned grid-backed `NavMesh` representation with A* path
queries. It exposes:

- `NavMesh`
- `NavMeshQuery`
- `NavMeshAgent`
- `nav_mesh_agent_system`

The grid representation is treated as the first runtime navmesh backend, not as
the final editor bake format. A later phase may add Recast or another offline
bake dependency behind the same high-level query API.

`NavMeshAgent` moves entities with `Transform` along a path computed from the
current `NavMeshQuery` resource. Dynamic obstacle carving and crowd simulation
are out of scope for Phase 44.

## Consequences

- Runtime pathfinding can be tested without an external native dependency.
- Scripts and gameplay systems get a stable path query surface early.
- The editor bake workflow remains a later task and can target the same runtime
  `NavMesh` contract.
- The MVP supports coarse navigation and deterministic tests, not production
  geometry baking.

## Alternatives Considered

### Recast immediately

Recast is the proven path for triangle-mesh navigation, but introducing native
binding, bake settings, and asset persistence would make Phase 44 much larger.

### No runtime pathfinding until full bake support

Waiting for full bake support would block scripting and gameplay iteration that
only needs basic path following.

## Compatibility and Migration

No persisted asset format is introduced by this ADR. If a later bake artifact is
added, it must version the serialized data and preserve the high-level
`NavMeshQuery` API where practical.
