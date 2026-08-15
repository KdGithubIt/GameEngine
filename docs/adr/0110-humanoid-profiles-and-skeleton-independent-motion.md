# ADR 0110: Humanoid Profiles and Skeleton-Independent Motion

Status: Accepted
Date: 2026-08-14
Amends: ADR 0079, ADR 0085, ADR 0099

## Context

ADR 0077 intentionally makes an imported `AnimationClip` skeleton-bound. Its
bone channels target stable `BoneId` values owned by one `SkeletonAsset`, which
preserves the full source rig and avoids treating names as runtime identity.
ADR 0079 then provides explicit `RetargetMap` assets and target-specific derived
clips for cross-skeleton reuse.

That generic design is necessary, but it leaves a common authoring case too
manual. PMX, Mixamo, FBX, VRM, and other biped rigs often represent the same
human body semantics even when their bone names, rest pose, local axes, helper
bones, and proportions differ. Requiring an explicitly authored Retarget Map
for every humanoid source/target pair prevents a reusable "this is a humanoid
walk" asset from existing.

Making a fixed humanoid skeleton the universal retarget space is not acceptable
either. ADR 0079 rejected that approach because real rigs contain information
outside a fixed human vocabulary: twist helpers, hair, skirts, tails, capes,
weapon bones, wings, PMX-specific controls, morphs, and other authored
channels. Converting every animation to only Humanoid channels would make a
source animation less expressive even when the target is the original model or
another rig with a richer explicit mapping.

The authoring UX must therefore make two facts visible at the same time:

- an animation can have a skeleton-independent Humanoid representation; and
- its full-fidelity Native clip still exists and can be the better choice.

Humanoid support is a compatibility layer, not a replacement for skeleton-bound
clips or generic retargeting.

## Decision

### 1. Add a model-owned `HumanoidProfile`

A humanoid-capable imported model may own a `HumanoidProfile` associated with
its `SkeletonAsset`. The profile maps stable humanoid semantic identifiers to
that skeleton's stable `BoneId` values. The original `SkeletonAsset` remains
complete; no source bone is removed, renamed, or replaced by a canonical
Humanoid skeleton.

The initial required body semantics are:

- `Hips`
- `Spine`
- `Head`
- `LeftUpperArm`, `RightUpperArm`
- `LeftLowerArm`, `RightLowerArm`
- `LeftHand`, `RightHand`
- `LeftUpperLeg`, `RightUpperLeg`
- `LeftLowerLeg`, `RightLowerLeg`
- `LeftFoot`, `RightFoot`

Optional semantics may include `Chest`, `UpperChest`, `Neck`, shoulders, toes,
fingers, eyes, and jaw. Optional semantics improve fidelity but do not decide
whether the profile is structurally usable.

Semantic parent/child validation is based on ancestry, not direct adjacency.
Helper and twist bones may exist between two mapped Humanoid bones. A profile
is invalid only when required semantics are missing, duplicated incompatibly,
or violate the required body-side/hierarchy structure.

`motion_root` is a separate optional `BoneId`; it is not
`HumanoidBone::Hips`. This is required for rigs such as PMX where center/groove
style motion controls and the anatomical lower body are distinct concepts.

### 2. Persist profile decisions with the model, without another Asset Browser row

Humanoid detection runs during model import/configuration using import-time
evidence such as known PMX or Mixamo conventions, bone names, hierarchy,
left/right placement, and rest-pose geometry. Names are heuristics only; once a
profile is resolved, its semantic mappings are persisted as `BoneId` values.

Following ADR 0029, the profile is model-owned authoring/import metadata in the
registered source's `asset_manifest.json` entry. It is not a disposable
Derived Cache record and does not introduce a second top-level
`*.humanoid.json` or `.meta` source of truth.

The Asset Browser continues to present one registered model source. Selecting
that model exposes Humanoid status and a **Configure Humanoid** action in the
Inspector. The configuration UI edits the persisted model-owned profile.

A profile that is structurally valid but contains uncertain auto-detected
mappings remains usable and is shown with a warning. The warning must identify
the questionable mappings and provide the Configure action. Structural
invalidity blocks Humanoid conversion and reports a diagnostic instead of
guessing at playback time.

Reimport revalidates the persisted profile against the imported
`SkeletonIdentity` and `BoneId` set. A still-valid authored mapping is
preserved. If the skeleton identity or mapped bones make the profile stale,
automatic detection may propose/rebuild a usable mapping, but an explicit user
mapping must not be silently overwritten without a diagnostic.

### 3. Add a distinct skeleton-independent `HumanoidMotion` sub-asset

When an imported animation has a structurally usable source `HumanoidProfile`,
import may generate two independently addressable variants under the same
logical source animation:

- **Native**: the existing skeleton-bound `AnimationClip`, unchanged and
  full-fidelity;
- **Humanoid**: a `HumanoidMotion` whose skeletal channels target Humanoid
  semantics rather than source `BoneId` values.

