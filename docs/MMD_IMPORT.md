# MMD asset import (PMX models and VMD motions)

The engine imports MMD `.pmx` models and `.vmd` motions directly, via the
`mmd-anim-format` / `mmd-anim-runtime` libraries (ADR 0097). Both are
authoring-time only: import runs in the editor and produces engine-native
assets, so nothing MMD-specific is linked into a packaged build.

## Registering a model

Copy a `.pmx` below the project's `assets/` directory and use **Register
Asset** in the Asset Browser exactly like a `.gltf`, `.glb`, or `.fbx`
source. Direct import normalizes the source to engine conventions
automatically: MMD's left-handed axes are converted to right-handed +Y up,
and positions are scaled to meters using MMD's authoring convention (a
typical character lands roughly life-sized).

A PMX rig routinely has more bones than one skin can render (the engine caps
a skin at 128 joints, ADR 0086). The importer splits the mesh into several
render parts — starting from the model's own materials, subdividing further
by bone locality when a single material still exceeds the cap — and binds
every part to **one shared skeleton**, so a split model is still one Skinned
Model over one rig rather than one per material.

Toon shading, sphere maps (matcap compositing), and edge/outline rendering
are not imported; those materials render flat-lit, and each one that declared
a dropped feature is reported with a `pmx.toon_shading_unsupported`
diagnostic rather than silently changing appearance.

## Registering a motion

Copy a `.vmd` below `assets/` and register it the same way. A motion is an
**animation-only source**: it produces animation clips and nothing else — no
mesh, no material, no skeleton, and no placement prefab.

### Model motion must select one or more output models

Unlike every other source, a model-domain `.vmd` cannot be imported from its
own file alone. It names bones but carries no rig, and MMD resolves a frame through
IK and appended-parent (付与親) chains that live in the `.pmx`. Each motion
therefore records which PMX models receive clips:

- When the project contains exactly one registered `.pmx`, registering a
  motion pairs it automatically.
- Otherwise the motion registers unpaired and reports
  `asset.motion_source_unpaired`. Select one or more output models in **Edit
  Import Settings...** on the motion's Asset Browser row, then **Save and
  Reimport**.
- A motion used in a scene while still unpaired is reported by scene
  validation as `scene.motion_source_unpaired`, so it cannot reach Play
  unnoticed.

**Original PMX model** is optional. Leave it at **Not set - Direct bake** to
preserve the existing behavior: MMD constraints are evaluated separately on
each selected output PMX. This is also the fallback when the VMD's authoring
PMX is unavailable; no separate mode needs to be selected.

When the original PMX is available, select it in that field. The importer
evaluates the VMD once against the original model's IK and appended-parent rig,
then uses the engine's ordinary explicit Retarget Map to produce each different
output clip. An output equal to the original reuses the direct original bake.
A missing or stale Retarget Map stops import with a clear error instead of
silently changing the result back to Direct.

The Import Settings window shows the model name recorded in the VMD header as
an authoring hint, but never guesses a PMX from that text. Names are not stable
identifiers and many distributed motions use a generic or renamed value.

Use **Check compatibility** to inspect the current VMD and selected PMX files
without saving or reimporting. The check includes only meaningful tracks:
translation must leave `(0, 0, 0)`, rotation must leave the identity
quaternion (either sign), and morph weight must leave zero, using an absolute
epsilon of `1e-6`. Every meaningful VMD name is classified as a unique exact
match, missing, or ambiguous when the PMX declares the same name more than
once. The displayed percentage is therefore:

```text
unique exact-name matches / meaningful VMD tracks
```

An ambiguous name is not counted as success. Rotation or translation support
is checked only for a uniquely matched bone and only when that component is
actually used by the VMD. Reports are transient, show at most twenty issue
details, and never rename or modify either source.

With an original PMX selected, its result is the source **Bone
compatibility**. A different output PMX shows **Direct bone-name
compatibility** as informational only, because its bone conversion uses the
explicit Retarget Map and does not require VMD names to match directly.
Output **Morph name compatibility** remains meaningful because morph curves
are not converted through the Retarget Map. This check does not score rest
pose, hierarchy, IK structure, or Retarget Map quality.

A VMD bone name the selected bake model does not have is dropped with a
`vmd.bone_not_found` diagnostic instead of failing the import. If the pairing
itself is wrong, the import reports `vmd.rest_pose_mismatch`.

### What baking produces

Import evaluates MMD's full per-frame pipeline (FK, then IK, then
appended-parent) across the whole motion and records the result as ordinary
keyframe curves, sampled at MMD's native 30 Hz. IK and appended-parent are
gone by the time the clip exists: bones driven only by them come out as plain
rotation channels, and bones that never move emit no channel at all.

