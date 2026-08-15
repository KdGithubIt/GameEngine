# ADR 0081: FBX Import via ufbx

Status: Accepted
Date: 2026-07-22

## Context

ADR 0078 drew the parser boundary: a format parser's only job is to
produce a normalized `ModelDocument`, and it explicitly deferred FBX as
"a major dependency decision (licensing, maintenance, compile time)
requiring its own ADR". This is that ADR.

The immediate driver is the Mixamo workflow: today an animated FBX must
be round-tripped through a DCC tool to glTF before the editor can
register it (`docs/FBX_IMPORT.md`). That round trip loses time, invites
export-settings mistakes, and is the single largest friction point in
the character pipeline.

FBX is a proprietary, undocumented format whose semantic layer (unit
scale, axis conventions, `PreRotation` / `PostRotation` /
`GeometricTransform` pivots, layered animation takes, embedded media) is
substantially harder than its container syntax. The dependency decision
is therefore dominated by who owns that semantic layer.

## Decision

### 1. Dependency: the `ufbx` crate (Rust bindings over the ufbx C library)

- **License**: MIT (both the bindings and the vendored C source). No
  Autodesk SDK, no proprietary code.
- **Why ufbx**: it is the de-facto standard open FBX parser (used by
  Blender's FBX tooling ecosystem, bevy_ufbx, many commercial engines'
  import tools). It parses binary and ASCII FBX across format versions,
  and — decisively — it owns the semantic layer: unit/axis conversion,
  pivot and `PreRotation` baking, animation evaluation, and embedded
  texture extraction are library features, not code we write and
  maintain.
- **Alternatives rejected** (see Alternatives Considered): pure-Rust
  `fbxcel` (tree-level only; the whole semantic layer would be ours),
  a hand-written parser (multi-thousand-line liability).
- **Cost accepted**: a C compilation step via `cc` in the build graph.
  ufbx is a single-file C library with no transitive C dependencies, so
  the surface is one translation unit compiled by the MSVC/clang/gcc
  toolchain already required for other native deps.

### 2. Target gating: desktop only, feature `fbx-import`

The engine crate gains a cargo feature `fbx-import`, default-enabled,
with the `ufbx` dependency declared under
`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` and the new
module gated `#[cfg(all(feature = "fbx-import", not(target_arch =
"wasm32")))]`. Rationale: compiling C to `wasm32-unknown-unknown` is not
supported by the plain `cc` flow (ADR 0041's build strategy), and FBX
import is an authoring-time operation — packaged content ships engine
formats, so the wasm player never needs the parser. The editor (desktop
only) always builds with the feature on.

### 3. Parser: `crates/engine/src/fbx_import.rs`, mirror of `gltf_import.rs`

New module owning every `ufbx` symbol, exactly as `gltf_import.rs` owns
every `gltf` crate symbol. Entry points mirror the glTF ones:

```rust
pub fn parse_fbx(bytes: &[u8]) -> Result<ModelDocument, FbxImportError>;
pub fn parse_fbx_path(path: &Path) -> Result<ModelDocument, FbxImportError>;
pub fn import_fbx_bytes(source: &AssetId, bytes: &[u8],
    existing_skeletons: &[SkeletonRecord]) -> Result<GltfImportResult, FbxImportError>;
pub fn import_fbx_path(source: &AssetId, path: &Path,
    existing_skeletons: &[SkeletonRecord]) -> Result<GltfImportResult, FbxImportError>;
pub fn import_fbx_path_with_contact_bones(...) -> ...; // parity with glTF
pub fn fingerprint_fbx_source(path: &Path) -> std::io::Result<String>;
pub fn fbx_source_dependencies(path: &Path) -> Vec<PathBuf>;
```

`import_fbx_*` = `parse_fbx` + `model_import::build_import_result`, the
same composition `import_gltf_bytes` uses. The result type stays
`GltfImportResult` (its name is a pre-existing misnomer the same way
`ModelDocument` docs already anticipate; renaming it is out of scope —
no serialized format contains it).

Normalization contract (module doc of `model_ir.rs`) is satisfied by
configuring ufbx's load options, not by post-processing:

- `target_unit_meters = 1.0`, `target_axes = right-handed +Y up` —
  ufbx bakes unit and axis conversion into node transforms.
- Geometry transforms / pivots: ufbx "geometry transform helpers" mode
  so no `GeometricTransform` residue survives; `PreRotation` /
  `PostRotation` are baked into the evaluated local TRS.
- Animation: one `IrClip` per FBX anim stack (take). Channels are
  resampled to linear keys at the stack's declared frame rate (30 Hz
  fallback) via ufbx's evaluation API — FBX curve interpolation modes
  never reach the IR, matching the existing "already linear-sampled"
  rule with a diagnostic per resampled curve
  (`anim.fbx_curve_resampled`, non-fatal).
