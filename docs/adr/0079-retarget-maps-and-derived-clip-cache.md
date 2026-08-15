# ADR 0079: Retarget Maps and the Derived Clip Cache

Status: Accepted (§4 packaging scope note revised by AP-7, 2026-07-22)
Date: 2026-07-21

> AP-7 (2026-07-22) closes the gap between this ADR's original §4 packaging
> intent and the bake walk `crates/editor/src/build.rs` actually implemented
> from AP-3 through AP-6, which baked every registered `*.retarget.json` map
> regardless of whether any content reached it. The bake walk now traces
> reachable `(source_skeleton, target_skeleton)` pairs from scene/prefab
> content, as this section always described, and `RetargetMap` gains an
> `always_package` escape hatch for maps backing clips assigned dynamically
> at runtime, which the static trace cannot see. See
> `docs/ANIMATION_PIPELINE_PLAN.md` AP-7 for the full policy.

## Context

With ADR 0077, a clip knows its skeleton and rig-identical sources share
one skeleton asset. What remains is playing a clip authored for skeleton A
on a character with skeleton B. Requirements fixed by the design review:

- The mapping must be a **reviewable asset**, not a hidden import option.
- Retargeting must be a **pure function** whose results are baked into a
  content-addressed cache: zero per-frame runtime cost, no N-clips ×
  M-characters asset explosion, and the same function callable at runtime
  later if dynamic retargeting is ever needed.
- Chains (spine, limbs, tail, …) are the mapping unit — bone counts may
  differ between rigs; a fixed humanoid enum is explicitly rejected.
- Ground-contact **correction** is out of scope here (runtime concern,
  ADR 0080); this ADR only produces FK-retargeted clips.

## Decision

### 1. `RetargetMap` — a persisted asset (`*.retarget.json`)

New authoring-side persisted format, `schema_version: 1`, editable JSON,
registered in the manifest like other assets:

```rust
pub struct RetargetMap {
    pub schema_version: u32,
    pub source_skeleton: AssetId,
    pub source_identity: u64,     // SkeletonIdentity at authoring time
    pub target_skeleton: AssetId,
    pub target_identity: u64,
    /// Direct 1:1 mappings (fingers, weapon bones, single bones).
    pub bone_pairs: Vec<BonePair>,           // { source: u32, target: u32 } BoneIds
    /// Ordered chains; the redistribution unit when counts differ.
    pub chain_pairs: Vec<ChainPair>,
    pub translation: TranslationPolicy,
}

pub struct ChainPair {
    /// Diagnostic label ("spine", "left_leg", ...). Not semantic.
    pub name: String,
    pub source_bones: Vec<u32>,   // root-to-tip BoneIds, contiguous chain
    pub target_bones: Vec<u32>,
}

pub struct TranslationPolicy {
    /// Which bones carry translation keys through retarget.
    pub mode: TranslationMode,    // RootOnly (default) | None | All
    /// Scale applied to carried translations.
    pub scale: TranslationScale,  // HipHeightRatio (default) | Manual(f32)
}
```

Stale identity (map's recorded identity ≠ current skeleton asset identity)
is a validation diagnostic `anim.retarget_map_stale`, not a silent
best-effort application.

Creation flow (editor): assigning a clip whose skeleton differs from the
target's prompts map creation; a name-heuristic pass (case-insensitive
name match, then suffix/prefix-stripped match) pre-fills `bone_pairs`, and
unresolved bones are listed for the user. The heuristic runs **once at
authoring time**; the asset stores only explicit results.

### 2. The retarget function is pure and deterministic

```rust
/// Pure: output depends only on arguments. No I/O, no globals, no time.
pub fn retarget_clip(
    clip: &AnimationClip,
    source: &SkeletonAsset,
    target: &SkeletonAsset,
    map: &RetargetMap,
) -> Result<AnimationClip, RetargetError>
```

Math (per output keyframe time, per mapped bone):

1. Sample source local TRS, FK to **model space**.
2. Rotation transfer: `q_dst_model = q_src_model * inverse(q_src_rest_model)
   * q_dst_rest_model` — the source's model-space delta from its own rest,
   applied onto the target's rest orientation. This is what makes the
   result independent of rest-pose convention (T-pose vs A-pose) and of
   per-bone axis conventions, the two couplings the design review names.
3. Convert back to target local space against the target parent's already
   -retargeted model-space rotation.
