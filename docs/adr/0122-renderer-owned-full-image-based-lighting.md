# ADR 0122: Renderer-Owned Full Image-Based Lighting

Status: Accepted
Date: 2026-08-16
Relates to: ADR 0036, ADR 0100, ADR 0118, ADR 0119, ADR 0120

## Context

ADR 0036 introduced `EnvironmentLighting` with runtime `skybox` and
`diffuse_irradiance` texture identifiers, but the first renderer only replaced
the ambient-light color with `diffuse_color`. It explicitly deferred specular
split-sum IBL, a BRDF integration LUT, and prefiltered environment radiance.

The renderer now has the prerequisites that were missing at that time:

- linear HDR scene color and an explicit output-transfer contract (ADR 0118);
- a stable tangent-space GPU contract and one authoritative material fragment
  stage for static and skinned meshes (ADR 0119); and
- a generic StandardLit material contract with metallic/roughness, normal, and
  ambient-occlusion inputs (ADR 0120).

Leaving environment lighting as an ambient replacement is now the largest gap
in StandardLit's indirect-lighting model. Adding format-specific MMD, glTF, or
FBX lighting paths would violate the generic material boundary, while storing
prefiltered renderer artifacts in authoring data would expose backend details
as project contracts.

## Decision

### 1. Full IBL remains a generic StandardLit renderer responsibility

`StandardLit` consumes the same environment-lighting path for every importer.
No MMD-, glTF-, or FBX-specific shader or material field is introduced.

The indirect term is split into:

- diffuse irradiance;
- GGX-prefiltered specular radiance; and
- a split-sum BRDF integration term.

ADR 0120's ambient-occlusion rule remains unchanged: AO modulates the complete
StandardLit indirect contribution and does not modify direct directional
lighting, shadows, emissive output, ToonLit, or Unlit.

### 2. Existing runtime texture identifiers are the source contract

No persisted scene, material, Stable ID, or importer schema changes are
required.

`EnvironmentLighting::skybox` is the equirectangular environment-radiance
source. When it resolves to a runtime texture, it supplies the visible sky and
specular IBL.

`EnvironmentLighting::diffuse_irradiance` remains an optional pre-convolved
Lambert-normalized irradiance override. When it is absent and a skybox exists,
the renderer derives diffuse irradiance from the skybox.

`diffuse_ibl_enabled` continues to gate only the diffuse environment term. A
resolved skybox supplies specular IBL independently because sky radiance and
reflections are two views of the same environment source. `intensity` scales
both environment lighting and the texture-backed sky.

When diffuse IBL is enabled but neither a diffuse-irradiance texture nor a
skybox is available, the existing `diffuse_color * ambient_intensity *
intensity` behavior is preserved as the fallback.

### 3. Derived IBL textures are renderer-owned and process-local

The render runtime derives and caches GPU resources from the resolved skybox:

- a low-resolution Lambert-normalized diffuse irradiance map;
- an equirectangular GGX-prefiltered specular map whose mip levels correspond
  from smooth to rough surfaces; and
- a two-channel split-sum BRDF integration LUT generated once per renderer.

These resources use linear `Rgba16Float` storage. Source color textures are
sampled through their existing sRGB view, so hardware decoding places source
radiance in linear space before convolution. Derived textures are never
serialized, assigned Stable IDs, or exposed as importer output.

The cache key is runtime texture identity. Replacing the skybox texture rebuilds
its derived irradiance and prefilter resources; unchanged textures do not pay
that cost every frame.

### 4. Equirectangular sampling is the runtime environment convention

The current generic `Texture` asset is two-dimensional, so environment sources
and derived maps use latitude-longitude/equirectangular addressing. The
renderer owns direction-to-UV conversion and uses horizontal repeat with
vertical clamp.

A future cubemap asset representation may replace the storage convention
without changing StandardLit's material schema or importer contracts. Such a
change must preserve the environment-lighting semantics in this ADR.

### 5. The procedural sky remains the no-texture fallback

When `skybox` resolves, the sky pass samples that radiance source using the same
world-space view direction as IBL. Otherwise the existing procedural
zenith/horizon/ground gradient remains available through `SkySettings`.

A resolved skybox is sufficient to draw the sky even when the procedural
`SkySettings::enabled` flag is false. This avoids requiring two independent
enable switches for one runtime environment texture.

## Consequences

- StandardLit gains diffuse and specular environment lighting without changing
  Material schema v3 or any importer.
- Metallic surfaces can receive indirect illumination even when directional
  and legacy ambient lighting are zero.
- Static and skinned meshes remain on the same authoritative fragment stage.
- Environment preprocessing costs occur only when the runtime skybox identity
  changes, not per draw or per frame.
- The current generic texture pipeline still supplies 8-bit color sources;
  adding HDR environment file decoding is an asset-pipeline enhancement, not a
  change to the IBL shader contract.
- Runtime environment IDs remain process-local and are not persisted into
  project files.

## Alternatives Considered

### Keep ambient replacement and defer specular IBL again

Rejected because the material and HDR foundations now exist, and StandardLit
metallic materials otherwise have no physically meaningful indirect response.

### Add prefiltered environment and BRDF-LUT fields to Material

Rejected because those are scene/environment resources, not per-material
semantic inputs. It would also duplicate renderer artifacts across materials.

### Persist irradiance, prefiltered specular, and BRDF LUT as authoring assets

Rejected for the initial implementation. They are deterministic backend-derived
resources and would unnecessarily couple project data to one renderer storage
strategy.

### Evaluate many GGX environment samples in every material fragment

Rejected because convolution cost belongs at environment-change time. The main
pass should perform a small fixed number of texture reads regardless of scene
mesh count.

## Compatibility and Migration

No serialized authoring format changes are introduced. Existing scenes with no
runtime environment texture preserve their current ambient and procedural-sky
behavior. Existing code that provides a `skybox` runtime texture begins using
that texture for the visible sky and StandardLit specular IBL, which is the
previously deferred meaning of that field from ADR 0036.
