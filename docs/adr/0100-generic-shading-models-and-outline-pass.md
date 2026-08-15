# ADR 0100: Generic Shading Models and Outline Pass

- Status: Accepted
- Date: 2026-08-13
- Amends: ADR 0029, ADR 0036, ADR 0040, and ADR 0097

## Context

The built-in material contract previously distinguished only lit and unlit
surfaces. PMX import consequently discarded toon ramps, sphere maps, edge
settings, and per-vertex edge scale even though those concepts are useful to
non-MMD stylized rendering as well. Shadow depth already existed as a separate
pass, while bloom did not consume its authored radius and there was no color
grading stage.

## Decision

The renderer distinguishes surface shading from render passes:

- `StandardLit`, `ToonLit`, and `Unlit` are shading models evaluated by the
  main material pass.
- Shadow depth and screen-space outline classification are independent passes.
  Both consume the same uploaded vertex positions and joint palettes as the
  main pass.
- Outline enablement, color, and material width remain material properties.
  Width is an object-space reference multiplied by the generic vertex outline
  scale, projected at each vertex, normalized against a 1024-texel reference
  height, and capped to four reference texels. The composite scales that radius
  by the actual mask density so output-space width does not change with mask
  resolution.
- Cast-shadow and receive-shadow controls are independent render-state values.

The version-2 material JSON contract receives backward-compatible optional
`toon`, `outline`, `cast_shadow`, and `receive_shadow` fields. The old serialized
`lit` shading-model spelling remains a read alias for `standard_lit`. Standard
roughness and metallic fields remain physically present for compatibility but
are evaluated only by `StandardLit`.

Toon materials support an optional ramp, shadow and ambient colors, stylized
specular and rim terms, and an optional sphere map. Sphere compositing
(`multiply` or `add`) is represented separately from coordinates (`view_normal`
or `additional_uv0`). PMX mode 3 therefore maps to additional-UV coordinates;
it is not interpreted as subtraction.

PMX import maps its material data into this generic contract. Alpha mode is an
importer policy derived from diffuse alpha and decoded base-texture alpha. A
material whose base texture is non-opaque over only a small share of the UVs
its own submesh vertices sample is a cutout and becomes `mask`, which keeps
depth writes; one that is non-opaque across most of its own surface is a
translucent layer and becomes `blend`. The distinction is not cosmetic: a
`blend` surface writes no depth and so is absent from every later
depth-tested draw. PMX edge size is converted to engine units and multiplied
by each imported vertex's edge scale.

The outline pass renders visible surfaces at their real positions into a mask
derived from the physical render viewport. The mask tracks native viewport
width and height while they fit the quality budget, then uniformly downscales
to preserve aspect while respecting `device.limits().max_texture_dimension_2d`,
a 2560x1440 texel budget, and a maximum height of 2048 texels (twice the width
reference density). Thus 1080p and 1440p viewports classify at native
resolution, while a 3840x2160 viewport classifies at 2560x1440 instead of
allocating an unconditional 4K mask. One attachment stores linear outline color
and normalized projected radius, a second stores a transient runtime hierarchy
group, and an independent depth attachment retains the nearest surface.
Alpha-mask materials apply their authored cutoff; fully transparent blend
texels do not occupy the mask. When any outline is enabled, outline-disabled
geometry still enters the group and depth attachments as an occluder, but
contributes zero radius.

The fullscreen composite emits a full outline when a nearby source has a
non-zero radius and its hierarchy group differs from the destination group.
Static render entities use their topmost reachable `Parent` ancestor. Skinned
render entities start from their `SkinnedMesh` rig and then use that rig's
topmost ancestor. The group is a frame-local runtime entity ID and is neither
serialized nor exposed through the material contract. These rules suppress
material seams and overlapping mesh parts within one character while retaining
silhouettes and boundaries between independent roots. Invalid parent cycles
degrade deterministically to the lowest runtime entity ID in the cycle.

The composite converts the four-reference-texel cap to mask texels using
`mask_height / 1024`. The mask-height cap therefore limits the search radius to
eight mask texels even for tall or narrow viewports. This density conversion
keeps the final apparent outline width stable across viewport resolution while
allowing finer silhouette classification at higher resolutions. The
classification attachments remain single-sampled; MSAA is an independent
rendering decision.

Materials may opt into same-root boundaries with
`outline.internal_boundary_strength` in the normalized `0.0..=1.0` range.
The default is `0.0`, so existing materials retain seam suppression. A
non-zero value permits an edge only against a different material identity,
scaling both search radius and opacity; it never outlines the interior of one
continuous material. PMX has no skin semantic, so its importer assigns `0.55`
only to conventional body material names containing `body`, `skin`, or `肌`.
Authors can override the value after extracting a material.

Post processing keeps tone mapping, consumes bloom radius, and adds optional
tint, saturation, contrast, and gamma color grading after tone mapping.

## Consequences

- MMD assets no longer need a dedicated shader to retain their principal
  surface semantics.
- Ordinary stylized assets can use the same toon, sphere-map, and outline
  features.
- Existing version-2 material documents continue to load with defaults.
- The built-in vertex layout gains outline scale and additional UV0 data.
- Outline width retains its object-space authoring meaning and therefore scales
  with the model transform, but rasterization is screen-space and cannot create
  inverted-hull seams between surfaces in the same hierarchy group.
- The four-reference-texel width cap still intentionally saturates very large
  authored widths. Fine silhouette detail follows physical viewport resolution
  until the bounded mask quality budget is reached.
- Scene View and Game View resizing recreates outline attachments when the
  selected mask extent changes. Resizes above the quality ceiling can reuse the
  same capped extent when their resulting mask dimensions are unchanged.
- Frames with no visible outline-enabled material skip mask allocation and
  rendering work. Once an outline is active, all visible geometry participates
  so occlusion and same-root suppression remain correct.
- Bloom remains the existing compact nine-tap implementation; its authored
  radius now controls sample spacing rather than being ignored.

## Alternatives Considered

### Add an MMD-only shader and render path

Rejected because it would duplicate skinning, shadows, material loading, and
editor behavior while preventing non-MMD content from reusing the features.

### Treat outline as part of ToonLit

Rejected because unlit and standard-lit surfaces can also need outlines, and
the operation is a separate draw with different culling and depth state.

### Keep inverted-hull extrusion and tune imported widths

Rejected because width tuning cannot reliably distinguish a true silhouette
from boundaries created by separate hair, clothing, and body meshes. It also
leaves the failure dependent on model topology instead of providing a generic
hierarchy-level suppression rule.
