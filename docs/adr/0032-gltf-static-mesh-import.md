# ADR 0032 — glTF / GLB Static Mesh Import

## Status: Accepted

## Context

Phase 36 adds `.gltf` and `.glb` import to the engine.  Two decisions must
be recorded before implementation:

1. Which Rust crate handles glTF / GLB parsing?
2. How are sub-asset IDs (mesh, material, texture within one file) kept
   **deterministic** across re-imports so that scene references remain stable?

## Decision

### Parsing crate

Use `gltf = "1"` (the canonical Rust glTF / GLB parser, MIT licensed).  The
dependency is added only to `crates/engine/Cargo.toml`.  No other crate
gains a new dependency.

### Deterministic sub-asset IDs

Add `AssetId::derive(parent: &AssetId, discriminant: &str) -> AssetId` to
`crates/authoring/src/id.rs`:

1. Concatenate `"{parent}:{discriminant}"` as the input string.
2. Hash with two passes of FNV-1a-64 to obtain a 128-bit value.
3. Wrap the value in `ulid::Ulid::from(u128)` and format as `"asset_{ulid}"`.

This produces a valid `AssetId` that is deterministic for equal inputs and
unique with very high probability.  The `AssetId::derive` method lives in
`engine-authoring` so it is available everywhere `AssetId` is used.

### Import scope (Phase 36)

| Item | Decision |
|------|----------|
| Static meshes (positions, normals, UV ch.0) | Imported |
| Vertex colors | Set to white; not read from glTF in v1 |
| Animation tracks | Silently ignored (Phase 37) |
| External buffer URIs (`.bin` sidecars) | Not supported in v1 |
| Image decoding | Not performed in v1 |

Only embedded buffers (data URIs in `.gltf` or GLB binary chunks) are
resolved.  Missing normals or UVs generate non-fatal `Diagnostic` warnings.

## Consequences

- Sub-asset IDs are stable across re-imports of the same file with the same
  parent `AssetId`.
- FNV-1a has no security properties; sub-asset IDs are not secret, so this
  is acceptable.
- External-buffer `.gltf` files require the caller to stitch buffers before
  calling `import_gltf_bytes`; this is documented in the function signature.

## Editor Ready v1 extension (2026-07-13)

The original Phase 36 scope above remains the contract of
`import_gltf_bytes`, but the production asset workflow now uses
`import_gltf_path`. Path import resolves external buffer and image URIs
relative to the source document and records those paths in manifest v2 import
settings. The importer additionally preserves tangents, skinning data,
inverse-bind matrices, animation channels, decoded textures, and supported PBR
material fields.

Every imported category uses the original glTF selector in its derivation
discriminant (`mesh:N`, `material:N`, `texture:N`, `skeleton:N`, `skin:N`, or
`animation:N`). Reimport may change names and content fingerprints without
changing references as long as those selectors remain present. Import parsing
runs on a cooperative cancellable editor worker; only a complete successful
result replaces the previous catalog.