`HumanoidMotion` is a first-class imported/derived sub-asset with its own stable
`AssetId`, but it remains nested under the registered source rather than
becoming a second top-level source file.

The Humanoid representation stores reference-pose-relative semantic motion,
not the source rig's raw local rotations. For a mapped semantic bone, the
rotation carried by the motion is the source model-space delta

```text
q_delta_model =
    q_source_animated_model * inverse(q_source_rest_model)
```

and target baking reconstructs the target model-space rotation with

```text
q_target_animated_model =
    q_delta_model * q_target_rest_model
```

before converting the result back through the target hierarchy. This uses the
same model-space delta principle accepted by ADR 0079 while making the stored
motion independent of one source skeleton's local axes and rest pose.

`HumanoidMotion` may also carry duration, root-motion data, and timeline events
that are not intrinsically tied to a source `BoneId`. Root motion is extracted
through the source profile's separate `motion_root`.

This ADR does not invent a universal translation/proportion scaling formula.
Before cross-proportion non-rotational Humanoid channels are implemented, their
normalization and target reconstruction policy must be specified explicitly.
They must not silently reuse raw source-local translation.

### 4. Never discard source-specific animation to create Humanoid compatibility

Automatic Humanoid conversion is allowed to be body-only. Channels that cannot
be represented by the Humanoid semantic vocabulary are excluded from the
`HumanoidMotion` and produce a non-blocking diagnostic such as "excluded N
source-specific channels."

The Native `AnimationClip` is always retained. In particular, Humanoid
generation must not destroy or silently absorb source-specific animation for
twist helpers, hair, skirts, tails, capes, weapon bones, PMX-specific controls,
or morphs.

Two different humanoid skeletons may happen to contain similarly named extra
bones. Automatic Humanoid adaptation must not opportunistically copy those
extra channels by name. Cross-skeleton transfer of information outside the
Humanoid vocabulary requires an explicit `RetargetMap` or a future explicitly
authored richer mapping. This keeps fidelity choices deterministic and
reviewable instead of making them depend on import-time name coincidences.

For PMX/VMD, the existing VMD-to-target-PMX Native bake remains the
full-fidelity path. When that PMX has a usable Humanoid profile, the body
portion may additionally produce a HumanoidMotion for reuse on other humanoid
targets such as Mixamo, FBX, or VRM rigs. PMX-specific bones and morphs remain
available through the Native path.

### 5. Resolve motion with lossless-first automatic precedence

The editor and build pipeline use the following precedence when a user chooses
an automatic/default logical animation for a target rig:

1. use the Native clip directly when it is already compatible with the target
   `SkeletonAsset`;
2. otherwise use the Native clip through an applicable explicit
   `RetargetMap` from ADR 0079;
3. otherwise bake `HumanoidMotion + target HumanoidProfile` when both sides
   are valid;
4. otherwise report the binding as unsupported.

This ordering is an information-preservation rule. Humanoid is the broad
compatibility fallback, not an assertion that Humanoid retargeting is more
faithful than a source-native or explicitly authored path.

Explicit user selection wins over the automatic ordering:

- explicitly selecting **Humanoid** uses the Humanoid path even if a Native or
  explicit-map path also exists;
- explicitly selecting **Native** does not silently fall back to Humanoid if
  the Native path cannot resolve;
- an explicit Retarget Map wins over *automatic* Humanoid adaptation when the
  logical animation is in automatic mode.

ADR 0079's rejection of a fixed humanoid enum as the universal canonical
retarget space therefore remains in force. This ADR narrows the role:
Humanoid semantics are one domain-specific vocabulary for biped compatibility,
while `RetargetMap` remains the generic and potentially higher-fidelity
cross-skeleton mechanism.

### 6. Show Native and Humanoid as selectable variants of one logical animation

The Asset Browser and animation pickers must not hide the Native variant merely
because Humanoid conversion succeeded. They also should not flatten every
imported animation into two unrelated top-level rows.

The intended presentation is one logical animation with variants, for example:

```text
Walk
  Auto
  Native [SourceSkeleton]
  Humanoid
```

Both Native and Humanoid are explicitly selectable. `Auto` represents the
lossless-first resolution policy in Decision 5. Exact tree, disclosure, badge,
or menu presentation is editor-owned, but the fidelity distinction and the
ability to choose both variants are part of the authoring contract.

Diagnostics for excluded Humanoid channels are shown with the Humanoid variant
without marking the Native variant as degraded.

### 7. Animation Sets bind stable motion sources, not only Native clip IDs

ADR 0085's stable `MotionSlotId` remains unchanged. The binding behind a slot
is generalized so that authoring can reference a Native `AnimationClip`, a
`HumanoidMotion`, or the logical automatic source selection described above.
The persisted representation must remain an explicit tagged/stable reference;
the implementation must not infer which variant was authored from display
names.

Existing Animation Set composition from ADR 0098 remains ordered and
deterministic. Every referenced motion source must first resolve/bake to the
target skeleton's ordinary `AnimationClip` representation; overlay composition
and graph playback then operate on target-bound clips as they do today.

