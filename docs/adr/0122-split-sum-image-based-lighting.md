# ADR 0122: Split-Sum Image-Based Lighting

Status: Accepted
Date: 2026-08-16
Relates to: ADR 0036, ADR 0100, ADR 0118, ADR 0119, ADR 0120

## Context

ADR 0036 established `EnvironmentLighting` but deliberately deferred specular
IBL, BRDF integration, and prefiltered environment maps. The current renderer
therefore collapses enabled environment lighting into an `AmbientLight` color.
That cannot represent directional diffuse irradiance, roughness-dependent
reflections, metallic energy response, or the split-sum BRDF used by
`StandardLit`.

ADR 0119 now gives static and skinned meshes one authoritative material
fragment stage, and ADR 0120 gives `StandardLit` generic roughness, metallic,
normal, and occlusion inputs. This is the correct point to add IBL once at the
shared lighting layer rather than creating format-specific material paths.

## Decision

### 1. Environment source and displayed sky remain separate

The renderer accepts one runtime `EnvironmentMap` resource as an
equirectangular, scene-linear HDR lighting source. `EnvironmentLighting`
continues to own the scene policy and intensity/tint controls. The sky pass is
not implicitly replaced by the environment map and may continue to render its
own procedural or future authored background.

This preserves ADR 0118's rule that background presentation and environment
lighting are separate responsibilities even when a caller intentionally gives
them the same source image.

`EnvironmentMap` is a resolved runtime resource rather than a new serialized
field. It can be built from linear HDR RGB or from sRGB RGBA8 source pixels.
Asset-authoring code may resolve persisted asset intent into this resource
without changing the renderer's preprocessing contract.

### 2. The renderer derives the lighting products once per source

When a new `EnvironmentMap` identity becomes active, the renderer performs a
one-time CPU preprocessing step and uploads:

- a 32x16 diffuse irradiance equirectangular texture;
- a 128x64 GGX-prefiltered specular equirectangular texture with eight
  roughness mip levels; and
- one renderer-owned 64x64 split-sum BRDF lookup texture.

Environment products use `Rgba16Float`; the BRDF LUT uses `Rg16Float`.
Preprocessing stays in linear scene color, so HDR values are not quantized
through material RGBA8 storage. The source identity is cached by the
`WorldRenderer`; ordinary frames only sample the already-uploaded products.

The first specular mip is the sharp environment. Increasing mip level maps
monotonically to increasing GGX roughness. The shader selects
`roughness * max_specular_lod`, rather than treating an ordinary box-filtered
image mip chain as a physically prefiltered environment.

### 3. StandardLit uses split-sum IBL

`StandardLit` keeps its existing direct directional GGX BRDF. Its indirect
term additionally evaluates:

- Lambertian diffuse from the irradiance map;
- metallic-aware diffuse energy reduction;
- reflection-direction sampling from the GGX-prefiltered specular map;
- the BRDF LUT scale/bias term using NdotV and roughness; and
- the existing environment tint and non-negative intensity multiplier.

Ambient occlusion continues to multiply only the combined indirect term, as
required by ADR 0120. It does not darken direct light or emissive output.
`ToonLit`, `Unlit`, and the independent Outline pass do not sample IBL.

### 4. Existing fallback behavior remains valid without a resolved map

The persisted `diffuse_ibl_enabled` switch remains the enable gate in this
phase so serialized component shape does not change. When it is enabled but no
resolved `EnvironmentMap` resource exists, the renderer retains the existing
`diffuse_color` ambient fallback instead of sampling placeholder textures.

When a resolved map is present, ordinary `AmbientLight` remains a separate
light contribution and environment lighting is evaluated independently. This
removes the previous need to overwrite the ambient-light color merely to make
environment intent visible.

The existing `skybox` and `diffuse_irradiance` runtime asset-ID fields remain
asset-resolution intent. This ADR does not silently reinterpret either ID as
an unconvolved specular source; authoring/asset resolution may bind those
contracts explicitly in a later change.

### 5. Bind-group and source-format contracts stay generic

IBL textures extend the existing light bind group used by the shared static
and skinned material fragment stage. The skinned joint-palette group and
material group numbers do not move. No PMX, MMD, glTF, FBX, or original-engine
format receives a private shader path.

## Consequences

- `StandardLit` gains directional diffuse and roughness-dependent specular
  environment response without a material-schema change.
- Existing scenes with no runtime `EnvironmentMap` keep their prior ambient
  environment fallback.
- HDR environment values survive preprocessing and GPU upload.
- Static and skinned surfaces consume exactly the same IBL implementation.
- Environment preprocessing is deterministic for one source and sampling
  configuration, though this ADR does not promise cross-platform bit-exact
  floating-point texture contents.
- Authoring UI/asset binding for an unconvolved environment source remains a
  separate responsibility; the renderer no longer needs another shading
  redesign when that binding is added.

## Rejected alternatives

### Use the ordinary material mip generator as the specular prefilter

Rejected. Box-filtered color mips do not integrate the GGX normal distribution
and therefore cannot represent roughness-dependent specular IBL correctly.

### Couple IBL directly to the visible sky pass

Rejected. A scene may intentionally light from one source while displaying a
different background, and ADR 0118 explicitly keeps those responsibilities
separate.

### Add a source-format-specific environment path

Rejected. Importers and authoring code resolve pixels and asset intent;
`StandardLit` consumes one renderer-owned, format-independent IBL contract.