4. Translation: only bones selected by `TranslationPolicy` keep
   translation channels; values scale by the policy
   (`HipHeightRatio` = target rest hip height / source rest hip height,
   where "hip" = the clip's `root_bone` mapping). All other target bones
   take their rest translation (bone lengths are the target's own —
   proportions are never squashed).
5. Chain pairs with equal counts map bone-by-bone in order. Unequal
   counts map each target chain bone to the source bone at the nearest
   normalized position along the chain (`i / (len-1)`), with the
   documented v1 limitation that curvature is transferred stepwise rather
   than redistributed smoothly; smooth spline redistribution is a future
   upgrade inside the same function signature (bumps
   `RETARGET_ALGORITHM_VERSION`).

Key times: the output uses the union of the source clip's key times per
mapped bone, capped at 4096 keys per channel (beyond that, uniform
resampling at 60 Hz). Baked output stays comparable in size to its source.

Determinism: same inputs produce bit-identical output on one platform;
cross-platform float drift is accepted because the cache is per-machine
(§3) and never shared or committed.

### 3. Derived clip cache — content-addressed, generic infrastructure

New `crates/engine/src/derived_cache.rs`, deliberately not animation
-specific (texture/mesh derivations are expected future users):

```rust
pub struct DerivedCache { root: PathBuf }  // <project>/.engine/cache/

impl DerivedCache {
    pub fn get(&self, key: &CacheKey) -> Option<Vec<u8>>;
    pub fn put(&self, key: &CacheKey, bytes: &[u8]) -> io::Result<()>;
}
```

Cache key for a retargeted clip = FNV-1a 64 over, in order:

1. `RETARGET_ALGORITHM_VERSION: u32` (bump on any math change),
2. the **source file fingerprint** (`ImportSettings::source_fingerprint`,
   already maintained) of the clip's source,
3. the clip sub-asset ID string,
4. source and target `SkeletonIdentity`,
5. the retarget map's canonical JSON bytes.

Every input is a reviewable value; forgetting an input is the worst bug
class this design has (stale results forever), so **the key composition
function is a single audited site** with a test asserting each component
changes the key.

Storage: `.engine/cache/anim/<key>.clip.json` — serialized baked clip
(schema_version 1). `.engine/` is already hidden import output (ADR 0075)
and excluded from source control; cache entries are derived artifacts,
never committed, safe to delete at any time.

### 4. Bake and resolution flow

- **Editor:** when spawn/asset-load resolves an animator whose clip
  skeleton ≠ entity skeleton, it looks up a manifest `RetargetMap` for
  that (source, target) pair. Cache hit → load baked clip. Miss → call
  `retarget_clip` synchronously, `put`, load. (Import is already the slow
  path; background baking is a UX upgrade, not a correctness need.)
  No map found → diagnostic `anim.retarget_map_missing`; the clip is not
  applied cross-skeleton silently.
- **Packaging (AP-7 reachability trace):** the build step walks every
  `.scene.json` / `.prefab.json` document and collects the
  `(source_skeleton, target_skeleton)` pairs an `engine.animator` +
  `engine.skeleton` entity actually needs (prefabs are roots
  unconditionally — they may be spawned by script at runtime, so their
  reachability cannot be ruled out statically). The bake set is every
  registered map matching a needed pair, unioned with every map that sets
  `always_package: true` (the escape hatch for clips assigned dynamically
  at runtime, which the static scene/prefab walk cannot see). A needed
  pair with no registered map is still a blocking diagnostic (consistent
  with the MissingAsset policy, ADR 0045). A registered map matching no
  needed pair and without `always_package` is skipped with a
  non-blocking, informational `RetargetMapNotReached` diagnostic instead
  of baked, so the narrowing is observable in the build report rather
  than silent.
- **Player:** loads baked clips only; it never retargets and never needs
  the cache.

## Consequences

- Authors manage source clips + small JSON maps; baked variants are
  disposable cache entries. Diff review sees mapping changes explicitly.
- Runtime animation cost is unchanged by retargeting (baked clips are
  ordinary `AnimationClip`s).
- A future dynamic-retarget path (crowds sharing one clip) reuses
  `retarget_clip` at load/frame time without redesign.
- New failure surface: stale-cache bugs. Mitigated by the single audited
  key site + per-component key test + "delete `.engine/cache` is always
  safe" as a documented recovery.
- `engine` gains `derived_cache` and retarget modules; `editor` gains map
  creation flow and bake calls; packaging gains the bake walk. Breaking
  change protocol applies (one PR per phase, all call sites updated).

## Alternatives Considered

- **Retarget as import settings** — rejected: invisible to review, not
  reusable across sources, untestable in isolation.
- **Runtime retargeting (Unity-style)** — rejected as default: per-frame
  cost and per-instance state for the common case; kept reachable via the
  pure function.
- **Committing baked clips (Godot-style)** — rejected: N×M asset
  explosion in source control; cache entries are derived artifacts.
- **Fixed humanoid enum as the canonical space** — rejected: excludes
  quadrupeds/tails/wings; chains + presets subsume it.
- **Cross-machine shared cache** — deferred: needs deterministic float
  contracts; per-machine cache is sufficient for a solo project.

## Compatibility and Migration

- `*.retarget.json` is a new persisted format with `schema_version: 1`
  and a migration test skeleton from day one.
- Baked cache format is versioned but disposable (not a compatibility
  surface).
- No change to scene/prefab schemas, `StableId`, or `Vertex`.