An existing Animation Set binding that names an imported `AnimationClip`
continues to mean the explicit Native variant. Introducing Auto or Humanoid
selection must not reinterpret an old clip reference.

### 8. Bake Humanoid adaptation before the runtime animation hot path

`HumanoidMotion + target HumanoidProfile` is resolved into a target-specific
ordinary `AnimationClip` in authoring/build conversion and stored in the
Derived Cache. The runtime `Animator`, Animation Graph, `RigPose`, root-motion
consumers, MMD physics, and skinning continue to consume target-bound
`AnimationClip` data rather than executing Humanoid matching every frame.

The Humanoid-derived cache identity must include at least:

- the HumanoidMotion content/identity;
- the target `SkeletonIdentity`;
- the target HumanoidProfile content/revision; and
- a Humanoid retarget algorithm version.

A change to any of those inputs invalidates the derived target clip. Packaged
Player content consumes the resolved/baked clips required by reachable
Animation Sets; it does not require a live editor-side Humanoid solver.

There is no engine-owned "Default Humanoid Skeleton" asset. The common space is
semantic motion plus profiles, not an artificial skeleton that all imports are
rewritten to match.

## Consequences

- A Humanoid-compatible motion becomes visibly reusable across independently
  imported biped rigs without authoring an N-by-M matrix of Retarget Maps.
- Source models retain their complete skeletons and Native animations, so
  Humanoid support cannot reduce fidelity for same-skeleton playback.
- Rich cross-skeleton transfer remains possible through explicit Retarget Maps,
  including helper or extra bones outside the Humanoid vocabulary.
- The normal runtime animation path stays simple because Humanoid adaptation is
  paid during resolution/bake, not during every animation sample.
- Model import gains persisted Humanoid profile state and structured
  diagnostics for uncertain/invalid mappings.
- Imported animation catalogs gain a Humanoid sub-asset/variant and Animation
  Set references eventually need an explicit motion-source representation.
- Asset Browser and picker UX must communicate fidelity and warnings rather
  than presenting Humanoid conversion as lossless.
- Changes to a profile can invalidate Humanoid-derived clips even when the
  underlying SkeletonIdentity did not change.

## Alternatives Considered

### Replace `AnimationClip` with a Humanoid-only animation type

Rejected. It would lose source-specific channels and make playback on the
original model less faithful than the imported data.

### Use a fixed engine Humanoid skeleton as the universal retarget space

Rejected as a universal model for the same reason recorded in ADR 0079: it
cannot represent arbitrary rigs or extra semantic structure. A Humanoid
vocabulary is accepted only as an optional biped-specific compatibility layer.

### Keep only `HumanoidProfile` and synthesize temporary Retarget Maps

Rejected as the complete authoring model. It could automate pairwise
retargeting, but it would not create a first-class skeleton-independent motion
that users can identify, select, cache, and reuse independently of the source
rig.

### Hide Native clips whenever Humanoid conversion succeeds

Rejected. The Native clip may animate twist, hair, skirt, tail, cape, weapon,
morph, or other channels that HumanoidMotion intentionally omits. Hiding it
would obscure the highest-fidelity choice and could cause silent animation
loss.

### Show Native and Humanoid as unrelated flat assets

Rejected as the default UX. It preserves choice but doubles visible animation
rows and obscures that both variants came from the same logical source
animation. Grouped variants preserve choice without catalog noise.

### Retarget Humanoid semantics every runtime frame

Rejected. The engine already has a target-bound AnimationClip runtime path and
a Derived Cache. Baking once keeps Animator, Animation Graph, RigPose, MMD
physics, and packaged Player behavior independent of Humanoid authoring logic.

### Automatically match extra bones by name between humanoid rigs

Rejected. Bone names are import heuristics, not stable runtime identity.
Implicit extra-bone matching would make fidelity source-dependent and hard to
review. Explicit Retarget Maps are the correct contract for richer transfer.

## Compatibility and Migration

This ADR itself is a documentation/architecture change; it does not change the
currently implemented serialized schemas or runtime APIs.

When implemented:

- existing skeleton-bound `AnimationClip` assets and their stable IDs remain
  valid Native variants;
- existing `RetargetMap` assets remain valid and retain precedence over
  automatic Humanoid adaptation;
- existing Animation Set clip bindings retain their Native meaning;
- Humanoid profile data is an optional extension of model import metadata, so a
  model without a profile remains a valid non-Humanoid model;
- HumanoidMotion is an additional nested sub-asset and never replaces the
  Native imported clip;
- if the Animation Set schema must change to encode tagged Native/Humanoid/Auto
  references, implementation must update all in-tree content atomically under
  ADR 0091's single-version document policy rather than adding a permanent
  compatibility reader.

ADR 0099's target-specific VMD Native clips remain valid and unchanged.
HumanoidMotion is an additional reusable body-motion derivative, not a new
identity for those target-specific clips.
