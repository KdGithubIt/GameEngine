// Generic skinned-mesh main pass. Fragment behavior matches mesh.wgsl.

struct CameraUniform {
    view_proj: mat4x4<f32>, world_position: vec3<f32>, _pad: f32,
    view: mat4x4<f32>,
}
struct LightUniform {
    ambient_color: vec3<f32>, ambient_intensity: f32,
    dir_direction: vec3<f32>, dir_intensity: f32,
    dir_color: vec3<f32>, _pad: f32,
}
struct ShadowUniform { light_view_proj_0: mat4x4<f32>, light_view_proj_1: mat4x4<f32>, params: vec4<f32> }
struct MaterialUniform {
    toon_shadow: vec4<f32>, toon_ambient: vec4<f32>, toon_specular: vec4<f32>,
    toon_rim: vec4<f32>, toon_params: vec4<f32>, outline: vec4<f32>,
}
struct JointPaletteUniform { joints: array<mat4x4<f32>, 128> }

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var t_diffuse: texture_2d<f32>;
@group(1) @binding(1) var t_normal: texture_2d<f32>;
@group(1) @binding(2) var t_emissive: texture_2d<f32>;
@group(1) @binding(3) var t_toon_ramp: texture_2d<f32>;
@group(1) @binding(4) var t_sphere: texture_2d<f32>;
@group(1) @binding(5) var s_material: sampler;
@group(1) @binding(6) var<uniform> material: MaterialUniform;
@group(2) @binding(0) var<uniform> light: LightUniform;
@group(2) @binding(1) var<uniform> shadow: ShadowUniform;
@group(2) @binding(2) var t_shadow: texture_depth_2d_array;
@group(2) @binding(3) var s_shadow: sampler_comparison;
@group(3) @binding(0) var<uniform> palette: JointPaletteUniform;

struct VertexInput {
    @location(0) position: vec3<f32>, @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>, @location(3) uv: vec2<f32>,
    @location(13) outline_and_uv: vec3<f32>,
}
struct InstanceInput {
    @location(4) model_0: vec4<f32>, @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>, @location(7) model_3: vec4<f32>,
    @location(8) color: vec4<f32>, @location(11) emissive_and_model: vec4<f32>,
    @location(12) surface: vec4<f32>,
}
struct SkinInput { @location(9) joints: vec4<u32>, @location(10) weights: vec4<f32> }
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>, @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>, @location(2) world_normal: vec3<f32>,
    @location(3) instance_color: vec4<f32>, @location(4) world_position: vec3<f32>,
    @location(5) emissive_and_model: vec4<f32>, @location(6) surface: vec4<f32>,
    @location(7) additional_uv: vec2<f32>,
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
    let world_position = deformed_model * vec4<f32>(vertex.position, 1.0);
    var output: VertexOutput;
    output.clip_position = camera.view_proj * world_position;
    output.color = vertex.color; output.uv = vertex.uv;
    output.world_normal = normalize((deformed_model * vec4<f32>(vertex.normal, 0.0)).xyz);
    output.instance_color = instance.color; output.world_position = world_position.xyz;
    output.emissive_and_model = instance.emissive_and_model; output.surface = instance.surface;
    output.additional_uv = vertex.outline_and_uv.yz;
    return output;
}

