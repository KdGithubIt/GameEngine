# ADR 0097: MMD (PMX/VMD) Import — Mesh, Baked Animation, Morphs, and Skin Splitting

Status: Accepted
Date: 2026-08-12

Amendment: ADR 0098 completes the deferred VMD-to-runtime morph channel,
defines section-based model/scene VMD routing, and adds ordered multi-VMD
composition through Animation Sets. The implementation-era statements below
that no morph channel is emitted are retained as historical context and are
superseded by ADR 0098.

Amendment: ADR 0112 replaces this ADR's PMX rigid-body/MMD-physics runtime
contract with best-effort conversion into engine-native Secondary Motion.
PMX mesh, skeleton, material, and morph import and the VMD-to-`AnimationClip`
bake remain in force.

## Context

ADR 0096 settled the physics-engine question (`rapier3d`) and drew one
boundary: an isolated MMD rigid-body/joint bridge for jiggle bones, with
every other MMD concern ("PMX/VMD parsing & IK integration, morph target
rendering, multi-skin splitting for `MAX_JOINTS`") explicitly deferred to
this ADR. This is that decision.

The goal, stated by the product owner: register a `.pmx` model and its
`.vmd` motions the same way a `.gltf`/`.fbx` is registered today
(`docs/USER_MANUAL_JA.md` §8.5–8.6), drag it into a scene, and get a
playable, animated character — mesh, skeleton, motion, and expression —
using the existing Asset Browser / Skinned Model / Animation Controller
workflow (ADR 0074, 0082–0087) rather than a bespoke MMD-only UI.

A concrete sample (`SakurabaEma_ByPOWER_v1_0.pmx`, a real booth.pm MMD
character) was parsed directly against the PMX 2.0 binary spec while
scoping this ADR, to ground every number below in one real, non-trivial
rig rather than a toy fixture:

| Property | Value |
| --- | --- |
| Vertices / triangles | 101,143 / 130,712 |
| Materials / textures | 13 / 9 |
| Bones | 376 (4 IK bones: L/R foot, L/R toe) |
| Bones with appended-parent (付与親) | 26 |
| Morphs | 85 (77 vertex, 3 material, 5 group) |
| Rigid bodies / joints | 306 / 333 |
| Bones driven by dynamic rigid bodies | 264 |

This single model already exercises every mechanism this ADR must cover:
IK, appended-parent, a bone count triple the render skin cap, vertex and
material morphs, and a heavy jiggle-physics rig (ADR 0096's problem, not
this one).

Four product decisions were made before drafting (recorded here so the
Alternatives section can refer to them):

1. VMD motion is **baked into `AnimationClip` at import time** (IK and
   appended-parent resolved to plain FK keyframes), not played through a
   parallel MMD-only runtime — so it drops into the existing Animation
   Set / Controller / Graph workflow unchanged.
2. Facial morphs are **in scope** for this ADR (not deferred).
3. Multi-skin splitting for `MAX_JOINTS = 128` is **designed in full
   here**, not deferred — jiggle-driven mesh regions (hair, skirt) must
   move on day one once ADR 0096's bridge is wired up.
4. Jiggle physics is **opt-in via a single marker component**, mirroring
   `FootIk`'s existing "present = active, absent = no-op" pattern
   (`foot_ik.rs`), with one required field (which rigid-body rig to use)
   and zero other configuration for the common case.

Explicitly out of scope, confirmed while narrowing the above: toon
shading, sphere-map (matcap) compositing, and edge/outline rendering
(`MaterialShadingModel` stays `Lit`/`Unlit`); VMD camera and light
tracks (this ADR is about a character, not a cinematic).

## Decision

### 1. Dependencies: `mmd-anim-format` + `mmd-anim-runtime`, authoring-time only

Both crates (alpha, active development, MIT-compatible per crates.io)
parse PMX/VMD and evaluate MMD's per-frame bone pipeline (FK → IK →
appended-parent). Unlike `rapier3d` (ADR 0096, needed at runtime on every
target for the jiggle bridge), **these two are import-time-only**: once
VMD motion is baked to `AnimationClip` (§3) and PMX geometry to `Mesh` +
`SkeletonAsset` + morph/rigid-body sub-assets (§2, §4, §5), nothing at
runtime ever calls into `mmd-anim-format`/`mmd-anim-runtime` again. They
are gated exactly like `ufbx` (ADR 0081 §2): a `mmd-import` feature,
default-enabled, declared under
`[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`, because
import is an editor/authoring operation and packaged content — including
wasm32 exports — ships only engine-native formats. `mmd-anim-physics-
bullet` is not a dependency of this engine at all (ADR 0096 §1).

