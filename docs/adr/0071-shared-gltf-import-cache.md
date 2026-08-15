# ADR 0071: Shared glTF Import Cache for Repeated Conversions

- Status: Accepted
- Date: 2026-07-21

## Context

`spawn_from_authoring_scene` keeps its glTF caches (`BridgeAssetState.gltf_imports`
/ `gltf_textures`) local to one conversion so that atomic rollback owns every
asset it created. The editor Scene View rebuilds its preview world — and
therefore runs a full conversion — every frame (ADR 0068 relies on this).

These two decisions compose badly for imported models: every frame the bridge
re-read the glTF/GLB source from disk, re-parsed it, and re-decoded every
embedded image to RGBA8. For a typical downloaded model (a 4.4 MB GLB with
several multi-megapixel textures) this costs hundreds of milliseconds to
seconds per frame, freezing the editor. Because each frame produced new
allocations, the renderer's pointer-identity texture caches also missed every
frame, re-uploading all textures to the GPU per frame.

## Decision

The engine exposes `scene_bridge::SharedGltfImportCache`, an opt-in,
host-owned cache of parsed glTF documents and decoded textures:

- A host that converts repeatedly (the Scene View) creates one cache, keeps it
  across frames, and inserts a clone as a world resource before conversion.
  The bridge consults it in `import_gltf_cached` and texture decoding; hosts
  that do not insert the resource (Play mode, the player, packaging, tests)
  are unaffected.
- Conversion-local caches in `BridgeAssetState` stay authoritative for
  atomicity; the shared cache only stores immutable parse/decode results
  (`Arc<GltfImportResult>`, `Arc<DecodedTexture>`), which rollback never needs
  to undo.
- Entries are validated per lookup against a cheap file stamp (modification
  time + byte length) of the source file and each external sidecar recorded at
  parse time. A stale source entry is evicted together with every texture
  decoded from it.
- Cached `DecodedTexture` allocations are reused by pointer identity, so the
  renderer's `Arc::as_ptr`-keyed GPU caches hit across frames and skip
  redundant uploads.

## Consequences

- Scene View frame cost for imported models drops from re-parse/re-decode to
  one `stat` per source file plus `Arc` clones; large GLB models stay
  interactive.
- Editing or replacing a source file on disk is picked up automatically via
  the stamp check. A same-length in-place rewrite within the filesystem's
  mtime granularity could theoretically be missed; the editor's import flow
  always rewrites files, so this is accepted.
- The cache grows with each distinct source previewed during the editor
  session; it is bounded by project content like other editor-lifetime asset
  stores. A host can drop the whole cache to reclaim memory.
- Strict conversion hosts keep exactly their previous behavior because the
  resource is absent.
