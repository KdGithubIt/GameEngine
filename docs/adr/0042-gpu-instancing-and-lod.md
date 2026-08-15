# ADR 0042 — GPU Instancing and Level-of-Detail (Phase 47)

## Status

Accepted

## Context

The previous mesh pipeline used one dynamic-offset uniform buffer (group 2, `ModelUniform`) per entity per frame. This meant O(N) bind-group switches even when many entities shared the same mesh and material, capping scene density.

Phase 47 replaces the per-entity model uniform with GPU instancing and adds a LOD selection system.

## Decisions

### 1. Instance vertex buffer (slot 1)

`InstanceData` (80 bytes, `repr(C)`, `bytemuck::Pod`) holds the full model matrix (4×vec4, locations 4–7) and a per-instance tint color (vec4, location 8). It is uploaded each frame as a transient `VERTEX` buffer via `RenderState::make_instance_buffer`.

Shader vertex slot 0 carries `Vertex` (per-vertex, `step_mode=Vertex`); slot 1 carries `InstanceData` (`step_mode=Instance`).

### 2. Bind-group layout change

Removing the model uniform collapses the pipeline from four bind-group slots to three:

| Group | Before | After |
|-------|--------|-------|
| 0 | Camera | Camera |
| 1 | Texture + sampler | Texture + sampler |
| 2 | Model (dynamic offset) | Light |
| 3 | Light | — (removed) |

### 3. GpuMeshCache world resource

`GpuMeshCache` (`HashMap<RuntimeAssetId, GpuMesh>`) is a world resource inserted by `App::new`. `upload_pending_meshes` populates it for every `Handle<Mesh>` whose ID is not yet cached. Entities with direct `Mesh` components still get a per-entity `GpuMesh` (used by the `Without<Handle<Mesh>>` batch path).

### 4. Batch keying

`collect_batches` groups entities by `(vertex_buffer_ptr, texture_bind_group_ptr)`. Entities sharing the same `GpuMesh` (same `Arc<wgpu::Buffer>` pointer) and texture are merged into one instanced draw call, emitting a single `draw_indexed` or `draw` call per batch.

### 5. LodGroup component

`LodGroup { levels: Vec<LodLevel> }` selects a `Handle<Mesh>` based on camera distance. `lod_selection_system` runs before the render upload step and writes the chosen handle back onto the entity, so the correct mesh is in the cache when instancing runs.

### 6. InstanceStats resource

`InstanceStats { batch_count, total_instances }` is updated each frame by the render pass and can be read by debug UIs to monitor instancing efficiency.

## Consequences

- Scene density scales much better: N entities sharing one mesh now cost 1 draw call instead of N.
- The `ModelUniform` struct, dynamic-offset bind group, `prepare_models`, `draw_mesh`, `make_model_buffer`, `make_model_bind_group`, and `maximum_model_count` are all removed.
- Instance buffers are transient (created and dropped each frame); a ring-buffer or persistent mapped buffer is a future optimization.
- LOD switching writes `Handle<Mesh>` back onto entities each frame; a dirty-flag optimization can be added later.
