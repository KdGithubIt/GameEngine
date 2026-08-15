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

## Consequences

- Gameplay/editor code can configure exposure, tone mapping, and bloom through a
  stable resource.
- GPU implementation can be incremental: direct swapchain rendering can coexist
  with the settings resource until the offscreen pass lands.
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
