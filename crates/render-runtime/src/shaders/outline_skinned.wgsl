// Screen-space outline classification pass for skinned meshes.

struct CameraUniform {
    view_proj: mat4x4<f32>,
    world_position: vec3<f32>,
    viewport_aspect: f32,
    view: mat4x4<f32>,
}

struct MaterialUniform {
    toon_shadow: vec4<f32>,
    toon_ambient: vec4<f32>,
    toon_specular: vec4<f32>,
    toon_rim: vec4<f32>,
    toon_params: vec4<f32>,
    outline: vec4<f32>,
}

struct JointPaletteUniform {
    joints: array<mat4x4<f32>, 128>,
}

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var t_diffuse: texture_2d<f32>;
@group(1) @binding(5) var s_material: sampler;
@group(1) @binding(6) var<uniform> material: MaterialUniform;
@group(2) @binding(0) var<uniform> palette: JointPaletteUniform;

const OUTLINE_REFERENCE_HEIGHT: f32 = 1024.0;
const OUTLINE_MAX_RADIUS_REFERENCE_TEXELS: f32 = 4.0;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(3) uv: vec2<f32>,
    @location(13) outline_and_uv: vec3<f32>,
}

struct InstanceInput {
    @location(4) model_0: vec4<f32>,
    @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>,
    @location(7) model_3: vec4<f32>,
    @location(8) color: vec4<f32>,
    @location(12) surface: vec4<f32>,
    @location(14) outline_identity: vec4<u32>,
}

struct SkinInput {
    @location(9) joints: vec4<u32>,
    @location(10) weights: vec4<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) instance_color: vec4<f32>,
    @location(2) surface: vec4<f32>,
    @location(3) projected_radius: f32,
    @location(4) @interpolate(flat) outline_identity: vec2<u32>,
}

struct FragmentOutput {
    @location(0) style: vec4<f32>,
    @location(1) outline_identity: vec2<u32>,
}

fn safe_ndc(clip: vec4<f32>) -> vec2<f32> {
    var safe_w = clip.w;
    if (abs(safe_w) < 0.000001) {
        safe_w = select(-0.000001, 0.000001, safe_w >= 0.0);
    }
    return clip.xy / safe_w;
}

fn skin_matrix(skin: SkinInput) -> mat4x4<f32> {
    let indices = min(skin.joints, vec4<u32>(127u));
    let sum = dot(skin.weights, vec4<f32>(1.0));
    if (sum <= 0.0001) {
        return mat4x4<f32>(
            vec4<f32>(1.0, 0.0, 0.0, 0.0),
            vec4<f32>(0.0, 1.0, 0.0, 0.0),
            vec4<f32>(0.0, 0.0, 1.0, 0.0),
            vec4<f32>(0.0, 0.0, 0.0, 1.0),
        );
    }

    return (
        skin.weights.x * palette.joints[indices.x]
        + skin.weights.y * palette.joints[indices.y]
        + skin.weights.z * palette.joints[indices.z]
        + skin.weights.w * palette.joints[indices.w]
    ) * (1.0 / sum);
}

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput, skin: SkinInput) -> VertexOutput {
    let model = mat4x4<f32>(instance.model_0, instance.model_1, instance.model_2, instance.model_3);
    let skinning = skin_matrix(skin);
    let width = material.outline.w * max(vertex.outline_and_uv.x, 0.0);
    let surface_clip = camera.view_proj * model * skinning * vec4<f32>(vertex.position, 1.0);
    let expanded_clip = camera.view_proj * model * skinning
        * vec4<f32>(vertex.position + vertex.normal * width, 1.0);
    let ndc_delta = safe_ndc(expanded_clip) - safe_ndc(surface_clip);
    let reference_texels = length(ndc_delta * vec2<f32>(camera.viewport_aspect, 1.0))
        * (OUTLINE_REFERENCE_HEIGHT * 0.5);

    var output: VertexOutput;
    output.clip_position = surface_clip;
    output.uv = vertex.uv;
    output.instance_color = instance.color;
    output.surface = instance.surface;
    output.projected_radius = select(
        0.0,
        clamp(
            reference_texels / OUTLINE_MAX_RADIUS_REFERENCE_TEXELS,
            0.0,
            1.0,
        ),
        width > 0.0,
    );
    output.outline_identity = instance.outline_identity.xy;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> FragmentOutput {
    let base_alpha = textureSample(t_diffuse, s_material, input.uv).a * input.instance_color.a;
    let alpha_mode = input.surface.w;
    if (alpha_mode > 0.5 && alpha_mode < 1.5 && base_alpha < input.surface.z) {
        discard;
    }
    if (alpha_mode > 1.5 && base_alpha <= 0.001) {
        discard;
    }

    var output: FragmentOutput;
    output.style = vec4<f32>(material.outline.rgb, input.projected_radius);
    output.outline_identity = input.outline_identity;
    return output;
}
