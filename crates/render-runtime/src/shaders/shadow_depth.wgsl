// Depth-only shadow pass — Phase 50 (ADR 0036).
//
// Bind group 0: per-cascade light view-projection.
// Vertex slot 0: per-vertex data (Vertex::LAYOUT, position only is read).
// Vertex slot 1: per-instance data (InstanceData::LAYOUT, model matrix only).

struct ShadowCameraUniform {
    view_proj: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> shadow_camera: ShadowCameraUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
}

struct InstanceInput {
    @location(4) model_0: vec4<f32>,
    @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>,
    @location(7) model_3: vec4<f32>,
}

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(
        instance.model_0,
        instance.model_1,
        instance.model_2,
        instance.model_3,
    );
    return shadow_camera.view_proj * model * vec4<f32>(vertex.position, 1.0);
}
