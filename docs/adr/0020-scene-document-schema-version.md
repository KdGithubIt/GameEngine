# ADR 0020: Scene Document Schema Version

Status: Accepted
Date: 2026-06-10

## Context

Graph documents carry `schema_version: u32` (`authoring::graph::Graph`), but
the serialized scene form produced by `AuthoringScene::to_canonical_json` and
consumed by `load_scene_from_json` is a bare `{ "entities": [...] }` object
with no version marker (`crates/authoring/src/scene.rs` `SceneFileRef`,
`crates/authoring/src/load.rs` `SceneFile`).

Specification §15.2 requires schema versions for migratable documents.
Phase 9-D turns scenes into a user-persisted project format. Adding a version
field after that point would require migrating files that carry no
self-describing marker.

## Decision

1. The serialized scene document gains a required top-level
   `schema_version: u32` field. The current format is version `1`.
2. `AuthoringScene::to_canonical_json` always writes `schema_version` as the
   first field, before `entities`.
3. `load_scene_from_json`:
   - A missing `schema_version` is treated as version `1`, so files written
     before this ADR continue to load.
   - A `schema_version` greater than the supported version returns a new
     structured error (`SceneLoadError::UnsupportedVersion`), never a silent
     best-effort parse.
4. Every new persisted authoring document kind introduced from now on
   (asset manifest, project file) MUST carry `schema_version` from its first
   release, following this same load policy. This concretizes the existing
   §15.2 requirement; the specification text itself does not change.

## Consequences

- Scene migrations become possible before any real project content exists.
- `SceneLoadError` gains one variant; loaders and tests are updated.
- Canonical scene JSON output changes (one added field); golden tests and
  the scene JSON examples in module documentation are updated in the same
  change.

## Alternatives Considered

- No version, infer format by shape: rejected; shape inference is ambiguous
  exactly when migration is needed.
- Reuse the editor wrapper's `format_version` name (ADR 0019): rejected;
  scenes are semantic documents and align with `Graph::schema_version`.

## Compatibility and Migration

Additive. Old unversioned files load as version 1. Readers built before this
ADR also tolerate newer files during the transition because the scene
deserializer does not reject unknown fields. CLI, MCP, and runtime formats
are unaffected.
