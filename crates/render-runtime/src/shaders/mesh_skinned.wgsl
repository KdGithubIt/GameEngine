// Generic skinned-mesh vertex deformation. Material pipelines pair this
// module's `vs_main` with the authoritative `mesh.wgsl` fragment stage.

struct CameraUniform {
    view_proj: mat4x4<f32>, world_position: vec3<f32>, _pad: f32,
    view: mat4x4<f32>,
}
struct JointPaletteUniform { joints: array<mat4x4<f32>, 128> }

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(3) @binding(0) var<uniform> palette: JointPaletteUniform;

struct VertexInput {
    @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>, @location(3) uv: vec2<f32>,
    @location(13) outline_and_uv: vec3<f32>, @location(15) tangent: vec4<f32>,
}
struct InstanceInput {
    @location(4) model_0: vec4<f32>, @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>, @location(7) model_3: vec4<f32>,
    @location(8) color: vec4<f32>, @location(11) emissive_and_model: vec4<f32>,
    @location(12) surface: vec4<f32>,
    @location(14) uv_transform: vec4<f32>,
}
struct SkinInput { @location(9) joints: vec4<u32>, @location(10) weights: vec4<f32> }
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>, @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>, @location(2) world_normal: vec3<f32>,
    @location(3) instance_color: vec4<f32>, @location(4) world_position: vec3<f32>,
    @location(5) emissive_and_model: vec4<f32>, @location(6) surface: vec4<f32>,
    @location(7) additional_uv: vec2<f32>, @location(8) world_tangent: vec4<f32>,
}

fn skin_matrix(skin: SkinInput) -> mat4x4<f32> {
    let indices = min(skin.joints, vec4<u32>(127u));
    let sum = dot(skin.weights, vec4<f32>(1.0));
    if (sum > 0.0001) {
        return (skin.weights.x * palette.joints[indices.x]
            + skin.weights.y * palette.joints[indices.y]
            + skin.weights.z * palette.joints[indices.z]
            + skin.weights.w * palette.joints[indices.w]) * (1.0 / sum);
    }
    return mat4x4<f32>(
        vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0),
        vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0),
    );
}

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput, skin: SkinInput) -> VertexOutput {
    let model = mat4x4<f32>(instance.model_0, instance.model_1, instance.model_2, instance.model_3);
    let deformed_model = model * skin_matrix(skin);
    let deformed_linear = mat3x3<f32>(deformed_model[0].xyz, deformed_model[1].xyz, deformed_model[2].xyz);
    let world_position = deformed_model * vec4<f32>(vertex.position, 1.0);
    var output: VertexOutput;
    output.clip_position = camera.view_proj * world_position;
    output.color = vertex.color; output.uv = vertex.uv * instance.uv_transform.xy + instance.uv_transform.zw;
    output.world_normal = normalize((deformed_model * vec4<f32>(vertex.normal, 0.0)).xyz);
    var world_tangent = vec3<f32>(0.0);
    if (dot(vertex.tangent.xyz, vertex.tangent.xyz) > 0.000001) {
        world_tangent = normalize(deformed_linear * vertex.tangent.xyz);
    }
    let orientation = select(-1.0, 1.0, determinant(deformed_linear) >= 0.0);
    output.world_tangent = vec4<f32>(world_tangent, vertex.tangent.w * orientation);
    output.instance_color = instance.color; output.world_position = world_position.xyz;
    output.emissive_and_model = instance.emissive_and_model; output.surface = instance.surface;
    output.additional_uv = vertex.outline_and_uv.yz;
    return output;
}