### 2. PMX becomes a third `ModelFormat` arm in `model_import.rs`

New module `crates/engine/src/pmx_import.rs`, structured like
`fbx_import.rs`: it owns every `mmd-anim-format` symbol and produces a
`ModelDocument` (ADR 0078). `detect_model_format` gains `"pmx" =>
ModelFormat::Pmx`; `import_model_path`/`import_model_bytes` dispatch to it
exactly like the FBX arm.

Normalization contract (`model_ir.rs`'s module doc) is satisfied the same
way `fbx_import.rs` satisfies it — at parse time, not by later
post-processing:

- **Units/axes**: PMX is already meters, right-handed, and MMD's own
  convention is left-handed +Y up; the parser mirrors X, matching the
  same axis-conversion obligation `fbx_import.rs` already discharges.
- **Bones → `IrNode`**: one `IrNode` per PMX bone, in PMX order (already
  parent-before-child in every real PMX file; the importer sorts
  defensively if not). `IrNode::translation/rotation/scale` are the
  bone's rest-pose local TRS.
- **IK and appended-parent are *not* representable as plain `IrNode` TRS**
  — they are per-frame runtime constraints, not rest-pose data — so they
  never reach `ModelDocument` at all. They exist only inside
  `mmd-anim-runtime`'s evaluator, consumed exclusively by §3's bake step.
  This keeps `model_ir.rs`'s "no format-specific residue" rule intact
  without extending it: IK/appended-parent are resolved *before* anything
  becomes IR, the same way `fbx_import.rs` bakes away `PreRotation`
  before anything becomes IR.
- **Skin**: PMX has exactly one implicit "skin" (every bone with
  non-zero vertex weight). It is emitted as a single `IrSkin` covering
  all 376 bones of the sample model — deliberately not yet split; §4
  performs the split as a `model_import.rs`-level concern shared by any
  future format that needs it, not a PMX parser concern.
- **Materials**: PMX material → `IrMaterial`, `shading_model:
  MaterialShadingModel::Lit` always (no toon/sphere-map representation
  exists to map to — this is where the "toon shading out of scope"
  decision is enforced, not silently dropped: a
  `pmx.toon_shading_unsupported` diagnostic is recorded per material that
  declared a toon reference or sphere-map mode, mirroring the existing
  `gltf.morph_targets_unsupported` diagnostic pattern
  (`gltf_import.rs:448`) that already tells authors when an import drops
  a feature).
- **Textures**: PMX texture paths resolve relative to the source file,
  contributing to `pmx_source_dependencies` for fingerprint invalidation
  parity with glTF/FBX sidecars.

### 3. VMD is a clip-only source, baked through `mmd-anim-runtime`'s evaluator

VMD is not a model — it carries no mesh/material/texture, only bone
curves, morph curves, and (ignored per this ADR) camera/light curves. It
gets its own entry point, not `model_import.rs`'s `ModelDocument`:

```rust
// crates/engine/src/vmd_import.rs
pub fn import_vmd_path(
    source_id: &AssetId,
    path: &Path,
    rig: &VmdBakeRig,               // the PMX rig + the skeleton to bind against
    options: &VmdBakeOptions,       // sample rate, contact-bone override
) -> Result<VmdImportResult, VmdImportError>;
```

**Amended during implementation.** This signature was first drafted as
`(source_id, path, skeleton: &SkeletonAsset, morphs: &MorphCatalog)`, which
cannot work: a `SkeletonAsset` carries bone names, parents, and rest TRS, but
none of the IK chains, appended-parent links, fixed/local axes, or bone
morphs the evaluator in step 3 exists to apply. Those live only in the PMX,
so the bake needs the model, not just the rig's shape. `VmdBakeRig` is that
missing input: built once from the same `.pmx` bytes §2 imported plus the
`SkeletonAsset` that import produced, then reused for every `.vmd` baked
against that model — a character with twenty motions parses its PMX once, not
twenty times. It also owns the PMX-bone-to-`BoneId` binding (by name, per
step 2 below) and reports `vmd.rest_pose_mismatch` when the skeleton it was
handed does not describe the same rest pose as the rig, which is the only
signal that a motion is being baked against the wrong model.

`MorphCatalog` is deferred with the rest of §5 rather than being passed as an
empty placeholder: PMX morph *names* are bound (bone morphs genuinely move
bones, so ignoring them would change the baked pose), but no morph channel is
emitted, and a motion carrying morph curves reports
`vmd.morph_channels_unsupported` so the gap is visible rather than silent.
Adding the catalog parameter is additive when §5 lands.

`VmdImportResult` carries one or more `AnimationClip`s (VMD files can
contain multiple named motions in later revisions; the common case is
one clip per file) built by:

1. Loading the VMD's raw bone/morph curves via `mmd-anim-format`.
2. Resolving VMD bone names against `skeleton`'s bone names. **MMD's
   bone-naming convention is a de facto standard across the ecosystem**
   (foot IK bones are always named `左足ＩＫ`/`右足ＩＫ` etc., as the
   sample model confirms) — this is why a VMD authored against a
   different, unrelated MMD character frequently still plays back
   correctly on this one, with no retarget map (ADR 0079) required for
   the common case. A VMD bone name absent from `skeleton` drops that
   bone's curve with a `vmd.bone_not_found` diagnostic (matches the
   existing "drop with diagnostic" convention used throughout
   `gltf_import.rs`/`fbx_import.rs`), rather than failing the whole
   import.