The result is one normal `AnimationClip` sub-asset per target PMX. The picker
shows the target model's current name after the motion and clip names, for
example `dance / dance - Hero`. The target's stable asset ID, not that display
name, is part of the clip identity, so model renames do not break references.
Assign a target-specific clip to an Animation
Set's Motion Slot, play it through the Animation Graph / Controller,
cross-fade it, attach Animation Events, drive Root Motion from it — every
existing animation feature works, with no MMD-specific step anywhere
downstream.

Each bake is reported once with a `vmd.curve_resampled` diagnostic. Repeated
imports of the same motion against the same model are served from the derived
cache (ADR 0079 §3), so only the first one pays for the evaluation.

## Reimport

Use **Reimport** after re-exporting a registered source. Sub-asset IDs derive
from the registered source ID and the original selector in the file, so
existing mesh, material, skin, and clip references stay stable across
reimports.

Editing a `.pmx` also changes every motion baked against it — the rig shapes
every baked curve — so a model's fingerprint is part of each paired motion's
fingerprint, and the editor reimports those motions automatically when the
model finishes reimporting.

## Facial morphs

PMX morphs import as `Morph` sub-assets, one per morph per render part it
touches. MMD group morphs are folded into the vertex and material changes
they imply at import time, so a group behaves like any other morph at
runtime with no separate concept to understand.

Morphs blend on the CPU over only the vertices they actually move, which for
a face is a small fraction of a character's mesh. Because weights are
per-entity, a morphing renderer keeps a private copy of its mesh rather than
sharing the one every other instance draws from — the cost of that copy is
what buys two characters independent expressions.

Material morphs are a separate, much smaller path: they override the
renderer's base color per frame instead of touching vertices at all.

VMD morph curves now import as scalar `AnimationClip` channels. The animator
resolves each decoded morph name against every render part belonging to its
PMX rig, so one facial curve drives all split meshes that contain that logical
morph. VMD morph values are not clamped. During a crossfade, a morph absent
from one side is treated as neutral weight zero.

## Motions distributed as several VMD files

Keep every source VMD registered independently. In the Animation Set editor,
assign the body or otherwise primary clip to a Motion Slot, then add face,
lip, eye, or corrective clips to that binding's ordered **Overlays** list.
All layers start together and are resolved as one logical clip before the
Animation Graph plays the slot.

Non-overlapping bone and morph channels are combined. When two layers drive
the same bone property or morph name, the later overlay has priority and
replaces that whole channel. Reorder overlays to make that priority explicit.
This composition is separate from crossfade: composition builds the motion
played by one graph state, while crossfade transitions between two states.

Do not infer roles from names such as `body`, `face`, or `camera`. The editor
examines the VMD's actual sections. Model or mixed model/scene VMDs can be
paired with PMX. A mixed file imports its model tracks and reports that its
scene tracks were ignored. Camera/light/self-shadow-only VMDs are recognized
and are not paired with a PMX model.

## Rigid bodies and joints (jiggle physics)

A PMX's rigid bodies and six-degree-of-freedom spring joints import as one
`RigidBodyRig` sub-asset per model, in engine units and axes. Add
**Rigid Body Physics** to the character's Skinned Model entity and point it
at that sub-asset — one component, one reference, nothing else to configure,
because the model's author already tuned every body and joint in their own
tool.

The rig is engine-native data plus intent. At runtime, ADR 0096's isolated
Rapier bridge creates one solver world per opted-in character, drives
follow-bone bodies from animation, simulates dynamic bodies and 6-DOF spring
joints, then writes corrected poses back to the skeleton. These cosmetic
bodies never enter gameplay collision events or spatial queries.

## Not yet imported

- **Packaged-player bake of a composed motion that also needs cross-skeleton
  retargeting.** Editor Play can compose the layers and retarget the resulting
  logical clip, and ordinary PMX-paired VMD playback does not enter this path
  because the motion already targets that PMX rig. The package builder's
  existing retarget pass, however, enumerates imported source clips rather
  than derived Animation Set compositions. Supporting this combination needs
  a build-time composition/bake plan whose cache identity and reachability
  exactly match runtime resolution; that package-contract change and its
  editor/player parity tests belong in a separate task. Until then, use the
  paired PMX rig in packaged content or pre-author one source clip.
- **VMD camera, light, and self-shadow tracks.** Deliberately out of scope:
  this pipeline imports a character, not a cinematic. These files are now
  identified before PMX pairing and report `vmd.scene_motion_unsupported`.
  Playback needs a scene timeline that binds cameras, lights, shadows, and
  audio rather than a model `Animator`, so it remains a separate task.