- Skins: FBX clusters → `IrSkin` joints + inverse bind matrices.
  Clips bind to the skin whose joints their curves target, mirroring
  the glTF "first-resolved skin" rule; non-joint channels drop with the
  existing diagnostic pattern.
- Materials: FBX material → `IrMaterial` via ufbx's PBR material
  mapping (base color, normal, emissive, metallic/roughness when
  present; lambert/phong sources map through ufbx's PBR view).
- Textures: embedded media decode directly; file references resolve
  relative to the source document like glTF sidecars, and contribute to
  `fbx_source_dependencies` (fingerprint invalidation parity).

### 4. Format dispatch: `model_import::import_model_path`

Call sites stop naming a format. `model_import.rs` gains:

```rust
pub fn import_model_path(source: &AssetId, path: &Path,
    existing_skeletons: &[SkeletonRecord]) -> Result<GltfImportResult, ModelImportError>;
pub fn fingerprint_model_source(path: &Path) -> ...;
pub fn model_source_dependencies(path: &Path) -> Vec<PathBuf>;
```

dispatching on the lowercased extension (`gltf`/`glb` → glTF parser,
`fbx` → FBX parser, else `ModelImportError::UnsupportedExtension`).
Existing `import_gltf_*` entry points remain (tests and parity checks
use them); production call sites (editor `asset_import.rs`, `build.rs`,
`scene_bridge` glTF cache/asset load, `gltf_prefab.rs`) migrate to the
dispatching form in the same phase (breaking-change protocol). On a
wasm32 or `fbx-import`-disabled build the `fbx` arm returns
`UnsupportedExtension` with a diagnostic message naming the feature.

The editor's registerable-extension list (`asset_browser.rs`) adds
`.fbx` next to `.gltf`/`.glb`; sub-asset ID derivation, skeleton
identity/dedupe, retarget, contact detection, and packaging need no
changes — that is the entire point of ADR 0078.

### 5. Sub-asset ID stability

`imported_sub_asset_id` inputs stay (source AssetId, kind, original
selector). For FBX the original selector is the index in ufbx's stable
element order (`ufbx_scene` element lists), which is deterministic for
a given file. Re-importing the same FBX yields identical sub-asset IDs;
the same model exported as FBX vs glTF yields *different* selectors and
therefore different sub-asset IDs — switching a registered source's
format is a re-registration, not a reimport, and `docs/FBX_IMPORT.md`
documents that.

## Consequences

- Mixamo FBX (mesh + skin + clips + embedded textures) registers
  directly in the Asset Browser; the glTF conversion step becomes
  optional instead of mandatory.
- One C translation unit enters the build; compile time cost is paid
  once per clean build and only on desktop targets.
- The IR contract gets its first second consumer, which is the real
  test of ADR 0078; any IR gap FBX exposes must be fixed in the parser
  (bake it away) or via an additive IR field with an ADR note — never
  by leaking a format branch downstream.
- ufbx version upgrades are contained to `fbx_import.rs` by the same
  rule that contains `gltf` crate upgrades today.

## Alternatives Considered

- **`fbxcel` / `fbxcel-dom` (pure Rust)** — parses the container tree
  only; units, axes, pivots, pre-rotations, curve evaluation, embedded
  media would all be hand-written here. That is precisely the layer
  where FBX correctness bugs live, and the crates' maintenance activity
  is low. Rejected: maximum liability for minimum dependency win.
- **Hand-written parser** — thousands of lines against an undocumented
  format with two syntaxes and per-version quirks. Rejected outright.
- **Autodesk FBX SDK** — proprietary license incompatible with an MIT
  project, C++ toolchain burden. Rejected.
- **Keep conversion-only workflow** — status quo; rejected by the
  product requirement this ADR exists to serve, but it remains the
  documented fallback for exotic FBX features the importer drops.
