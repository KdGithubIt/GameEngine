// Material-aware directional shadow pass for skinned meshes — Phase 50-D.
//
// Bind group 0: per-cascade light view-projection.
// Bind group 1: joint palette (same layout as the main skinned pipeline).
// Bind group 2: generic material textures (base alpha + sampler are consumed).
// Vertex slot 0: per-vertex data (position + UV).
// Vertex slot 1: per-instance data (model matrix + alpha contract).
// Vertex slot 2: skinning attributes (SkinningVertexData::LAYOUT).

struct ShadowCameraUniform {
    view_proj: mat4x4<f32>,
}

// Must match MAX_JOINTS in skinning.rs (128 matrices, 8 KiB).
struct JointPaletteUniform {
    joints: array<mat4x4<f32>, 128>,
}

@group(0) @binding(0) var<uniform> shadow_camera: ShadowCameraUniform;
@group(1) @binding(0) var<uniform> palette: JointPaletteUniform;
@group(2) @binding(0) var base_texture: texture_2d<f32>;
@group(2) @binding(5) var base_sampler: sampler;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(3) uv: vec2<f32>,
}

struct InstanceInput {
    @location(4) model_0: vec4<f32>,
    @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>,
    @location(7) model_3: vec4<f32>,
    @location(8) color: vec4<f32>,
    @location(12) surface: vec4<f32>,
}

struct SkinInput {
    @location(9) joints: vec4<u32>,
    @location(10) weights: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // x = instance alpha, y = alpha cutoff, z = alpha-mode numeric code.
    @location(1) coverage: vec3<f32>,
}

@vertex
fn vs_main(
    vertex: VertexInput,
    instance: InstanceInput,
    skin: SkinInput,
) -> VertexOutput {
    let model = mat4x4<f32>(
        instance.model_0,
        instance.model_1,
        instance.model_2,
        instance.model_3,
    );

    // Same skinning math as mesh_skinned.wgsl so casters match receivers.
    let indices = min(skin.joints, vec4<u32>(127u));
    var skin_matrix = mat4x4<f32>(
        vec4<f32>(1.0, 0.0, 0.0, 0.0),
        vec4<f32>(0.0, 1.0, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0),
        vec4<f32>(0.0, 0.0, 0.0, 1.0),
    );
    let weight_sum = dot(skin.weights, vec4<f32>(1.0));
    if (weight_sum > 0.0001) {
        skin_matrix = (skin.weights.x * palette.joints[indices.x]
            + skin.weights.y * palette.joints[indices.y]
            + skin.weights.z * palette.joints[indices.z]
            + skin.weights.w * palette.joints[indices.w]) * (1.0 / weight_sum);
    }

    var output: VertexOutput;
    output.position = shadow_camera.view_proj * model * skin_matrix * vec4<f32>(vertex.position, 1.0);
    output.uv = vertex.uv;
    output.coverage = vec3<f32>(instance.color.a, instance.surface.z, instance.surface.w);
    return output;
}

fn dither_threshold(position: vec2<f32>) -> f32 {
    let pixel = floor(position);
    return fract(52.9829189 * fract(dot(pixel, vec2<f32>(0.06711056, 0.00583715))));
}

@fragment
fn fs_main(input: VertexOutput) {
    let alpha = textureSample(base_texture, base_sampler, input.uv).a * input.coverage.x;
    let alpha_mode = input.coverage.z;
    if (alpha_mode > 0.5 && alpha_mode < 1.5 && alpha < input.coverage.y) {
        discard;
    }
    if (alpha_mode > 1.5 && alpha <= dither_threshold(input.position.xy)) {
        discard;
    }
}
