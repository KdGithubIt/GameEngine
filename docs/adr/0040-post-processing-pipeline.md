# ADR 0040: Post-Processing Pipeline and HDR Target

Status: Accepted
Date: 2026-06-14
Target Phase: Phase 45

## Context

The renderer currently writes directly to the swapchain format. Phase 45 adds a
post-processing contract so tone mapping, exposure, and bloom can be controlled
as engine settings and later moved into a full offscreen HDR pipeline.

This decision must define the HDR target format and post-process settings before
renderer code begins to depend on them.

## Decision

The Phase 45 HDR contract uses `Rgba16Float` as the offscreen render target
format. The public settings surface is:

- `PostProcessSettings`
- `ToneMapOperator`
- `BloomSettings`
- `HdrRenderTargetFormat`

ACES fitted tone mapping is the default operator. Reinhard remains available for
simple previews and tests. Bloom settings are part of the resource contract even
when a specific GPU bloom pass is disabled.

The first implementation may expose CPU-side tone mapping helpers and settings
resources before the renderer is fully converted to an offscreen target. The
resource contract is the compatibility boundary for the later GPU pass.

### GPU pass ownership

The runtime GPU implementation keeps post-processing as an explicit renderer-owned
pass chain rather than embedding screen-space effects in material shaders. Bloom
runs entirely in linear HDR before exposure and tone mapping:

```text
scene HDR
  -> bright extraction / bounded downsample pyramid
  -> filtered upsample
  -> HDR bloom composite
  -> exposure / tone mapping / color grading
  -> output transfer
```

The multi-resolution bloom implementation reuses the existing `BloomSettings`
fields. Resizing the source HDR target recreates renderer-owned bloom targets;
callers do not provide or persist bloom textures.

White-balance and LUT-based grading, if added later, belong to this same
post-process stage. This refinement does not add persisted controls, LUT asset
references, Stable IDs, or a post-process graph; those contracts require a
separate authoring/asset decision before they are introduced.

## Consequences

- Gameplay/editor code can configure exposure, tone mapping, and bloom through a
  stable resource.
- Bloom filtering and composition stay in renderer-owned HDR passes, independent
  from material shading and the caller-owned swapchain target.
- The existing settings and scene-authoring contract remain stable while the GPU
  implementation can improve internally.
- Any future change to the HDR format or default tone mapper needs an ADR update.

## Alternatives Considered

### `Rgba32Float`

`Rgba32Float` is simpler for precision reasoning but costs more bandwidth and
memory than the engine needs.

### LDR-only post processing

LDR-only processing would avoid an offscreen HDR target, but it would not serve
the lighting pipeline planned after Phase 41.

### Reinhard-only tone mapping

Reinhard is compact and useful for tests, but ACES fitted produces a better
default look for game scenes.

## Compatibility and Migration

No persisted renderer settings are introduced by this ADR. Future project
settings should serialize the selected HDR format, tone mapper, exposure, and
bloom fields with defaults matching this ADR.
