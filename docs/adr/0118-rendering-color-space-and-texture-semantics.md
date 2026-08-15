# ADR 0118: Rendering Color-Space and Texture-Semantics Contract

Status: Accepted

## Context

The renderer already has the core pieces of a physically coherent color path:
color textures are uploaded through sRGB formats, normal textures use linear
UNORM storage, material color values are represented by `LinearRgba`, scene
lighting renders into `Rgba16Float`, and the final fullscreen pass performs
exposure and tone mapping.

Those rules are currently distributed across importer, runtime, shader, and
surface code rather than stated as one invariant. In particular, material
texture preparation selected sRGB versus linear sampling with an unnamed
boolean, procedural color textures could write scene-linear values directly
into byte storage that would later be sRGB-decoded, and the tone-map pass
implicitly relied on an sRGB swapchain format even though the surface layer can
fall back to a non-sRGB format.

The renderer is expected to grow additional StandardLit textures, image-based
lighting, stylized shading, temporal effects, and post-processing. Those
features need one color-space contract before more texture slots or passes are
added.

## Decision

### Scene color space

The renderer uses **linear RGB scene values** for material arithmetic,
lighting, environment contributions, emissive values, HDR intermediate
targets, and render-pass composition.

The normal SDR presentation path is:

```text
source color texture
  -> sRGB decode
  -> linear scene values
  -> lighting / shading
  -> linear Rgba16Float HDR scene color
  -> exposure
  -> tone mapping
  -> artistic display-linear grading
  -> sRGB output transfer
  -> presentation surface
```

There must be exactly one sRGB output transfer. When the swapchain format is an
sRGB format, the render target performs that transfer. When the selected
presentation format does not perform sRGB encoding, the final presentation
shader performs it explicitly.

Tone mapping and the artistic `grading_gamma` control are not substitutes for
the output transfer.

### Material texture semantics

Texture slots declare whether stored RGB bytes represent color or numeric data.

**sRGB color textures**:
- Base Color
- Emissive
- Toon Ramp
- Sphere Map / Matcap

These are uploaded with an sRGB texture format. Sampling returns linear RGB to
the shader.

**linear data textures**:
- Normal
- future Metallic/Roughness
- future Occlusion/AO
- other future masks or packed numeric channels unless their contract says
  otherwise

These bypass the sRGB transfer function.

The runtime material preparation boundary must name this distinction. An
unnamed boolean is not sufficient because future material slots must be
reviewable against the contract.

### Numeric material colors

`LinearRgba` and runtime material color arrays are scene-linear values.
Emissive RGB is linear HDR and may exceed one where the authoring contract
allows it. Editor controls editing these values do not introduce an additional
sRGB conversion into persisted values.

### Procedural textures

A producer that creates byte-backed **color** textures from scene-linear color
values must apply the sRGB source transfer before quantizing the RGB bytes.
Alpha remains linear coverage data. A producer of numeric/data textures must
not apply that transfer.

This rule applies to every producer. PMX shared-toon synthesis is one current
producer affected by it; the rule itself is not MMD-specific.

### HDR and pass composition

The main scene color target remains `Rgba16Float` and linear. Independent
render passes that composite into scene color, including Outline and future
Bloom/Temporal passes, operate on values consistent with their declared
intermediate formats and must not silently apply presentation gamma.

Skybox/display background and environment-lighting source remain separate
responsibilities even when they use the same source image in a future phase.

## Consequences

- Existing serialized material schema and Stable IDs do not change in this
  phase.
- Existing StandardLit, ToonLit, Unlit, Outline, MSAA, transparency, and CSM
  architecture remains intact.
- Adding Metallic/Roughness/Occlusion textures later has a defined linear-data
  path instead of inheriting Base Color's sRGB behavior by accident.
- Procedural color textures become consistent with file-decoded color
  textures.
- Non-sRGB presentation fallback no longer skips the SDR output transfer.
- Full HDR display color management, wide-gamut output transforms, LUT color
  management, and automatic exposure remain future work.

## Rejected alternatives

### Apply gamma in each material shader

Rejected. Gamma is an output/encoding concern, not a lighting operation.
Applying it before or during scene-linear lighting breaks energy relationships
and compounds as passes are added.

### Treat every RGBA texture as sRGB

Rejected. Normal maps and packed PBR channels contain numeric data and are
corrupted by sRGB decoding.

### Treat every RGBA texture as linear

Rejected. Ordinary authored color images are conventionally sRGB-encoded and
would shade too dark if sampled as numeric UNORM values.

### Add PMX-specific color compensation

Rejected. The bug is the generic producer/consumer encoding contract. PMX
shared-toon synthesis must obey the same color-texture rule as any future
procedural color texture.

## Follow-up

The next renderer-foundation phase should extend the generic material contract
with Metallic/Roughness/Occlusion texture semantics, promote retained mesh
tangents and handedness into the GPU vertex contract with MikkTSpace-compatible
fallback generation, and share surface-shading WGSL between static and skinned
mesh paths before adding IBL and broader lighting.
