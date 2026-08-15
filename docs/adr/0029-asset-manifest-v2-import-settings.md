# ADR 0029: Asset Manifest v2 — Import Settings Extension

Status: Accepted
Date: 2026-06-14

## Context

Phase 14 / ADR 0021 established the `asset_manifest.json` format mapping
`AssetId → {path, name}` (schema version 1).  Phase 31 requires per-asset
import settings (texture compression, mesh LOD targets, audio format) and
reimport-tracking state so that the editor can detect missing or orphaned
assets and know when a source file must be reimported.

Two storage strategies were considered:

1. **Manifest extension**: add optional fields to each entry in
   `asset_manifest.json` (schema version 2).
2. **Sidecar `.meta` files**: one `<asset>.meta` file per source file, stored
   alongside the source in `assets/`.

## Decision

**Extend the existing manifest with optional import settings.  Do not
introduce `.meta` sidecar files.**

The manifest is bumped to **schema version 2**.  All existing fields remain
unchanged and all `import_settings` sub-fields are optional, so a v1 manifest
can be read without migration.  `AssetManifest::from_json` accepts both
schema versions 1 and 2.  `AssetManifest::to_canonical_json` always writes
schema version 2.

### Import settings fields (initial set)

Each manifest entry gains an optional `import_settings` object.  Missing
sub-fields use the defaults listed below.

```json
"import_settings": {
  "texture_compression": "none",
  "mesh_lod_target_count": null,
  "audio_format": "pcm"
}
```

| Field | Type | Default | Applies to |
|-------|------|---------|-----------|
| `texture_compression` | `"none"` \| `"bc"` | `"none"` | Texture |
| `mesh_lod_target_count` | `u32` \| `null` | `null` (no LOD) | Mesh |
| `audio_format` | `"pcm"` \| `"vorbis"` | `"pcm"` | Audio |

### Editor Ready v1 imported-source catalog extension (2026-07-13)

The same schema-version-2 `import_settings` object also persists optional
source import state:

| Field | Type | Default | Purpose |
|-------|------|---------|---------|
| `source_fingerprint` | string \| null | null | Deterministic content fingerprint over the source and sidecars |
| `source_dependencies` | string array | empty | Asset-root-relative external buffers and images required for packaging |
| `sub_assets` | object array | empty | Stable derived ID, kind, source name, and original selector for each imported item |

Derived mesh, material, texture, skeleton, skin, and animation IDs remain
nested under the single registered source entry. They are not duplicated as
top-level path entries. This makes a successful reimport one atomic manifest
update while `AssetManifest::imported_sub_asset` provides reverse lookup.
Missing fields preserve compatibility with all previously written v2 files.

### Compatibility story

- Schema version 1 manifests are accepted without migration.  The reader
  treats absent `import_settings` as all-defaults.
- `AssetId → path` mapping is unchanged.  Existing callers that only read
  `path` and `name` continue to work without modification.
- Schema version 2 manifests written by this version will be rejected by
  older builds (they check `schema_version != 1`). This is intentional:
  once import settings are written, the file must not be silently
  downgraded.

## Consequences

- A single file continues to be the source of truth for all registered
  assets, avoiding cross-file consistency issues from `.meta` files.
- The manifest grows with the number of assets, but typical project
  manifests remain small.
- `AssetManifest` is the integration point for reimport decisions; if a
  source file's mtime differs from the manifest's expectation, the editor
  can flag it as a missing/orphaned diagnostic.
- `.meta` sidecar files are explicitly **not** part of this project's asset
  strategy. A future ADR would be required to change this.

## Alternatives Considered

### Sidecar `.meta` files

Rejected. Sidecar files create a new consistency problem: the manifest and
the `.meta` file may disagree on the import settings for the same asset.
Managing two canonical sources of truth for the same asset identity is error-
prone and harder to validate.  The manifest already has the `AssetId → path`
mapping; adding import settings there is a single-file extension.

## Compatibility and Migration

The existing `asset_manifest.json` files (schema version 1) are read without
change.  When the editor saves an updated manifest it writes schema version 2,
which is not readable by builds that predate this ADR.  Teams using version
control can migrate by letting the editor rewrite the manifest on first save.
