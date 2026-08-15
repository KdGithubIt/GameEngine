# ADR 0066: Project Component Sidecar Metadata

Status: Accepted (the legacy attribute path is removed by ADR 0091)

Date: 2026-07-19

> ADR 0091 removes the compatibility path described here. The
> `.rs.meta.json` sidecar is now the only identity source:
> `#[game_component(id = "...")]` is rejected, nothing backfills a
> sidecar from it, and the ID-mismatch diagnostic no longer exists.

## Context

Project Rust component IDs were literals in `#[game_component(id = "...")]`.
Although the ID itself was stable, keeping identity in editable source made
component creation, duplication, source navigation, and migration depend on a
mutable Rust declaration. ADR 0061 consequently indexed attributes and could
not resolve a component whose ID had been separated from its source.

## Decision

Each project component source has an editor-owned JSON sidecar with the suffix
`.rs.meta.json`. Version one stores only `schema_version` and `component_id`.
It never stores an absolute path. New project IDs use
`game.c_<lowercase Crockford ULID>` and are generated exactly once.

The sidecar is authoritative. The GameComponent derive resolves it during
compilation and exports the resulting ID through the existing GameModule ABI.
The editor indexes the same sidecar together with its paired source. Moving or
renaming a source moves the sidecar without changing the ID. Duplication
creates a new sidecar and new ID.

Legacy `#[game_component(id = "...")]` declarations remain accepted. When no
sidecar exists, project initialization may create one containing that exact
legacy ID. When both exist, the sidecar wins and an ID mismatch is a Problems
warning. A present but malformed sidecar never falls back to the attribute.

Missing, malformed, unsupported, invalid, and duplicate metadata is diagnosed.
No recovery path invents a replacement ID for an existing component. Scene and
prefab schemas remain unchanged because they already persist ComponentTypeId
keys.

The ordinary Inspector shows schema `display_name`, not the opaque project ID.
Source navigation resolves the selected ComponentTypeId through the sidecar
index. Raw IDs remain available to diagnostics and serialized documents, but
are not editable Inspector properties.

## Consequences

Rust type, field, and source file renames no longer affect component identity.
Sidecars must be version-controlled and must participate in source move,
rename, duplication, and deletion operations. A source file contains exactly
one sidecar-backed GameComponent so the file-level metadata is unambiguous.

ADR 0061's SDK source bundle and explicit external-editor decisions remain in
force. Its project-source discovery rule is superseded by this ADR.

## Alternatives Considered

Deriving IDs from type names or paths was rejected because either changes on a
normal refactor. Generating IDs in a build output directory was rejected
because the identity would not be a durable, reviewable project artifact.
Copying the source ID during duplication was rejected because it creates an
ambiguous registry and can attach scene data to the wrong Rust schema.

## Compatibility and Migration

Existing scenes and prefabs require no rewrite. Attribute-only components keep
their exact IDs and can be migrated by creating matching sidecars. Old source
syntax remains compilable during the migration period. Sidecar/attribute
mismatches are never reconciled silently.
