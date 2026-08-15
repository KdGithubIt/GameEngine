# ADR 0078: Format-Independent Model Intermediate Representation

Status: Accepted
Date: 2026-07-21

## Context

`gltf_import.rs` (~1500 lines) currently performs parsing, normalization,
sub-asset ID derivation, skeleton construction, and clip conversion in one
pass. Two forces demand a boundary:

1. ADR 0077 and 0079 add skeleton identity, bone ID assignment, and
   retarget baking on top of import. Coupling that logic to glTF accessor
   plumbing would make every future format (FBX, USD) reimplement it.
2. Format quirks must not leak past the parser. The moment a PreRotation
   or unit-scale concern appears downstream of import, every consumer has
   to know about every format.

FBX/USD parsers are **not** in scope: each is a major dependency decision
(licensing, maintenance, compile time) requiring its own ADR. This ADR only
draws the boundary so they can be added without touching anything
downstream.

## Decision

### 1. One IR, produced by parsers, consumed by one asset builder

New module `crates/engine/src/model_ir.rs`:

```rust
/// Format-independent model document. Everything is already normalized
/// to engine conventions when this type exists (§2); consumers must not
/// know which format produced it.
pub struct ModelDocument {
    pub nodes: Vec<IrNode>,          // tree via parent indices
    pub meshes: Vec<IrMesh>,         // one entry per primitive-equivalent
    pub skins: Vec<IrSkin>,
    pub clips: Vec<IrClip>,
    pub materials: Vec<IrMaterial>,
    pub textures: Vec<IrTexture>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct IrNode {
    pub name: String,
    pub parent: Option<usize>,
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    pub mesh: Option<usize>,
    pub skin: Option<usize>,
}

pub struct IrSkin {
    pub name: String,
    pub joint_nodes: Vec<usize>,
    pub inverse_bind_matrices: Vec<Mat4>,
}

pub struct IrClip {
    pub name: String,
    /// Channels target *nodes*; the asset builder resolves them to
    /// BoneIds (ADR 0077) after skeleton construction.
    pub channels: Vec<IrChannel>,   // { node: usize, property, keyframes }
    pub duration: f32,
}
```

`IrMesh` / `IrMaterial` / `IrTexture` mirror the payloads of the existing
`GltfMeshData` / `GltfMaterialData` / `GltfTextureData` minus IDs and
selectors (IDs are assigned by the builder, not the parser).

### 2. Normalization contract — enforced at the parser boundary

A `ModelDocument` is always in engine conventions:

- Length unit: meters.
- Axes: right-handed, +Y up (the engine/glTF convention).
- Node TRS is plain local TRS; no format-specific pivot, PreRotation,
  GeometricTransform, or axis-conversion residue may survive into the IR.
- Keyframe times are seconds from clip start; rotations are unit
  quaternions; non-`LINEAR` source interpolation is resampled or
  downgraded by the parser with a diagnostic (existing rule).

The contract is documented on `ModelDocument` and covered by builder tests
that consume hand-written IR (no format file involved), which is the
mechanism that keeps the builder format-blind.

### 3. Split of `gltf_import.rs`

- `gltf_import.rs` shrinks to a parser: `parse_gltf(bytes, diagnostics) ->
  ModelDocument`. All accessor/buffer/URI handling stays here.
- New `model_import.rs`: `build_import_result(document: &ModelDocument,
  source_id: &AssetId, prior: Option<&ImportSettings>) -> GltfImportResult`.
  Sub-asset ID derivation (`imported_sub_asset_id`, unchanged), skeleton
  construction + identity + dedupe + reimport matching (ADR 0077), clip
  node→BoneId resolution, and the generated-prefab input all live here.
- `GltfImportResult` keeps its name and shape in this ADR. Its content is
  already format-agnostic; renaming it would churn the editor and the
  import cache (ADR 0071) for zero behavior. A rename can ride any future
  ADR that changes its shape.

The import cache (ADR 0071) keys on source bytes and continues to cache
the *result*; the IR is a transient in-memory stage and is not persisted.

## Consequences

- ADR 0077/0079/0080 logic is written once against IR-derived data and is
  untouched by any future parser.
- Builder behavior becomes testable with synthetic documents (unit tests
  without .glb fixtures), including skeleton dedupe and rebind cases.
- One extra in-memory copy of mesh/clip data during import; import is
  already an offline path and the copy is transient.
- Adding FBX later = writing `parse_fbx -> ModelDocument` + its own ADR
  for the dependency; nothing downstream changes.

## Alternatives Considered

- **Keep one-pass import and add retarget logic inside it** — rejected:
  the exact coupling this ADR exists to prevent; already at ~1500 lines.
- **Persist the IR as an asset** — rejected: derived, format-shaped data
  with no consumer; the import cache already de-duplicates repeated parses.
- **Rename `GltfImportResult` to `ModelImportResult` now** — rejected as
  cosmetic churn across editor/import-cache call sites; revisit when its
  shape actually changes.

## Compatibility and Migration

- Pure refactor at the public surface: import entry points keep their
  signatures; no persisted format, ID derivation, or manifest change.
- Same-PR updates for any internal call sites that reached into
  `gltf_import` internals now owned by `model_import`.
