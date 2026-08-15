# FBX asset import

The engine imports `.fbx` files directly, via the `ufbx` library (ADR 0081).
Copy an FBX source below the project's `assets/` directory and use
**Register Asset** in the Asset Browser exactly like a `.gltf` or `.glb`
source — no conversion step is required for the common case (mesh, skin,
inverse bind poses, animation clips, embedded or sidecar textures).

Direct import normalizes the source to engine conventions automatically:
units are converted to meters, axes to right-handed +Y up, and any pivot,
`PreRotation`, or `PostRotation` transform is baked into a plain per-node
transform. Animation clips (one per FBX animation stack) are resampled to
linear keyframes at the file's declared frame rate (30 Hz when the file does
not declare one). None of this requires authoring-side configuration.

## Reimport

Use **Reimport** after re-exporting a registered source from the DCC tool.
Reimport derives sub-asset IDs from the registered source ID and the
original FBX selector (the element's position in the file's own node / mesh /
skin / material / animation lists), so existing mesh, material, skin, and
clip references remain stable as long as that selector order is preserved
across exports.

## Switching a source's format is a re-registration, not a reimport

A model's sub-asset IDs are derived in part from its *original selector* in
the source document (ADR 0081 §5). An FBX file and a glTF/GLB export of the
same model do not share selectors, so replacing a registered FBX source with
a glTF/GLB export of the same model (or vice versa) produces a different set
of sub-asset IDs — every reference to the old sub-assets (mesh, material,
skin, clip) breaks. Treat a format switch as registering a new source, not as
reimporting the existing one: re-point scene/prefab references at the new
sub-asset IDs after registration, the same as replacing a source with an
unrelated model.

## What direct FBX import does not cover

`ufbx` parses the FBX semantic layer (units, axes, pivots, skinning,
animation) but the importer does not attempt every exotic authoring feature
FBX can express — blend shapes / morph targets, NURBS surfaces, multi-take
layered animation blending, and vendor-specific shader graphs are not
imported. If a source relies on one of these, the DCC round trip to glTF
described below remains the documented fallback; the glTF exporter in most
DCC tools either bakes these features down to something the engine's glTF
importer already supports (e.g. blend shapes baked into vertex animation
clips) or drops them with an explicit warning at export time, which is
easier to act on than a silent gap in the direct FBX path.

### Fallback: convert to glTF in the DCC tool

Convert the source to glTF 2.0 (`.gltf` plus sidecars) or binary glTF
(`.glb`) in the DCC tool before copying it below the project's `assets/`
directory. Keep mesh normals, tangents, UVs, skin weights, inverse bind
poses, animation clips, and material textures enabled during export. Prefer
`.glb` when a single-file handoff is useful; prefer `.gltf` when texture and
buffer files need to remain individually inspectable.

After conversion, use **Register Asset** once and **Reimport** after
subsequent exports, exactly as described above for a direct FBX
registration — the same selector-stability and format-switch rules apply.
