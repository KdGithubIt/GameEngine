# ADR 0003: Runtime Renderer Ownership Boundary

Status: Accepted
Date: 2026-06-04

## Context

The workspace contained an `engine-renderer` crate for GPU context and window
surface helpers, while the `engine` crate independently implemented the same
initialization and surface configuration logic.

Two low-level rendering paths can silently choose different adapters, formats,
limits, or recovery behavior. They also make it unclear where future backend
configuration, headless rendering, and platform-specific surface behavior
belong.

Runtime mesh and texture data can also reach wgpu with invalid dimensions,
empty buffers, truncated draw counts, or out-of-bounds indices unless the
engine validates that data before resource creation.

## Decision

The `engine-renderer` crate owns low-level GPU adapter, device, queue, and
window surface initialization.

- `engine-renderer` must not depend on `engine`, ECS, authoring, editor, CLI, or
  MCP types.
- `engine` depends on `engine-renderer` and must not duplicate its GPU context
  or window surface initialization logic.
- Surface initialization uses an explicit
  `UnconfiguredWindowSurface -> GpuContext -> WindowSurface` flow because
  adapter selection depends on the surface and surface configuration depends on
  the selected adapter.
- Default initialization remains convenient, while descriptors expose adapter,
  device, format, present mode, and alpha mode choices without duplicating
  initialization code.
- `GpuContext` keeps its adapter, device, and queue as one validated unit;
  callers use accessors rather than constructing mismatched handle sets.
- Runtime render assets are validated before they are passed to wgpu.
- Recoverable rendering failures return typed errors where practical.
- A surface out-of-memory failure is treated as fatal for the running
  application instead of being retried every frame.

High-level render extraction, built-in materials, meshes, and pipelines may
remain in `engine` while the renderer crate is still small. Moving those
features later requires preserving the one-way dependency from `engine` to
`engine-renderer`.

## Consequences

- GPU and surface policy changes have one implementation owner.
- The renderer crate remains reusable by future runtime applications without
  importing gameplay or authoring concepts.
- Initialization order is visible in the type system.
- Invalid mesh and texture input can be reported before wgpu validation fails.
- Some early public rendering APIs return typed errors instead of assuming all
  input is valid.

## Alternatives Considered

### Keep both implementations temporarily

Rejected because the duplicate paths already create ambiguity and will diverge
as soon as platform or backend settings change.

### Move all high-level rendering into `engine-renderer` immediately

Rejected because built-in mesh, material, and ECS extraction ownership is not
yet stable enough to justify a large crate migration.

### Remove the renderer crate

Rejected because a low-level renderer boundary is useful for future headless,
editor, and runtime rendering configurations.

## Compatibility and Migration

The project is in an early prototype stage, so this ADR permits breaking
changes to the renderer helper APIs and runtime mesh or texture upload APIs.

No persisted authoring data or serialized format is affected.
