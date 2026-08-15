# ADR 0106: Layered Rig Pose and Pose Graph Boundary

Status: Accepted
Date: 2026-08-13

## Context

Animation, foot IK, and MMD physics previously wrote the same joint Transform
components directly. A Transform did not identify whether it represented an
animation input, a procedural correction, a physics result, or the final
published pose.

A physics-composed Transform can therefore become input to a later fixed step.
Partial animation channels and PMX rigid-body write-back make this feedback
visible as progressively stretched or folded meshes.

The engine also needs a reusable foundation for layered animation, partial
ragdolls, additive animation, look-at constraints, and retargeted pose
composition.

SkeletonAsset bones and Skeleton joint entities already share a stable,
parent-before-child order. The pose runtime can reuse that order instead of
building entity-keyed pose maps every frame.

## Decision

Each runtime rig will own contiguous local-space pose data aligned with
SkeletonAsset bone order:

1. immutable rest pose;
2. persistent animation layer;
3. transient procedural layer;
4. transient physics layer; and
5. composed final pose.

Each layer stores per-bone translation, rotation, and scale ownership. Layer
composition supports exact replacement and weighted interpolation.

Composition always begins from the imported rest pose and applies animation,
procedural, and physics layers in that order. It never reads the previous
final pose.

This rest baseline matches current rig creation: spawn_rig initializes every
joint Transform from the same SkeletonAsset rest TRS.

RigPose also stores each bone's parent index and a reusable world-matrix
buffer. evaluate_world evaluates one composition prefix and performs forward
kinematics in a single parent-before-child pass rooted at the supplied model
world matrix.

Foot IK requests the Animation stage. MMD physics requests the
Procedural stage. Skinning and attachments will observe the Final stage after
publication and transform propagation.

Physics write-back rebuilds the complete hierarchy in parent-before-child
order. Non-physics spacer bones inherit an already resolved physics ancestor
before a later physics child is localized; consulting only the child's direct
physics parent would apply the ancestor correction twice at publication time.

The existing Animation State Graph continues to select states, motion slots,
and transitions. The pure Pose Graph evaluates clip sampling, blending,
additive operations, and bone masks into the animation layer.

World-aware and stateful operations such as ground-query foot IK, MMD physics,
and ragdoll simulation remain ordered modifiers outside the pure Pose Graph.

Root motion remains a side output consumed by the character motor. Morph
weights remain outside skeletal PoseBuffer storage.

## Joint Transform ownership

The migration is complete. Play-time direct writes to
joint Transform components are unsupported. Engine pose producers must write a
RigPose layer. Joint Transform becomes derived publication output.

BoneAttachment remains supported because it reparents the attached entity
under a joint and reads the propagated final transform. It does not write the
joint itself.

## Implemented migration

1. PoseBuffer, PoseLayer, PoseStage, PoseBlend, and RigPose own rig pose data.
2. Every runtime Skeleton receives a matching RigPose. spawn_rig returns both
   components together as SpawnedRig, because a Skeleton without a pose has no
   remaining path from an animation clip to its joints and would fail silently
   rather than loudly.
3. Animation and Foot IK write animation and procedural layers.
4. MMD physics reads the Procedural stage and writes the physics layer. PMX
   mode 1 owns translation and rotation; mode 2 owns rotation only.
5. Pure clip sampling, crossfading, layered blending per bone, and additive
   composition run through PoseGraphOutput. BoneMask scales a blend weight per
   joint; additive deltas are expressed as a translation offset, a
   parent-relative rotation, and a scale ratio. PoseArena recycles the
   intermediate buffers so a steady-state frame allocates nothing per bone.
6. One final publisher writes composed joint Transforms after physics.
   Long-running tests cover both the solver alone and the complete fixed-step
   order with animation, verifying bounded, finite output.

A skeletal channel that only the additive side drives is dropped rather than
applied to an assumed neutral value, because the rest pose lives in RigPose and
is deliberately invisible to this data-only stage. Morph weights do have an
unambiguous neutral of zero, so a one-sided morph still contributes.

Authoring for blend, additive, and mask nodes is out of scope here. The
existing `*.graph.json` Animation State Graph format is unchanged; exposing
these operations to authoring requires its own ADR.

This ADR now supersedes ADR 0080 and ADR 0096 where they specify direct joint
Transform writes.

## Fixed-step order after migration

1. Clear transient pose layers.
2. Evaluate the Animation State Graph.
3. Evaluate animation or the pure Pose Graph.
4. Extract and submit root motion.
5. Move gameplay entities.
6. Propagate current character-root and world transforms.
7. Evaluate procedural modifiers.
8. Simulate stateful physics modifiers.
9. Compose Final Pose.
10. Publish final joint transforms.
11. Propagate final world transforms.

## Consequences

Pose ownership becomes explicit and physics output cannot feed back into the
next animation pose.

Per-bone storage is contiguous and avoids entity-keyed pose maps in the final
implementation. Where a stage must still cross the pose/entity boundary — clip
channels resolving a BoneId, rigid bodies resolving their bound bone, and the
publisher resolving joint entities — it prepares one lookup per rig per step
instead of scanning per channel, per body, or per destination entity.

Joint entities remain available for attachments, skinning, and engine-internal
BoneId resolution.

The runtime carries both pose buffers and derived joint Transform components.
This duplication is intentional because Transform remains the hierarchy,
skinning, and attachment publication surface.

No authoring schema, serialized scene format, stable identifier, asset
manifest, PMX source, or imported model format changes in this ADR.
