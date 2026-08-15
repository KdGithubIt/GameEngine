# ADR 0055: Render Authoring and Material v2 Contract

Status: Accepted
Date: 2026-07-13

## Context

The renderer already supports static and skinned meshes, GPU instancing, LOD,
particles, directional shadows, environment-lighting intent, and HDR
post-processing. Most of those features are runtime-only, however. Material
assets are persisted but scene conversion ignores their files, texture
references, roughness, and metallic values. Environment and post-process editor
state is transient. This makes the normal Scene -> Editor Play -> packaged
player path materially different from isolated runtime examples.

Editor Ready v1 needs one authoring contract without serializing GPU handles or
letting editor-only state become the runtime authority.

## Decision

1. Artistic render settings are scene-owned. The component registry adds stable
   authoring IDs for `engine.shadow_settings`,
   `engine.environment_lighting`, `engine.post_process`,
   `engine.skinned_mesh_source`, and `engine.lod_group`. The first three map to
   the existing runtime resources during scene conversion. A scene may contain
   at most one of each settings component; duplicates produce diagnostics and
   deterministic last-in-authoring-order replacement is forbidden.
2. Device capabilities and quality ceilings remain project/device-owned. GPU
   limits, fallback formats, and default quality tiers are not copied into scene
   entities. Authoring validation reports when scene content exceeds the active
   renderer contract before a draw or upload is attempted.
3. `engine.skinned_mesh_source` stores a registered glTF/GLB `AssetRef` plus
   deterministic mesh and skin selectors. Scene conversion imports the source,
   creates runtime mesh and skeleton state, and attaches `SkinnedMesh` and
   `JointPalette`. Joint entities and runtime handles remain transient as
   required by ADR 0043.
4. `engine.lod_group` stores an ordered array of levels. Each level contains a
   positive distance and a mesh `AssetRef`. Distances must be strictly
   increasing. Conversion resolves and caches every mesh and attaches the
   existing runtime `LodGroup`; no runtime asset ID is serialized.
5. Material schema v2 is a backward-compatible extension of
   `*.material.json`. It keeps v1 defaults and adds optional normal/emissive
   texture references, emissive color, alpha mode/cutoff, cull mode, and a
   supported shading model enum. Unknown enum values, non-finite values,
   out-of-range scalar/color values, missing texture references, and wrong
   texture categories are diagnostics. Reading schema v1 supplies v2 defaults;
   writing uses schema v2.
6. Texture source files remain registered manifest assets. Scene conversion
   parses and validates material documents and image data on the CPU. GPU
   texture creation is deferred to the renderer's preparation boundary, where
   the device and queue already exist. This keeps headless validation and
   packaged loading deterministic while avoiding GPU objects in authoring data.
7. Missing or invalid non-blocking mesh, material, or texture content uses a
   visible diagnostic fallback: unit cube for mesh, magenta checker for
   material/texture. A missing asset must never silently become the unrelated
   built-in triangle.
8. Scene View and Editor Play consume the same authored scene settings and
   material assets. Preview controls may restart particles or force an LOD for
   inspection, but preview overrides are transient and never saved into scene
   semantics unless the user edits the component.

## Compatibility and Migration

- Existing material v1 files load unchanged with opaque, back-face-culling,
  lit defaults and no additional textures.
- Existing scene files remain valid because all new components are opt-in.
- The stable IDs above must not be renamed after scene files use them.
- ADRs 0036, 0040, 0042, 0043, and 0044 continue to define runtime behavior;
  this ADR adds the previously deferred authoring and persistence path.

## Consequences

- Material parsing, CPU image decoding, and GPU upload become separate stages.
- Headless tests can verify content and package dependencies without a GPU.
- Scene switching replaces artistic render resources along with the scene,
  preventing project-wide settings from leaking between levels.
- A later physically based renderer may consume the persisted roughness,
  metallic, normal, and emissive inputs without changing material asset IDs.

## Verification

- Material v1/v2 round trips, invalid enum/range checks, texture dependency
  checks, reimport, and visible fallback are tested.
- Every new component is covered by command undo, canonical save/reopen, scene
  conversion, runtime execution, and package-copy tests.
- One static environment and one skinned character are compared across Scene
  View, Editor Play, and packaged-player render captures.
