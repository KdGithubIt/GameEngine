# ADR 0030: Prefab Schema v1

Status: Accepted
Date: 2026-06-14

## Context

Phase 33 adds the ability to save a selected entity (and its children) as a
reusable `.prefab.json` asset and to instantiate it multiple times in a scene
without `EntityId` collisions.  The serialized schema touches persisted
project data and the `EntityId` remap contract is shared across crates, so an
ADR is required before any implementation.

## Decision

### File layout

Prefab assets are stored under `assets/` with the suffix `.prefab.json`.
They carry a `schema_version` field following the ADR 0020 convention.

```json
{
  "schema_version": 1,
  "root": "ent_01JP…",
  "entities": {
    "ent_01JP…": {
      "name": "Player",
      "description": "",
      "parent": null,
      "components": {
        "engine.transform": { … }
      }
    },
    "ent_01JP…child": {
      "name": "Weapon",
      "parent": "ent_01JP…",
      "components": {}
    }
  }
}
```

### EntityId remap

The prefab file stores **authoring-stable EntityIds** (ULID format per ADR
0004), but these IDs are local to the prefab definition and must **not** be
reused in the live scene.  On every instantiation, `EntityId::generate()` is
called for every entity in the prefab.  The mapping from prefab-local IDs to
newly-generated IDs is used to rewrite all `parent` references so the entity
tree structure is preserved.

**The prefab file must not store live runtime ECS `Entity` values.**

### "Save as Prefab"

Saving a selection as a prefab:
1. Identifies the root entity (the one with no parent, or whose parent is
   outside the selection).
2. Traverses the selection to collect all entities in the subtree.
3. Serializes the entity tree with authoring-stable IDs into `.prefab.json`.
4. Registers the resulting file in the asset manifest under a fresh `AssetId`.

### Prefab overrides

Instance-level field overrides are **explicitly out of scope for v1**.  The
schema must not make overrides harder to add in a future version; this is
satisfied by the per-entity `components` map, which allows a future ADR to
introduce an `overrides` section alongside it.

## Consequences

- Prefab instantiation is deterministic and collision-free because every
  `EntityId` is freshly generated.
- The prefab file is a standalone document that can be version-controlled and
  diffed at the entity level.
- Overrides are deferred; this is a deliberate design choice, not an oversight.
- Prefab drop from the Asset Browser (Phase 32) can use the existing drag-drop
  infrastructure and call `instantiate_prefab`.

## Alternatives Considered

### Store runtime Entity values in the prefab

Rejected. Runtime ECS `Entity` values are process-local and cannot survive
serialization.  Using them would make the prefab file unusable across process
restarts.

### Inline the prefab definition inside the scene file

Rejected. A separate `.prefab.json` file can be shared across multiple scenes,
placed in version control independently, and tracked in the asset manifest.

## Compatibility and Migration

The prefab format is new; there are no existing files to migrate.  The
`schema_version` field is written on save so that a future ADR can introduce
version 2 with override support while remaining readable by this version (v1
implementations reject unknown schema versions gracefully via an error rather
than silent data loss).
