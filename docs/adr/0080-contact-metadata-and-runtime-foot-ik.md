# ADR 0080: Contact Metadata and Runtime Foot IK

Status: Accepted
Date: 2026-07-21

## Context

FK retargeting (ADR 0079) matches orientations but not world-space
meaning: a longer-legged target slides or floats where the source was
planted. The original design draft proposed baking an IK correction pass
into the retargeted clip. That is unsound: the engine blends clips at
runtime (Phase 59 crossfade, anim_graph), contact intervals of two clips
never coincide, and blending two individually-corrected poses breaks both
corrections. Therefore:

- What can be baked is **detection** (where contacts are, a property of a
  single clip).
- What must run at runtime is **correction** (a constraint on the final,
  post-blend pose).

This ADR defines both halves. It depends on ADR 0077 (BoneId, rest poses)
and composes with 0079 (retargeted clips carry their intervals through).

## Decision

### 1. Contact intervals are clip metadata, detected at import/bake

```rust
pub struct AnimationClip {
    // ... (ADR 0077 fields)
    /// Detected ground-contact spans, sorted by start time.
    pub contacts: Vec<ContactInterval>,
}

/// One span during which a bone is treated as planted.
pub struct ContactInterval {
    pub bone: BoneId,
    pub start: f32,   // seconds, inclusive
    pub end: f32,     // seconds, exclusive
}
```

Detection (in the import builder, ADR 0078, and re-run on retargeted
output in 0079 — intervals are re-detected on the baked clip, not copied,
so the target's own proportions decide):

- Candidate bones: leaf-side bones whose name matches foot patterns
  (`foot`, `ankle`, `toe`, case-insensitive) — a heuristic override list
  rides `ImportSettings` (`contact_bones: Vec<String>`, additive field).
- FK-sample each candidate's model-space position at 60 Hz.
- Contact when **speed < 0.15 m/s** AND **height above the clip's own
  per-bone minimum < 0.08 m**, sustained ≥ 0.1 s; adjacent frames merge,
  sub-threshold gaps < 0.05 s are bridged.
- Thresholds are constants in one module with rustdoc; false positives
  are expected for sliding motions (moonwalk), so detection is a default,
  and per-clip manual override is editor UX (AP-5) layered on the same
  data — automation is an assist, not an authority.

### 2. Runtime correction: post-blend two-bone foot IK

New fixed-schedule system, registered **after** `animation_system` (both
run on the fixed timestep; correction sees the final blended local pose):

```rust
pub fn foot_ik_system(/* skeletons, animators, transforms, colliders */)
```

Per animator with a `Skeleton` and an opt-in `FootIk` component:

```rust
pub struct FootIk {
    /// Max vertical adjustment (m); larger errors are left alone.
    pub max_correction: f32,      // default 0.3
    pub enabled: bool,            // default true
}
```

1. **Contact weight** per foot bone: for each clip currently contributing
   to the pose (target and crossfade source), weight = clip blend weight
   if `time` is inside one of its `ContactInterval`s for that bone, else
   0; summed and clamped to [0,1], then eased over 0.1 s at interval
   edges so corrections fade in/out instead of popping.
2. **Ground query:** vertical ray from the foot's FK model-space position
   (own local FK over the leg chain — hip, knee, ankle resolved by
   walking `BoneId` parents), against **static colliders** using the
   Phase 58 segment-vs-AABB slab test. No hit within `max_correction` →
   weight 0 (no correction without ground truth — never guess).
3. **Two-bone analytic IK** (law-of-cosines) on hip–knee–ankle: solve the
   knee angle placing the ankle at the ground target, preserve the
   knee-bend plane from the FK pose, blend result with FK pose by the
   contact weight, write the RigPose procedural layer (ADR 0106). Chains that are not exactly
   three bones long, degenerate lengths, or unreachable targets skip with
   a one-shot diagnostic — never a panic, never an extreme pose.
4. Pelvis follow-up: when both feet correct downward, the pelvis (clip
   `root_bone` child convention: the hip chain root) lowers by the lesser
   correction so legs do not hyperextend. Single constant behavior, no
   configuration in v1.

Cost: two raycasts + two analytic solves per character per fixed tick,
only for entities that opted in via `FootIk`.

### 3. What is explicitly not in v1

- Hand/weapon contact constraints (same data model extends later; the
  interval struct is bone-generic on purpose).
- Full-body IK, hyperextension stretching, toe rolling.
- Baking corrections into clips — rejected permanently, see Context.

## Consequences

- Retargeted characters with different proportions keep planted feet
  through blends and crossfades — the case that breaks the bake-only
  design.
- The "runtime cost 0" claim of the original draft is amended honestly:
  FK retarget remains zero-cost (baked); contact correction is a small,
  opt-in, per-character runtime cost.
- `animation.rs` gains clip metadata; `skinning`/transform code is
  untouched; collision query reuse ties `engine` animation to the
  existing collider module (both already in `engine`, no new crate edge).
- Detection thresholds will misfire on stylized motion; the override
  field and AP-5 editing exist for exactly that, and a wrong interval
  degrades to (at worst) the uncorrected pose because correction is
  ground-verified and clamped.

## Alternatives Considered

- **Bake IK correction into retargeted clips** — rejected: breaks under
  blending (Context); also couples baked output to level geometry.
- **Full IK rig / solver graph** — rejected for v1: two-bone analytic
  covers the dominant artifact (feet) at a fraction of the complexity.
- **Physics raycast against dynamic bodies** — deferred: moving-platform
  foot planting needs velocity inheritance; statics cover the common
  case.
- **Contact detection at runtime (no metadata)** — rejected: runtime
  cannot distinguish "foot slow because planted" from "foot slow at swing
  apex" without lookahead; offline detection sees the whole timeline.

## Compatibility and Migration

- `AnimationClip::contacts` is a runtime field (clips are rebuilt from
  sources); baked cache entries embed it under the existing cache
  versioning (ADR 0079) — no independent format surface.
- `ImportSettings::contact_bones` is additive with `skip_serializing_if`.
- `FootIk` is a new opt-in component registered in the builtin component
  registry (follows ADR 0027 conventions); existing content is
  unaffected until it opts in.
