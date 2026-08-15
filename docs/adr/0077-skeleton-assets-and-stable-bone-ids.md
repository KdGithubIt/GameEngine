# ADR 0077: Skeleton Assets, Stable Bone IDs, and Clip Binding

Status: Accepted (the runtime `Skeleton` component shape in §2 is superseded
by ADR 0086)
Date: 2026-07-21

> ADR 0086 replaces the `Skeleton` component declared in §2: `joints` and
> `bone_ids` cover every bone of the skeleton asset rather than one skin's
> joints, and `inverse_bind_matrices` moves to `SkinnedMesh`. `BoneId`,
> `SkeletonAsset`, `SkeletonIdentity`, and clip binding are unchanged.

## Context

ADR 0043/0074 made the runtime `Skeleton` a shared component, but three
identity problems remain unsolved and they are the only parts of the
animation pipeline that cannot be corrected later without migrating every
asset:

1. **Clips bind bones by skin joint index.** `AnimChannel::target_joint:
   Option<usize>` refers to glTF skin joint order. Any reexport that reorders
   joints silently retargets every channel to the wrong bone.
2. **Skeletons have no persisted identity.** The `Skeleton` sub-asset ID
   exists (`imported_sub_asset_id(source, Skeleton, n)`) but nothing records
   which *bones* it contains, so nothing can survive a reimport that adds,
   removes, or renames bones.
3. **The same rig imported from two files becomes two skeletons.** A
   character file and a motion-only file exported from the same DCC rig
   differ only by float jitter. Without a dedupe rule, every motion file
   would demand a retarget map against its own character — the primary
   failure mode observed in other engines.

This ADR fixes bone identity, clip binding, and skeleton identity. It is the
prerequisite for ADR 0078 (model IR), ADR 0079 (retargeting), and ADR 0080
(contact metadata). Retargeting itself is out of scope here.

## Decision

### 1. `BoneId` — persisted, per-skeleton, never reused

```rust
/// Stable identity of one bone within one skeleton asset.
///
/// Allocated sequentially at first import, persisted in the manifest,
/// preserved across reimports, and never reused after a bone is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BoneId(pub u32);
```

`BoneId` is scoped to its skeleton asset; it is not globally unique and is
not a `StableId` (`<prefix>_<ULID>`). Rationale: bones are always addressed
through a skeleton reference, ULIDs would bloat every channel record, and
the existing `StableId` format must not change.

Bone names remain what they are in the source document. Names are used for
**first-import heuristics and human-facing diagnostics only**; every
persisted binding is a `BoneId`.

### 2. `SkeletonAsset` — a first-class runtime asset with rest pose

New type in `crates/engine/src/skeleton_asset.rs`, stored in
`Assets<SkeletonAsset>`:

```rust
pub struct SkeletonAsset {
    /// Imported sub-asset ID (existing derivation scheme, unchanged).
    pub id: AssetId,
    pub name: String,
    /// Parent-before-child order. Index 0 is a root.
    pub bones: Vec<BoneDef>,
    /// Canonical structure hash; see §4.
    pub identity: SkeletonIdentity,
    /// Next BoneId value to allocate; monotonic, never decremented.
    pub next_bone_id: u32,
}

pub struct BoneDef {
    pub id: BoneId,
    pub name: String,
    /// Index into `bones`; `None` marks a root.
    pub parent: Option<usize>,
    /// Rest (bind-time local) TRS captured from the source document.
    pub rest_translation: Vec3,
    pub rest_rotation: Quat,
    pub rest_scale: Vec3,
}
```

The runtime ECS `Skeleton` component (ADR 0074) gains parallel bone
identity so systems can resolve `BoneId` to a joint entity:

```rust
pub struct Skeleton {
    pub joints: Vec<Entity>,                 // unchanged, skin joint order
    pub inverse_bind_matrices: Vec<Mat4>,    // unchanged
    /// BoneId per joint, same order as `joints`.
    pub bone_ids: Vec<BoneId>,
    /// Skeleton asset these joints were spawned from.
    pub asset: Option<AssetId>,
}
```

`joint_palette_system` is unchanged (palette math never touches identity).

### 3. Clips bind by `BoneId` and always reference their skeleton

`AnimChannel::target_joint: Option<usize>` is **removed** and replaced:

```rust
pub struct AnimChannel {
    pub property: AnimProperty,
    /// Bone this channel drives, resolved through the animator's
    /// `Skeleton::bone_ids`. `None` drives the animator entity's own
    /// `Transform` (Phase 37 behavior, unchanged).
    pub target_bone: Option<BoneId>,
    pub keyframes: Vec<Keyframe>,
}

pub struct AnimationClip {
    pub duration: f32,
    pub channels: Vec<AnimChannel>,
    pub events: Vec<AnimEvent>,
    /// Skeleton this clip was sampled against. Required whenever any
    /// channel has a `target_bone`; enforced at import and by
    /// `AnimationClip::validate`.
    pub skeleton: Option<AssetId>,
    /// Identity of that skeleton at sampling time, so a stale binding is
    /// detectable instead of silently wrong.
    pub skeleton_identity: Option<SkeletonIdentity>,
    /// Bone whose translation carries locomotion; input to root-motion
    /// extraction and to translation policy in ADR 0079.
    pub root_bone: Option<BoneId>,
}
```

Invariant (checked, not assumed): a channel with `target_bone: Some(_)` in a
clip with `skeleton: None` is an import error / validation diagnostic
(`anim.clip_missing_skeleton`). Code-authored single-entity clips (all
targets `None`) remain valid without a skeleton, preserving Phase 37 usage.

