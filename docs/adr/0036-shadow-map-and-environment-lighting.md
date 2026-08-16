# ADR 0036: Shadow Map and Environment Lighting Contract

Status: Accepted
Date: 2026-06-14
Target Phase: Phase 41

Amendment: ADR 0122 completes the split-sum image-based-lighting work that this
MVP explicitly deferred. The shadow-map compatibility contract in this ADR is
unchanged.

## Context

Phase 41 introduces directional shadows and environment lighting. The renderer
already has depth testing, material lighting, and editor preview paths, but it
does not have a stable contract for shadow-map format, cascade policy, or
environment-lighting resources.

The first implementation must avoid locking the engine into a large renderer
rewrite while still establishing the runtime resources and public API that later
GPU passes will use.

## Decision

The MVP shadow contract uses a directional-light shadow map with:

- `Depth32Float` shadow texture format
- 2048 px default shadow-map resolution
- exactly two cascades for the MVP
- explicit depth and normal bias settings

Cascade splits are stored as normalized camera-depth fractions. The default
split places the first cascade at 20% of the camera range and the second at the
far plane.

Environment lighting is represented by an engine resource that stores skybox
and diffuse irradiance intent. Phase 41 may expose the resource before every
GPU path consumes it. Specular split-sum IBL remains out of scope.

## Consequences

- Runtime code can depend on `ShadowSettings` and `EnvironmentLighting` without
  depending on concrete GPU pass internals.
- Two cascades are a compatibility contract for Phase 41; later phases need an
  ADR if they change the cascade count or persisted settings.
- The renderer can add a shadow pass incrementally while keeping the existing
  main pass stable.
- Specular IBL, BRDF LUT generation, and prefiltered environment maps are
  deferred.

## Alternatives Considered

### One shadow cascade

A single cascade is simpler but produces poor quality over large 3D scenes and
would likely need to be replaced immediately.

### Four cascades

Four cascades are common in production renderers but add more tuning and GPU
cost than this engine needs for the first shadow contract.

### External IBL preprocessing dependency

An external preprocessing dependency would be useful for high-quality specular
IBL, but Phase 41 only needs skybox and diffuse environment-lighting contracts.

## Compatibility and Migration

No serialized scene format changes are required by this ADR. Future persisted
renderer settings should serialize the chosen format, resolution, cascade
split, and environment-lighting fields with explicit defaults matching this
ADR.