3. Running `mmd-anim-runtime`'s per-frame evaluator across the clip's
   full duration at a fixed sample rate (default 30 Hz, matching MMD's
   native frame rate; configurable per ADR 0081 §3's FBX resample
   precedent), which resolves FK + IK + appended-parent into a single
   flat local-TRS-per-bone-per-frame result — i.e. it *dissolves* IK and
   appended-parent, the same way `fbx_import.rs` dissolves `PreRotation`.
4. Emitting one `AnimChannel` (`Translation`/`Rotation`) per bone with
   non-static curves, `target_bone` resolved via `skeleton`'s `BoneId`s
   exactly like `model_import.rs::build_animations` already does for
   glTF/FBX. A `vmd.curve_resampled` diagnostic records the bake,
   mirroring `anim.fbx_curve_resampled` (ADR 0081 §3).
5. Emitting one new channel kind per morph curve: `AnimChannel::morph`
   (a sibling list on `AnimationClip`, not a variant of the existing
   bone-targeted `AnimChannel`, since a morph channel has no `BoneId`):

   ```rust
   pub struct MorphChannel {
       pub target_morph: MorphId,       // resolves via §5's MorphCatalog
       pub keyframes: Vec<Keyframe>,    // weight in [0, 1] per frame
   }
   ```

