# ADR 0120: Generic PBR Material Texture Contract

Status: Accepted
Date: 2026-08-16
Relates to: ADR 0100, ADR 0101, ADR 0115, ADR 0118, ADR 0119

## Context

`StandardLit` already carries scalar roughness and metallic values, while the
import boundary can preserve richer source material data from glTF, FBX, PMX,
and future formats. The renderer needs one format-independent contract for the
remaining physically based texture inputs without creating a glTF-specific or
MMD-specific shading path.

The contract must also preserve ADR 0118's color-space rules and ADR 0100's
separation between `StandardLit`, `ToonLit`, `Unlit`, and the independent
outline pass.

## Decision

### 1. Material schema v3 owns the generic PBR texture inputs

The canonical material fields are:

- `metallic_roughness_texture`: optional packed linear-data texture where
  green is roughness and blue is metallic;
- `occlusion_texture`: optional linear-data texture where red is ambient
  occlusion;
- `normal_scale`: finite scale applied to tangent-space normal-map X/Y before
  normalization;
- `occlusion_strength`: normalized `[0, 1]` strength applied to the sampled
  ambient-occlusion value.

Scalar `roughness` and `metallic` remain part of the material contract. A
packed texture multiplies those scalar factors rather than replacing them.

### 2. Texture color-space semantics follow data meaning

Metallic/roughness, ambient occlusion, and normal maps are linear data and MUST
use linear GPU texture formats. Base color, emissive, toon-ramp, and
sphere/matcap textures remain sRGB color textures under ADR 0118.

Missing metallic/roughness and occlusion textures resolve to white data. This
keeps the scalar roughness/metallic values unchanged and represents full
ambient visibility respectively.

### 3. Occlusion affects only StandardLit's indirect lighting term

Ambient occlusion modulates the indirect/ambient contribution of
`StandardLit`. It does not darken direct directional-light BRDF evaluation,
shadow visibility, emissive output, `ToonLit`, or `Unlit`.

This boundary is intentional so later image-based lighting can replace the
current ambient approximation without changing material semantics: occlusion
continues to belong to indirect lighting rather than direct-light visibility.

### 4. Normal scale is tangent-space data, not a shading-model switch

`normal_scale` scales sampled tangent-space X/Y components before the normal is
renormalized. The tangent basis itself follows ADR 0119, including authored
tangents when available and derivative fallback when a valid tangent frame is
absent.

### 5. Importers normalize into the shared contract

glTF maps its metallic-roughness texture, occlusion texture, normal scale, and
occlusion strength directly into the format-neutral IR semantics above. FBX,
PMX, original engine assets, and future importers populate the same fields when
their source data can express them and otherwise use the generic defaults.

No source format receives a private runtime material type or renderer pass.

### 6. Schema v3 is current-format-only

Per ADR 0115, `.material.json` readers accept only the current schema version.
The v2 representation is not silently inferred or migrated by the runtime.
Current optional texture slots remain optional because absence is part of the
v3 writer contract, not a compatibility path.

## Consequences

Generic model import can preserve common PBR texture inputs through authoring,
runtime texture resolution, and `StandardLit` without coupling the renderer to
one source format. Texture encoding is unambiguous, scalar factors remain
useful with or without textures, and later IBL work can consume the same
occlusion semantics without another material-schema change.

`ToonLit`, `Unlit`, and outline behavior remain independent. Projects carrying
schema-v2 material documents must regenerate or update them to schema v3 before
loading them with this engine revision.
