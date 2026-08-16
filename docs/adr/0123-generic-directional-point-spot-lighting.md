# ADR 0123: Generic Directional, Point, and Spot Direct Lighting

Status: Accepted

## Context

The renderer already has a generic StandardLit PBR path, one scene directional
light with cascaded shadows, and renderer-owned image-based lighting. The next
renderer roadmap step needs finite local lights without creating importer-
specific render paths or weakening the existing indirect-lighting contract.

Point and spot lights are also authoring data: they need stable component IDs,
editor-discoverable schemas, deterministic runtime budgets, and a clear transform
contract. Point/spot shadows remain outside the existing ADR 0036 shadow scope.

## Decision

### Authoring components

Two additive engine-owned component IDs are stable contracts:

- `engine.point_light`
- `engine.spot_light`

Both use the entity `GlobalTransform` for runtime placement. Point lights use
translation only. Spot lights use translation plus the transformed local `-Z`
axis as the light-travel direction.

Point Light fields are `color_r`, `color_g`, `color_b`, `intensity`, and
`range`. Spot Light adds `inner_angle_degrees` and `outer_angle_degrees`, with
`0 <= inner < outer < 90`.

Existing `engine.directional_light` remains an explicit direction-valued
component and is not migrated to transform-derived orientation.

### Stable renderer budgets

Editor Ready v1 evaluates at most:

- 1 directional light,
- 16 point lights,
- 8 spot lights, and
- 1 ambient light.

Scene validation reports over-budget light counts before GPU preparation. The
runtime renderer uses ascending runtime entity identity for deterministic local-
light selection, which preserves scene spawn order for normal authoring
conversion while also defining programmatic runtime behavior.

### Direct-light model

StandardLit evaluates all selected direct lights with the same Cook-Torrance
BRDF. Directional radiance remains `color * intensity`.

Point and spot lights use inverse-square attenuation with a smooth finite-range
cutoff:

`attenuation = max(1 - (distance / range)^4, 0)^2 / max(distance^2, 0.01)`

Spot lights multiply that distance term by a smooth cone factor between the
outer and inner half-angle cosines.

Local lights affect direct lighting only. Ambient, diffuse irradiance, GGX
environment specular, BRDF LUT evaluation, and material occlusion keep the ADR
0120/0122 indirect-light contract unchanged.

### Shadows and ToonLit boundary

Only the existing primary directional light participates in CSM. Point and spot
shadows are not implemented by this ADR and remain a later shadow-system
decision.

Generic ToonLit keeps its existing primary-directional-light behavior in this
phase. Its multi-light artistic response belongs to the later Generic ToonLit
quality phase so this change does not silently alter established MMD/toon
appearance.

### GPU resource contract

Local lights are fixed-size vec4-packed arrays inside the existing scene-light
uniform at mesh bind group 2. No new mesh bind group is introduced, so the
portable four-bind-group floor established by ADR 0122 remains intact.

## Consequences

- glTF, FBX, PMX, procedural meshes, static meshes, and skinned meshes share the
  same StandardLit local-light path.
- Existing scenes and serialized components remain valid because the two
  component IDs are additive.
- The forward pass has a bounded per-pixel local-light loop. A later renderer
  architecture phase may replace this with tiled/clustered culling without
  changing the authoring component contract.
- Point/spot shadowing and ToonLit local-light styling remain explicit future
  work rather than hidden partial behavior.

## Related decisions

- ADR 0036: directional shadows and environment-lighting boundary
- ADR 0055: render authoring and renderer budgets
- ADR 0100: generic shading-model boundary
- ADR 0120: generic PBR material texture contract
- ADR 0122: renderer-owned full image-based lighting