fn sample_shadow_cascade(layer: i32, view_proj: mat4x4<f32>, world_pos: vec3<f32>) -> vec2<f32> {
    let clip = view_proj * vec4<f32>(world_pos, 1.0); let ndc = clip.xyz / clip.w;
    if (abs(ndc.x) > 1.0 || abs(ndc.y) > 1.0 || ndc.z <= 0.0 || ndc.z >= 1.0) { return vec2<f32>(1.0, 0.0); }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    var sum = 0.0;
    for (var dy: i32 = -1; dy <= 1; dy++) {
        for (var dx: i32 = -1; dx <= 1; dx++) {
            sum += textureSampleCompareLevel(t_shadow, s_shadow,
                uv + vec2<f32>(f32(dx), f32(dy)) * shadow.params.w,
                layer, ndc.z - shadow.params.x);
        }
    }
    return vec2<f32>(sum / 9.0, 1.0);
}
fn shadow_visibility(world_pos: vec3<f32>, normal: vec3<f32>) -> f32 {
    if (shadow.params.z < 0.5 || material.toon_ambient.w < 0.5) { return 1.0; }
    let biased = world_pos + normal * shadow.params.y;
    let first = sample_shadow_cascade(0, shadow.light_view_proj_0, biased);
    if (first.y > 0.5) { return first.x; }
    let second = sample_shadow_cascade(1, shadow.light_view_proj_1, biased);
    return select(1.0, second.x, second.y > 0.5);
}
fn mapped_normal(world_position: vec3<f32>, geometric_normal: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let q1 = dpdx(world_position); let q2 = dpdy(world_position);
    let st1 = dpdx(uv); let st2 = dpdy(uv); let determinant = st1.x * st2.y - st1.y * st2.x;
    if (abs(determinant) < 0.000001) { return normalize(geometric_normal); }
    let tangent = normalize((q1 * st2.y - q2 * st1.y) / determinant);
    let bitangent = normalize((-q1 * st2.x + q2 * st1.x) / determinant);
    let tangent_normal = textureSample(t_normal, s_material, uv).xyz * 2.0 - vec3<f32>(1.0);
    return normalize(mat3x3<f32>(tangent, bitangent, normalize(geometric_normal)) * tangent_normal);
}
fn standard_lit(base: vec3<f32>, n: vec3<f32>, l: vec3<f32>, v: vec3<f32>, surface: vec4<f32>, visibility: f32) -> vec3<f32> {
    let h = normalize(l + v); let ndl = max(dot(n, l), 0.0); let ndv = max(dot(n, v), 0.0001);
    let ndh = max(dot(n, h), 0.0); let vdh = max(dot(v, h), 0.0);
    let roughness = clamp(surface.x, 0.04, 1.0); let metallic = clamp(surface.y, 0.0, 1.0);
    let alpha = roughness * roughness; let alpha2 = alpha * alpha;
    let denominator = ndh * ndh * (alpha2 - 1.0) + 1.0;
    let distribution = alpha2 / max(3.14159265 * denominator * denominator, 0.0001);
    let k = ((roughness + 1.0) * (roughness + 1.0)) / 8.0;
    let geometry = (ndv / (ndv * (1.0 - k) + k)) * (ndl / (ndl * (1.0 - k) + k));
    let f0 = mix(vec3<f32>(0.04), base, vec3<f32>(metallic));
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vdh, 5.0);
    let specular = distribution * geometry * fresnel / max(4.0 * ndv * ndl, 0.0001);
    let direct = (((vec3<f32>(1.0) - fresnel) * (1.0 - metallic)) * base / 3.14159265 + specular)
        * light.dir_color * light.dir_intensity * visibility * ndl;
    return light.ambient_color * light.ambient_intensity * base * (1.0 - metallic) + direct;
}
fn toon_lit(base: vec3<f32>, n: vec3<f32>, l: vec3<f32>, v: vec3<f32>, additional_uv: vec2<f32>, visibility: f32) -> vec3<f32> {
    let ramp_u = clamp(dot(n, l) * visibility, -1.0, 1.0) * 0.5 + 0.5;
    // MMD toon ramps are authored as a vertical gradient (bright at v=0,
    // dark at v=1); the horizontal axis carries no information.
    let ramp = select(smoothstep(0.45, 0.55, ramp_u),
        textureSample(t_toon_ramp, s_material, vec2<f32>(0.5, 1.0 - ramp_u)).r,
        material.toon_shadow.w > 0.5);
    var color = base * mix(material.toon_shadow.rgb, vec3<f32>(1.0), vec3<f32>(ramp));
    color += base * material.toon_ambient.rgb;
    let highlight = pow(max(dot(n, normalize(l + v)), 0.0), max(material.toon_specular.w, 1.0));
    color += material.toon_specular.rgb * step(0.5, highlight) * light.dir_intensity * visibility;
    color += material.toon_rim.rgb * material.toon_rim.w
        * pow(max(1.0 - max(dot(n, v), 0.0), 0.0), max(material.toon_params.x, 0.0001));
    if (material.toon_params.w > 0.5) {
        let view_normal = normalize((camera.view * vec4<f32>(n, 0.0)).xyz);
        let sphere_uv = select(vec2<f32>(view_normal.x * 0.5 + 0.5, 0.5 - view_normal.y * 0.5), additional_uv,
            material.toon_params.z > 0.5);
        let sphere = textureSample(t_sphere, s_material, sphere_uv).rgb;
        color = select(color * sphere, color + sphere, material.toon_params.y > 0.5);
    }
    return color;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let base_color = textureSample(t_diffuse, s_material, input.uv)
        * input.instance_color * vec4<f32>(input.color, 1.0);
    let alpha_mode = input.surface.w;
    if (alpha_mode > 0.5 && alpha_mode < 1.5 && base_color.a < input.surface.z) { discard; }
    let n = mapped_normal(input.world_position, input.world_normal, input.uv);
    let l = normalize(-light.dir_direction); let v = normalize(camera.world_position - input.world_position);
    let visibility = shadow_visibility(input.world_position, n); let model = input.emissive_and_model.w;
    var shaded = base_color.rgb;
    if (model < 0.5) { shaded = standard_lit(base_color.rgb, n, l, v, input.surface, visibility); }
    else if (model < 1.5) { shaded = toon_lit(base_color.rgb, n, l, v, input.additional_uv, visibility); }
    let emissive = textureSample(t_emissive, s_material, input.uv).rgb * input.emissive_and_model.rgb;
    return vec4<f32>(shaded + emissive, select(1.0, base_color.a, alpha_mode > 1.5));
}
