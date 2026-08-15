# ADR 0088: Bone Attachment and Reading a Rig's Pose

Status: Accepted
Date: 2026-07-26

## Context

ADR 0086 made every bone of a skeleton asset exist as a joint entity, and
ADR 0087 gave rigs an owner. Neither gives an author a way to say "this sword
is in that hand", and neither gives game code a way to read where a hand is.

Both are the same underlying question — how does something outside the rig
address a bone — and both have the same failure mode if answered casually:
a binding that silently follows the wrong bone after a reexport.

Three constraints shape the answer:

1. **ADR 0077** requires every persisted bone binding to be a `BoneId`. Bone
   names exist for first-import heuristics and human-facing diagnostics.
2. **Animation is fixed-step, transform propagation is not.** `engine.animation`
   runs as a fixed system, while `engine.transform_propagation` and
   `engine.joint_palette` run once per rendered frame. A caller inside a fixed
   step and a caller inside the variable step therefore do not see the same
   joint world matrices.
3. **ADR 0049** keeps game modules on a command-and-copied-view boundary. A
   synchronous "give me this bone's matrix now" call across that boundary is
   not expressible, and copying every bone of every rig into a view each frame
   is not affordable.

## Decision

### 1. `engine.bone_attachment` binds an entity to a bone

```
engine.bone_attachment (schema version 1)
├─ rig       : EntityRef -> entity carrying engine.skinned_model
├─ bone      : I64       -> BoneId within that rig's skeleton asset
└─ bone_name : String    -> the bone's name when it was picked
```

`bone` is the binding. `bone_name` is written alongside it so the Inspector
and diagnostics can say "RightHand" instead of "bone 37", and so a binding
that stops resolving can name what it was looking for. Nothing reads
`bone_name` to resolve anything, which is exactly the division ADR 0077 draws.

The Inspector offers the bones of the referenced rig's skeleton by name and
writes the `BoneId` behind it, so an author never types an ID and never picks
a bone that is not in that rig.

### 2. Attachment is reparenting, resolved after conversion

Conversion adds a runtime `BoneAttachment { rig, bone }`. A post-pass then
reparents the entity onto the joint entity for that bone: the attached entity
becomes a child of the joint, and its own `engine.transform` becomes the
offset from the bone.

Reparenting rather than per-frame matrix copying means the existing transform
propagation does all the work, an attached entity's own children follow
correctly with no extra system, and the attachment costs nothing per frame.

The pass runs after every component on every entity is dispatched, like the
existing cross-skeleton clip resolution, because the rig entity's `Skeleton`
is not guaranteed to exist while the attachment's own component is being
dispatched.

A `rig` that is unassigned, an entity that carries no rig, or a `bone` the rig
does not have all leave the entity where it was, with a diagnostic. Nothing
snaps to the origin and nothing panics.

### 3. Reading a bone's pose is reading an attachment's transform

There is no `rig.bone_transform(character, "RightHand")` in the game-module
API. The supported way to read where a bone is, is to attach an entity to that
bone in the editor and read that entity's ordinary transform view.

This is not a smaller version of a bone query; it is the version that fits the
boundary. The set of bones a game actually cares about is small, known while
authoring, and already needs to exist as an entity for anything to be mounted
there. Declaring it makes the cost bounded and the dependency visible, where an
open query would copy hundreds of matrices per rig per frame or reintroduce
the synchronous call ADR 0049 removed.

Engine-internal systems keep direct `Skeleton` access, and
[`Skeleton::joint_of`] resolves a `BoneId` to its joint entity for them.

### 4. The pose a caller observes is the last completed propagation

`GlobalTransform` on a joint is whatever `engine.transform_propagation` last
wrote, which is the most recent rendered frame. A fixed-step system that runs
several times in one frame reads the same value each time; it does not
observe intermediate poses.

This is stated rather than fixed. Making a fixed-step caller observe its own
sub-step would mean running propagation inside the fixed loop, which changes
scheduling for every entity in the world to serve bone reads. Attachments do
not care — they are resolved by propagation itself, one hierarchy pass later,
so an attached entity is never a frame behind what is drawn.

## Consequences

- Mounting a weapon, an effect, or a camera target on a bone is authoring
  data: it saves, it validates, it undoes, and it survives reimport as long as
  the bone does.
- An attachment costs one `Parent` link. No system iterates attachments each
  frame.
- Bone bindings survive a reexport that renames bones, and break loudly on a
  reexport that removes them.
- Game code reads bone positions through entities it declared, so the
  module boundary keeps its bounded copied-view shape.
- A game that genuinely needs an arbitrary runtime bone lookup is not served
  by this ADR. Adding one later means adding a bounded selection mechanism,
  not a general query.

## Alternatives Considered

### Persist the bone by name

Rejected. It contradicts ADR 0077's rule for a case that is not special: the
manifest already carries a stable `BoneId` ledger across reimports, and the
name is the part a DCC round-trip changes. The name is kept beside the ID for
humans, which is the role ADR 0077 assigns it.

### Copy the attached joint's world matrix each frame instead of reparenting

Rejected. It needs a system ordered after propagation, and it leaves the
attached entity's own descendants stale for a frame unless propagation runs
again. Reparenting gets both for free.

### Expose `rig.bone_transform(entity, name)` to game modules

Rejected for now. A per-call lookup across the module boundary is a
synchronous read ADR 0049 does not allow, and the copied-view alternative
means copying every bone of every rig. §3's declared attachment covers the
real use case at bounded cost.

### Resolve attachments during component dispatch

Rejected. The rig entity's `Skeleton` may not exist yet, because components
dispatch in `ComponentTypeId` order within an entity and entities dispatch in
plan order. The post-pass has every rig in hand.

## Compatibility and Migration

`engine.bone_attachment` is new at schema version 1. No existing component,
serialized format, `StableId`/`AssetId` derivation, or schedule position
changes, and no existing content is rewritten. A scene with no attachments
behaves exactly as before.
