# ADR 0098: VMD Motion Composition and Content Routing

- Status: Accepted
- Date: 2026-08-12
- Supersedes: the deferred VMD morph-channel implementation note in ADR 0097

## Context

VMD uses one extension for two target domains. Model motion contains bone,
morph, and model-property keys. Scene motion contains camera, light, and
self-shadow keys. A file can physically contain both domains. Distribution
packages also commonly split one character performance into body, face/lip,
eye, or corrective VMD files, while file names and directory layouts remain
conventions rather than a reliable contract.

The existing importer paired every VMD with one PMX and emitted only baked
bone channels. `AnimationSet` bound one clip per motion slot, so separately
distributed face motion could neither reach `MorphWeights` nor play in sync
with a body clip. Animation crossfade cannot solve that problem: crossfade is
a transition between logical motions, whereas body/face composition forms one
logical motion before a graph state plays it.

## Decision

### Content routing

VMD files are classified from populated binary sections, never from names:

- `Model`: bone, morph, or model-property keys only;
- `Scene`: camera, light, or self-shadow keys only;
- `Mixed`: both domains;
- `Empty`: neither domain.

Only `Model` and `Mixed` sources use PMX pairing and produce model
`AnimationClip` sub-assets. `Mixed` imports its model tracks and reports that
scene tracks were ignored. `Scene` is recognized before pairing and reports
`vmd.scene_motion_unsupported`; `Empty` is invalid. Camera/light/self-shadow
playback requires a future scene-timeline contract and is not represented as
model animation.

### Morph channels

`AnimationClip` owns `MorphChannel { target_name, keyframes }`. The imported
target is a decoded logical PMX/VMD morph name rather than one mesh-local
`Morph` sub-asset ID. At runtime the animator resolves that name against every
`MorphTargets` component whose `SkinnedMesh::rig` is the animator entity and
writes every matching `MorphWeights` entry. This fans one logical expression
out across PMX render parts.

VMD morph keys use linear scalar interpolation and preserve authored values,
including values outside `0..=1`. Before writing a playing animator's sample,
all morph weights on its render parts are cleared so a clip that omits a
previous expression returns it to neutral. During crossfade, a missing morph
channel means neutral weight zero; this differs intentionally from the
existing transform-channel pass-through simplification.

### Ordered clip composition

Animation Set schema version 1 gains an additive, default-empty
`AnimationBinding.overlays` list. `clip` remains the primary layer. All layers
start at the same clip-local time and form one derived runtime clip before the
Animation Graph consumes the slot. Later overlays have higher priority.

Composition unions non-overlapping channels. A later layer replaces an
earlier whole channel for the same `(BoneId, AnimProperty)` or morph name.
Duration is the maximum layer duration, events are merged and sorted, and
root/contact metadata comes from the primary clip. Every layer must declare
the same skeleton ID and identity. Duplicate clip references are invalid.

The derived clip receives a cache identity containing every ordered source
fingerprint and sub-asset ID, so cross-skeleton retarget caches are invalidated
when any layer or its priority changes. Crossfade continues to operate only
between the already-composed clips selected by graph states.

## Consequences

- A body VMD plus face/lip/eye VMDs can be authored as one motion slot without
  merging source files or adding an MMD-only playback runtime.
- Same-channel conflicts are explicit, deterministic, and reviewable in the
  Animation Set editor. Additive or weighted runtime layers remain separate.
- Existing schema-version-1 Animation Sets deserialize with no overlays and
  retain their previous behavior.
- Camera, light, and self-shadow files are no longer accidentally paired with
  PMX models, but their scene-level playback remains deliberately unsupported.
- Morph names are model-semantic. Cross-skeleton retargeting preserves them;
  a target model without a matching name has no target for that channel.
- Editor/runtime conversion can retarget a composed clip using the ordered
  composite cache identity. Packaging that same derived composite for a
  different target skeleton is deferred: the current package bake enumerates
  imported source clips, not Animation Set-derived clips. Ordinary VMD use on
  its paired PMX skeleton is unaffected. A follow-up must add one shared
  build/runtime composition plan and parity tests rather than letting the two
  cache-key paths drift.

## Alternatives considered

### Merge every VMD in one directory during import

Rejected because package naming and layout are not standardized, character
variants and multi-character motions are ambiguous, and conflict priority
would be implicit.

### Add general weighted animation layers to `Animator`

Rejected for this scope because it would redefine graph transitions, events,
root motion, contact metadata, masks, and additive rotation semantics. Ordered
source composition satisfies synchronized body/face distributions while
keeping the runtime's one-logical-clip contract.

### Treat composition as crossfade

Rejected because a transition weight intentionally changes over time, while
body and face tracks must be evaluated together at full authored strength for
the entire logical motion.
