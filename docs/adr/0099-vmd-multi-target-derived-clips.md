# ADR 0099: VMD Multi-Target Derived Clips

- Status: Accepted
- Date: 2026-08-12
- Amends: ADR 0097 section 3a

## Context

ADR 0097 registered one VMD source with one `motion_model_source` and derived
its animation sub-asset ID from only `(VMD source, animation index)`. Changing
the paired PMX therefore changed the skeleton and baked channel data behind an
unchanged clip ID. The same VMD also could not keep clips baked for two PMX
models available at the same time.

The target PMX is a semantic bake input, not presentation. It supplies IK,
appended-parent, axis, bone-morph, rest-pose, and stable `BoneId` data that a
VMD and an engine `SkeletonAsset` do not contain by themselves.

## Decision

One registered VMD source owns an ordered `motion_model_sources` list. Import
also owns an optional `motion_original_model_source`:

- When it is absent, import preserves direct bake: the VMD is evaluated
  separately against every output PMX.
- When it is present, import evaluates MMD IK and appended-parent constraints
  once against that original PMX, then retargets the resulting ordinary FK
  clip to every different output PMX through the registered explicit Retarget
  Map for the skeleton pair.
- When an output is the original PMX itself, import reuses the original bake
  without a redundant retarget pass.

The editor does not persist a separate mode enum. `None` is the explicit
Direct choice in the optional original-PMX picker, which keeps every existing
manifest on its previous behavior. A missing or stale Retarget Map is an error;
the importer never silently falls back to a direct bake.

Import Settings also offers an on-demand, read-only VMD/PMX compatibility
report. It considers only tracks that leave their neutral value, with an
absolute epsilon of `1e-6`, and classifies every meaningful exact name as
unique, missing, or ambiguous. Compatibility is the number of unique matches
divided by the number of meaningful VMD tracks; duplicate PMX names are never
successes. Bone operation flags are checked only for uniquely matched bones
and only for translation or rotation components that the VMD actually uses.

The report preserves the distinction introduced by this ADR. The original
PMX receives source bone compatibility. For a different output PMX, direct
bone-name compatibility is informational because the Retarget Map performs
the conversion. Output morph-name compatibility remains actionable because
morph tracks do not use the bone Retarget Map. Results are transient editor
state and do not affect manifests, serialized formats, import commands, or
derived clip identity.

Import produces one clip per selected PMX target and source clip index. Each canonical
derived clip ID is a deterministic function of:

```text
(VMD source AssetId, PMX source AssetId, animation index)
```

An imported VMD clip records `target_model_source` as stable metadata. Asset
pickers resolve that ID to the PMX source's current display name and show
`<motion source> / <clip> - <target model>`. Display-name changes never alter
clip identity or Animation Set references.

Target order is not semantic. Reordering `motion_model_sources` does not
change derived IDs or source fingerprints. Adding, removing, or changing a
target updates only that target's derived clip and the aggregate source
dependency fingerprint.

The old singular `motion_model_source` remains a read-only compatibility
field. On reimport, its old `(source, animation index)` clip ID is retained as
a hidden catalog alias for the same target-specific clip. Existing Animation
Sets continue to resolve it, but new pickers expose only canonical
target-specific IDs. If the author removes that legacy target, its alias is
removed and old references become explicitly missing rather than silently
changing to a different skeleton.

## Consequences

- One VMD file is registered once and can supply independently selectable
  clips for several PMX characters.
- A motion distributed with its authoring PMX can preserve that model's MMD
  constraint result before adapting it to other output skeletons.
- Changing a PMX display name updates picker labels without rewriting IDs.
- Runtime clip lookup uses the full imported sub-asset ID rather than a
  source-local numeric animation index, because several target clips can all
  have index zero and the same human-readable clip name.
- Every selected PMX path is a source dependency. Reimporting any target PMX
  requeues the VMD and refreshes all of its derived target clips.
- Existing singular pairings and old Animation Set references remain valid.

## Alternatives Considered

### Put the target model name into the persisted clip name

Rejected because renaming a model would either leave a stale label or rewrite
derived catalog data. Names also do not provide stable uniqueness.

### Register the same VMD file once per target PMX

Rejected because the manifest deliberately prevents duplicate source paths,
and copying the same motion file creates redundant authoring assets solely to
obtain different bake settings.