`root_bone` is auto-detected at import as the topmost bone that has any
translation channel; the existing `RootMotionMode` machinery reads it
instead of guessing.

`AnimationClip` is a runtime asset rebuilt from sources (ADR 0043 §6), so
this is a workspace-wide source-breaking change but **not** a persisted
format change. All call sites (`animation.rs`, `anim_graph.rs`,
`gltf_import.rs`, examples, tests) are updated in the same PR per the
breaking-change protocol.

### 4. Skeleton identity and dedupe

```rust
/// Canonical structure hash of a skeleton: 64-bit FNV-1a over a canonical
/// byte stream of bone count, and per bone (in `bones` order): UTF-8 name,
/// parent index (u32, u32::MAX for roots), and rest TRS quantized to 1e-4
/// (rotation sign-canonicalized so w >= 0 before quantization).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SkeletonIdentity(pub u64);
```

Quantization exists so float jitter between exports of the same rig cannot
split identity; 1e-4 (0.1 mm at meter scale) is far below authoring
precision and far above export jitter.

**Dedupe rule at import:** after building a candidate skeleton from a
source, compute its identity. If any manifest entry already records a
skeleton with the same identity, the import **adopts** that skeleton's
`AssetId` and `BoneId` assignments (bone-by-bone match is by position —
identical identity implies identical order, names, and topology). Otherwise
a new skeleton asset is minted. Motion-only files therefore bind to the
character's skeleton automatically when the rig is the same.

The 64-bit hash is not cryptographic; a collision would wrongly unify two
rigs. With per-project skeleton counts in the dozens this is accepted for
v1 and recorded as a known limitation.

### 5. Manifest persistence — the bone catalog

Bone IDs must survive reimport, so their assignments are persisted in the
manifest, following the existing `ImportSettings::sub_assets` pattern:

```rust
// ImportSettings gains (additive, skip_serializing_if = Vec::is_empty):
pub skeleton_records: Vec<SkeletonRecord>,

#[derive(Serialize, Deserialize)]
pub struct SkeletonRecord {
    /// Sub-asset ID string of the skeleton this source binds to. May
    /// belong to a *different* source when dedupe adopted it (§4).
    pub id: String,
    pub identity: u64,
    pub next_bone_id: u32,
    /// name → BoneId assignments from the latest successful import.
    pub bones: Vec<SkeletonBoneRecord>, // { bone_id: u32, name: String }
}
```

Rest TRS is deliberately **not** persisted in the manifest: it is derivable
from the source file, and the manifest stays a reviewable identity ledger
rather than a data dump. Existing manifests parse unchanged
(`schema_version` does not change).

### 6. Reimport matching

On reimport of a source with an existing `SkeletonRecord`:

1. Same identity → same bones by construction; records carry over.
2. Different identity → match new bones to recorded ones **by exact name**.
   Matched bones keep their `BoneId`. Unmatched new bones receive fresh IDs
   from `next_bone_id`. Recorded bones absent from the new import are
   retired — their IDs are never reused.
3. The import result reports the match as a diagnostic:
   `anim.skeleton_rebind` with counts (kept / added / retired) and the
   names of retired and added bones, surfaced through the existing
   Problems panel. A rename therefore appears as one retired + one added
   bone; interactive rename reconciliation is editor UX deferred to AP-5
   (plan doc) and does not change this data model.

Clips whose `skeleton_identity` no longer matches the skeleton asset get a
`anim.clip_skeleton_stale` warning diagnostic; channels targeting retired
bones are skipped at bind time (never a panic), matching the existing
out-of-range behavior.

## Consequences

- Joint order, bone renames (via reimport matching), and bone
  additions/removals no longer silently corrupt clip bindings; failures
  become named diagnostics.
- Motion-only sources share the character's skeleton automatically when
  the rig is identical, which is the precondition for ADR 0079 asking for a
  retarget map *only* when rigs actually differ.
- `animation_system` resolves `BoneId → joint entity` through
  `Skeleton::bone_ids` (a per-skeleton `HashMap<BoneId, usize>` built once
  per system run, mirroring the existing palette map pattern).
- `crates/engine` (animation, skinning, gltf_import, asset), scene_bridge
  spawn paths, editor import code, and examples change together — breaking
  change protocol, one PR.
- The manifest grows a compact per-source bone ledger; diffs show bone
  additions/retirements explicitly, which is the observability §9 of the
  design review demands.

## Alternatives Considered

- **Bind by name/path** — rejected: renames and duplicate names corrupt
  bindings invisibly; names stay as heuristics and diagnostics only.
- **Global ULID per bone** — rejected: bones are never addressed without a
  skeleton in hand; ULIDs would bloat channels and manifest for no lookup
  benefit and would touch the frozen `StableId` conventions.
- **Persist rest pose in the manifest** — rejected: derivable data,
  manifest bloat, and a second source of truth that could drift from the
  source file.
- **Exact (non-quantized) identity hashing** — rejected: float jitter
  between exports of the same rig would split identity, recreating problem
  (3) of the Context.
- **Interactive reimport matching UI in this ADR** — deferred to AP-5: the
  data model (retire + allocate, never reuse) is UI-independent and the
  diagnostic already names what changed.

## Compatibility and Migration

- No persisted format version changes. `ImportSettings::skeleton_records`
  is additive with `skip_serializing_if`.
- `AnimationClip` / `AnimChannel` are runtime types; in-tree content
  (`examples/`, `assets/`) is regenerated by reimport. `spire_lite` and
  other code-authored clips compile against the new fields with
  `skeleton: None`, `target_bone: None`.
- `Vertex`, `StableId` format, scene/prefab schema versions: unchanged.