Because baking is deterministic and pure over (VMD bytes, skeleton,
morph catalog), it reuses ADR 0079's derived-cache machinery
(`resolve_or_bake_retargeted_clip`'s pattern) so re-registering the same
VMD against the same PMX is free after the first bake.

**Consequence of baking**: the resulting `AnimationClip` is a normal
sub-asset. It is assignable to an Animation Set's Motion Slot, plays
through the Animation Graph/Controller, cross-fades, fires Animation
Events, and drives Root Motion — every existing animation feature — with
*zero* MMD-specific code anywhere downstream of import. This is the
entire point of decision 1.

#### 3a. A motion is a new source kind, paired with its model in the manifest

Registering a `.vmd` is not quite "the same flow as glTF/FBX" the way §2's
`.pmx` is, and pretending otherwise would hide a real difference: a motion
is the first **animation-only source** in the pipeline. It produces no mesh,
no material, no skeleton, and no placement prefab (ADR 0074) — only
`Animation` sub-assets — and, uniquely, it cannot be imported from its own
file at all, because the rig it needs lives in a second file.

`AssetKind` therefore gains `MotionSource` (`*.vmd`) beside the existing
`GltfSource` (`*.gltf`/`*.glb`/`*.fbx`/`*.pmx`). Routing on the source's own
kind — rather than adding `.vmd` to `GltfSource` and branching inside the
model importer — is what keeps `.vmd` out of every mesh/skin/prefab-producing
path that cannot apply to it. `AssetKind::AnimationClip` also accepts `.vmd`,
so a clip reference may name the file exactly as it may name a `.gltf`.

The pairing is recorded as `ImportSettings::motion_model_source`, the
registered PMX's `AssetId`:

- **Explicit, not inferred.** Directory layout and the VMD header's
  target-model name are both conventions MMD authors routinely break, and a
  wrong pairing produces a plausible-looking, wrong clip rather than a
  failure — §3's `vmd.rest_pose_mismatch` is a warning, not a hard stop. On
  registration the editor auto-pairs only when the project holds exactly one
  `.pmx`; anything ambiguous is left unpaired and reported
  (`asset.motion_source_unpaired`, `scene.motion_source_unpaired`) for the
  author to resolve in Import Settings.
- **A source dependency.** The paired `.pmx` path lands in
  `ImportSettings::source_dependencies`, and the motion's fingerprint
  (`fingerprint_motion_source`) hashes both files, so editing the model
  invalidates every motion baked against it. The editor requeues those
  motions automatically when the model finishes reimporting.
- **Additive.** `motion_model_source` is `Option<String>` with
  `skip_serializing_if`, so every existing `asset_manifest.json` stays
  byte-identical.

Baked clips bind the `BoneId`s the *model's* import assigned, so the motion's
import runs the model import first (or, in the runtime scene bridge, reuses
the cached one) rather than deriving a second skeleton identity from the
motion's own source ID.

### 4. Multi-skin splitting reuses ADR 0086/0087 unchanged — no new runtime mechanism

ADR 0086 already separated the rig (`Skeleton`, unbounded bone count)
from the skin binding (`SkinnedMesh`, ≤128 joints), specifically so "one
rig serves any number of skins" and ADR 0087 already places body,
clothes, and hair as **separate render parts over one joint hierarchy**.
Splitting a 376-bone PMX for rendering is therefore not a new runtime
concept — it is an **importer-side partitioning algorithm** that emits
several `IrMesh`/`IrSkin` pairs (one per render part) against the single
376-bone rig from §2, each satisfying `MAX_JOINTS`:

1. Start from PMX materials as the natural partition seed — the sample
   model's 13 materials already correspond to visually distinct parts
   (face, body, hair, two clothing layers, shoes, ...).
   `mmd-anim-format` supplies this step directly via
   `split_pmx_model_by_material`, which also remaps each material's
   morph offsets to the split mesh's local vertex indices
   (`PmxMaterialSplitMesh::morph_index_map`), so §5's morph data
   survives the split without separate bookkeeping.
2. For each material's vertex range, compute the set of distinct bones
   with non-zero weight across those vertices.
3. If a material's own bone set is ≤128, it becomes one `IrSkin`
   directly. Otherwise the material's vertex range is further split by
   spatial locality (bone-chain grouping) until every resulting group is
   ≤128.

   **This sub-split is required, not a rare fallback.** Measured
   per-material distinct bone counts on the sample model:

   | Material | Tris | Distinct bones |
   | --- | ---: | ---: |
   | `Mt_SakurabaEma_Hair` | 10,637 | **143** |
   | `Mt_SakurabaEma_Clothes01` | 10,793 | 114 |
   | `Mt_SakurabaEma_Clothes01_Stencil` | 3,029 | 79 |
   | `Mt_SakurabaEma_Clothes02` | 72,104 | 52 |
   | `Mt_SakurabaEma_Body` | 7,670 | 44 |
   | remaining 8 materials | — | ≤ 8 each |

   12 of 13 materials fit under the cap; hair exceeds it at 143 because
   that is where jiggle-physics bones concentrate (264 of the model's
   376 bones are physics-driven). Note that bone count does not track
   triangle count — the largest material by far (72,104 tris) needs only
   52 bones. An importer that implements step 1–2 only would drop the
   hair skin with the existing `MAX_JOINTS` diagnostic and render a bald
   character, so step 3 must ship in the same change.
4. Each group becomes one `GltfMeshData`/`GltfSkinData` pair through the
   existing `build_import_result` machinery (`model_import.rs`), which
   already assigns stable sub-asset IDs per `source_index` — a PMX
   material index here plays the same role a glTF primitive index plays
   today, so ADR 0074/0077's dedupe/reimport-rebind rules apply
   unchanged.
5. At scene instantiation, each group's render part becomes one entity
   with `engine.skinned_mesh_renderer.model` pointing at the one shared
   `engine.skinned_model` (ADR 0087 §2–§4) — identical to how today's
   FBX/glTF importer already emits one renderer per surviving mesh
   against one shared model.

This step lives in `model_import.rs`, not `pmx_import.rs`, because
nothing about it is PMX-specific: any future format whose natural skin
exceeds 128 joints gets it for free, matching ADR 0078's stated purpose.

#### 4a. Skins must be able to declare a document-wide shared skeleton

Step 5's "one shared `engine.skinned_model`" does **not** fall out of the
existing importer, and this was missed when §4 was first drafted.
`model_import.rs::build_skins` derives a *separate* `SkeletonAsset` per
skin, from `build_skeleton_bones(nodes, &ir_skin.joint_nodes)` — that
skin's joints plus their ancestors — and keys its ID on
`ir_skin.source_index`. That is correct for glTF, where a skin's joint
list *is* its skeleton. Applied to a split PMX it produces one skeleton
per render part (~14 for the sample model), each covering a different
bone subset: the duplicated joint hierarchies ADR 0074/0086 exist to
prevent, ~14 Skinned Model entities instead of one, and — fatally for
§3 — no single skeleton for a baked VMD clip to target.

`ModelDocument` therefore gains an explicit, format-independent
declaration of how its skins relate to skeletons:

```rust
pub enum SkeletonScope {
    /// Each skin defines its own skeleton from its joints and their
    /// ancestors. glTF/FBX behavior, unchanged, and the default.
    #[default]
    PerSkin,
    /// Every skin binds to one shared skeleton spanning exactly
    /// `skeleton_nodes` (and their ancestors), as ADR 0086 §4 permits: the
    /// skeleton may exceed `MAX_JOINTS` so long as no single skin does.
    SharedAcrossDocument {
        /// Indices into `ModelDocument::nodes` that form the shared rig.
        ///
        /// Nodes outside this list — mesh anchors, scene-graph scaffolding —
        /// are deliberately excluded so the skeleton's identity (ADR 0077)
        /// depends only on the rig itself and never on how the document's
        /// meshes happen to be partitioned.
        skeleton_nodes: Vec<usize>,
    },
}
```

This is a general IR concept, not PMX residue — "do these skins share a
rig?" is a question any format can answer, and a future glTF with several
skins over one armature could opt in the same way — so it satisfies ADR
0078's rule that the IR never encodes format-specific structure.

`skeleton_nodes` is carried explicitly rather than inferred (e.g. by
assuming every node without a mesh is a bone), and deliberately excludes
nodes that are not part of the rig. This matters concretely for PMX: the
multi-skin split (§4) attaches each render part's `mesh`/`skin` indices to
its own placeholder `IrNode`, appended to `ModelDocument::nodes` after the
PMX bones. If those placeholder nodes were included in the shared skeleton,
`SkeletonIdentity` (ADR 0077 §4) would depend on how many render parts the
split happened to produce rather than on the PMX rig itself — so tuning the
split heuristic, or editing a model just enough to change a split part
count, would spuriously trip ADR 0077's rebind path even though the rig
never changed. `pmx_import.rs` therefore sets `skeleton_nodes` to exactly
`0..bone_count` (the PMX bone range, captured before the placeholder nodes
are appended), which also keeps IK bones — driven by no skin's vertex
weights but targeted by baked VMD motion (§3) — in the shared skeleton,
since they are still PMX bones.

Under `SharedAcrossDocument`, `build_skins` builds the skeleton **once**
from `skeleton_nodes` (reusing `build_skeleton_bones`, so its ancestor
-closure and parent-before-child ordering logic is not duplicated),
resolves its identity/dedupe/rebind once (unchanged `resolve_skeleton_ids`
logic, ADR 0077), and every skin's `joint_bone_ids` resolves its own
`joint_nodes` through that one skeleton's node→bone map. The shared
skeleton takes selector `0`, so its `imported_sub_asset_id` is stable.
`GltfImportResult::imported_sub_assets` emits the shared Skeleton exactly
once rather than once per skin, and `skeleton_records` likewise carries one
entry, not one duplicate per skin.

### 5. Morph targets: a new `ImportedSubAssetKind::Morph`, sparse CPU blending in v1

**IR**: `IrMesh` gains `pub morph_targets: Vec<IrMorphTarget>`, where

```rust
pub struct IrMorphTarget {
    pub source_index: usize,
    pub name: String,
    pub kind: IrMorphKind,           // VertexPosition | Material
    pub vertex_deltas: Vec<(u32, Vec3)>, // sparse: (vertex_index, position delta)
}
```

Group morphs (PMX kind 0, weighted combinations of other morphs) are
**flattened at parse time**: a group morph's weight curve expands to
weighted contributions on its underlying vertex/material morphs before
anything reaches the IR, so no separate runtime "group" concept exists —
this mirrors how `fbx_import.rs` already resolves multi-take animation
into independent clips rather than carrying FBX's take structure forward.

This IR addition is **not PMX-specific**: `gltf_import.rs` already
detects and silently drops glTF morph targets today
(`gltf.morph_targets_unsupported`, `gltf_import.rs:448`). This ADR's
runtime morph-weight machinery (below) is written once, and a follow-up
change (out of scope here) can delete that diagnostic and wire glTF
morph targets through the same path — ADR 0078's format-independence
promise paying off exactly as designed.

**Sub-asset catalog**: `ImportedSubAssetKind` gains `Morph`. Import
produces one `Morph` sub-asset per surviving `IrMorphTarget`, with stable
IDs via the existing `imported_sub_asset_id` formula. `MorphCatalog`
(§3's `import_vmd_path` parameter) is this per-source list of `(name,
AssetId)`, used to resolve VMD morph-curve names.

**Runtime — v1 is CPU-side, scoped to each morph's own sparse vertex
set, not full-mesh GPU blending**: A `MorphWeights` component (parallel
to `Skeleton`/`SkinnedMesh`) holds `HashMap<MorphId, f32>`, written by
`AnimationClip`'s new `MorphChannel`s (§3) exactly like `AnimChannel`
already drives bone transforms. A new `morph_blend_system` runs before
`joint_palette_system` each frame: for every mesh with non-zero morph
weights, it recomputes only the vertices any active morph actually
touches (the sample model's face-only materials are ~12,345 of 130,712
triangles — under 10% of the mesh — so this is a small, bounded working
set even at 77 morphs) and re-uploads that vertex sub-range. Skinning
then applies to the morphed positions unchanged, so morph and skin
compose correctly without a shader change.

A full GPU vertex-shader morph blend (parallel delta buffers, weights in
a storage buffer, composed with the existing bone-palette skin pass in
one shader stage) is documented here as the natural future upgrade if
profiling shows the CPU path is too slow for a given scene, but is
explicitly **not** built in v1 — the sparse CPU path is simpler to get
correct first and is where "start simple, measure, then optimize"
judgment applies. This is the one place this ADR commits to a v1 scope
narrower than "fully solved," and is called out accordingly.

**Amended during implementation — a morphing entity owns its mesh.** The
"re-upload that vertex sub-range" sketch above is wrong as written for any
scene with two instances of the same character: meshes are shared, so every
entity referencing a mesh asset draws from one `GpuMesh` in `GpuMeshCache`,
while morph weights are per-entity. Blending into that shared buffer would
make two copies of a character wear each other's expression. A morphing
renderer therefore carries a **private `Mesh` component** instead of a
`Handle<Mesh>` — a path the renderer already supports ("direct `GpuMesh`
entities", used by both the static and skinned draw collectors) — and pays
one mesh copy per morphing entity. That is the unavoidable cost of
per-instance deformation in any CPU scheme, and it disappears entirely if
the GPU path above is ever built, since weights then live per-draw rather
than in the vertex buffer.

**Amended — morph sub-asset selectors are `(mesh, morph)` pairs.** A source's
morph index is unique only *within* a mesh, and the multi-skin split (§4)
puts one PMX morph on every render part it touches, so deriving IDs from the
morph index alone would collide. The selector packs the owning mesh's
selector with the morph's own, which stays a pure function of the source and
keeps each part's copy separately assignable.

**Material morphs**: a much smaller mechanism — a
`MaterialMorphWeights` side list resolved by a small system that
overrides `MaterialAsset` color/alpha fields per frame, not routed
through the vertex pipeline at all.

### 6. Jiggle physics: opt-in `RigidBodyRig` sub-asset + one marker component

PMX rigid-body/joint definitions (shape, bone-local offset, mass,
damping, mode; joint 6-DOF spring limits — the data ADR 0096 §5's bridge
consumes) are captured at import time into a new engine-native,
serializable sub-asset:

```rust
pub struct RigidBodyRigAsset {
    pub id: AssetId,
    pub bodies: Vec<RigidBodyDef>,   // shape, bone offset, mass, damping, mode
    pub joints: Vec<JointDef>,       // body pair, 6-DOF spring limits
}
```

This is what makes ADR 0096 §1's authoring/runtime split work: the
*asset* is produced by the desktop-only, `mmd-anim-format`-dependent
importer, but the *asset file* is engine-native JSON like any other
sub-asset — so ADR 0096's rapier-backed bridge reads it on every target,
including wasm32, without ever linking `mmd-anim-format` there.

Activation mirrors `FootIk`'s existing "present = active" pattern
(`foot_ik.rs`):

```rust
pub struct RigidBodyPhysics {
    pub rig: AssetRef,  // -> RigidBodyRig sub-asset; required, no other fields
}
```

Adding this one component to the Skinned Model entity is the entire
end-user action; ADR 0096's bridge system is a no-op for any entity
without it, and needs no per-body/per-joint authoring — the physics
designer already tuned the PMX in their own tool. This directly answers
the "how much end-user effort" question raised while scoping this ADR:
one component, one reference field, zero further configuration for the
common case.

**Scope note.** §6 covers the *asset and the intent* only: capturing the rig
at import, publishing it as an engine-native sub-asset, and recording which
entity wants it simulated. Nothing here simulates — the solver is ADR 0096's
rapier-backed bridge, which this section deliberately lands ahead of so the
data format is stable before a backend depends on it. Until that bridge
exists, `engine.rigid_body_physics` is inert rather than wrong, exactly like
`engine.foot_ik` on a character with no foot bones.

**Convention chosen without an in-tree reference.** PMX stores a body's and a
joint's orientation as three Euler angles and does not name a composition
order; the importer builds them as yaw-pitch-roll (`EulerRot::YXZ`), which is
what mainstream MMD loaders reproduce. This is the one place in the MMD
importer where a convention was picked with no reference implementation in
this repository to validate against, so ADR 0096's bridge should confirm it
against a real jiggle rig before relying on the exact orientation of a
non-axis-aligned body.

## Consequences

- A `.pmx` + one or more `.vmd` files register through the exact same
  Asset Browser flow as glTF/FBX (`docs/USER_MANUAL_JA.md` §8.5–8.6):
  Register Asset → drag to Scene View → Create Animation Set → assign
  Motion Slots → assign Controller. No new top-level UI is introduced.
- IK and appended-parent become invisible after import: they are baked
  FK curves indistinguishable from any other `AnimationClip`, so
  blending, cross-fade, retargeting (ADR 0079), and Animation Events all
  work on MMD-sourced clips for free.
- A bug in `mmd-anim-runtime`'s IK/appended-parent evaluation is baked
  permanently into the clip; fixing it requires reimporting the VMD, not
  a runtime patch. Accepted per the product decision to bake (§3), same
  trade-off ADR 0081 already accepted for FBX curve resampling.
- `mmd-anim-format`/`mmd-anim-runtime` alpha-stage risk (breaking changes
  before 1.0, per ADR 0096's context) is now contained to two new
  modules (`pmx_import.rs`, `vmd_import.rs`), matching how `gltf`/`ufbx`
  upgrades are already contained (ADR 0081's consequences).
- Morph target IR support is reusable by glTF once this ADR lands;
  wiring `gltf_import.rs`'s existing (currently dropped) morph data
  through the same runtime path is a small follow-up, not part of this
  ADR.
- Materials needing the spatial sub-split (§4 step 3) add import time
  proportional to that material's vertex count. The sample model needs
  it for exactly one material (hair, 143 bones), so the cost is real but
  small; models with heavier jiggle rigs will hit it more often.
- Toon shading, sphere maps, and edge/outline remain absent; imported
  PMX materials render flat-lit. A per-material diagnostic tells authors
  which visual features were dropped, rather than silently changing
  appearance.

## Alternatives Considered

- **Play VMD through a separate MMD-only runtime pose evaluator**
  (`mmd-anim-runtime` called every frame instead of baked at import) —
  rejected per the product decision recorded in Context: it would make
  an MMD character's animation authoring permanently disconnected from
  Animation Set/Controller/Graph, cross-fade, Animation Events, and
  retargeting — every tool built for every other imported format. The
  only advantage (less import-time engineering; runtime IK bugs are
  reimport-free) does not outweigh that isolation for a feature whose
  entire premise is "use MMD models the same way I use everything else."
- **Skip morph targets in this ADR, revisit later** — was the initial
  recommendation while scoping (rendering pipeline work with no existing
  precedent in this codebase); overridden by explicit product decision,
  since a MMD character's face is a large part of what "using an MMD
  model" means for a character-focused import.
- **Cap PMX bone count at import, drop excess bones with a diagnostic**
  (matching `MAX_JOINTS` skin-drop precedent in `gltf_import.rs`/
  `fbx_import.rs`) instead of multi-skin splitting — rejected per product
  decision: dropping physics-driven bones from the render skin would
  leave hair/skirt permanently rigid even with ADR 0096's bridge fully
  wired up, defeating the reason jiggle physics was adopted at all.
  Multi-skin splitting costs more importer engineering but the runtime
  mechanism (ADR 0086/0087) already exists, so the marginal cost is
  contained to §4's partitioning algorithm.
- **Full GPU vertex-shader morph blending in v1** — the architecturally
  "complete" answer, but a shader-pipeline change with no existing
  precedent (skinning's vertex shader would need a second blend stage)
  against a face-only workload that the scoped CPU path already handles
  at the sample model's scale. Left as documented future work (§5),
  revisited if profiling on a real scene demands it.
- **One shared `RigidBodyPhysics` config surface instead of a `FootIk`-
  style marker component** (e.g. exposing per-body mass/damping in the
  Inspector) — rejected: the PMX author already tuned every rigid body
  and joint in their own tool; re-exposing that as editable engine state
  duplicates authoring surfaces for no user-visible benefit and directly
  contradicts the "one component, zero configuration" answer given while
  scoping this ADR.

## Compatibility and Migration

- No changes to existing persisted formats: `Skeleton`, `SkinnedMesh`,
  `AnimationClip`, `MaterialAsset`, scene/prefab schemas are additive
  only (`MorphChannel` list on `AnimationClip`, `Morph` sub-asset kind,
  new `RigidBodyRig` sub-asset kind, new `RigidBodyPhysics` component) —
  every existing glTF/FBX-imported project loads and renders unchanged.
- `asset_manifest.json` gains one optional field,
  `ImportSettings::motion_model_source` (§3a), written only for `*.vmd`
  entries and skipped when absent, so an existing manifest round-trips
  byte-identically and an older build ignores it.
- `AssetKind` gains `MotionSource`. Existing variants and every path that
  matches on them are untouched; only `AnimationClip`'s accepted extension
  list widens (by `.vmd`), which cannot reclassify any existing file.
- `ModelDocument::skeleton_scope` (§4a) defaults to `PerSkin`, which is
  exactly today's behavior, so glTF and FBX imports produce byte-identical
  results and identical sub-asset IDs. `ModelDocument` is an in-memory IR
  with no serialized form, so this addition changes no file on disk.
- `ImportedSubAssetKind` gains `Morph` and `RigidBodyRig` variants;
  existing variants and their ID derivation are untouched, so existing
  sub-asset IDs are stable across this change.
- `model_import.rs`'s `ModelFormat` enum gains `Pmx`; `ModelImportError`
  gains a `Pmx(PmxImportError)` variant following the exact pattern
  `Fbx(FbxImportError)` already established in ADR 0081 — no changes to
  existing `Gltf`/`Fbx` arms.
- `vmd_import.rs` is new, additive, and has no legacy surface to migrate.
- Packaged/wasm32 builds are unaffected: `mmd-import` (like `fbx-import`)
  is a desktop-only, authoring-time feature; shipped content is baked
  `AnimationClip`/`Mesh`/`Morph`/`RigidBodyRig` data, identical in kind to
  what glTF/FBX already produce.
