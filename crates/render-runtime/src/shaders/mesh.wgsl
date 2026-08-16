// Generic static-mesh main pass and authoritative material fragment stage.

struct CameraUniform {
    view_proj: mat4x4<f32>, world_position: vec3<f32>, _pad: f32,
    view: mat4x4<f32>,
}
struct PointLightData {
    position_range: vec4<f32>,
    color_intensity: vec4<f32>,
}
struct SpotLightData {
    position_range: vec4<f32>,
    direction_outer: vec4<f32>,
    color_intensity: vec4<f32>,
    params: vec4<f32>, // x=cos(inner angle)
}
struct LightUniform {
    ambient_color: vec3<f32>, ambient_intensity: f32,
    dir_direction: vec3<f32>, dir_intensity: f32,
    dir_color: vec3<f32>, _pad: f32,
    counts: vec4<u32>, // x=point count, y=spot count
    point_lights: array<PointLightData, 16>,
    spot_lights: array<SpotLightData, 8>,
}
struct ShadowUniform {
    light_view_proj_0: mat4x4<f32>, light_view_proj_1: mat4x4<f32>, params: vec4<f32>,
    cascade_depths: vec4<f32>, // x=blend start, y=first cascade far depth
}
struct EnvironmentUniform {
    diffuse_color: vec3<f32>,
    intensity: f32,
    params: vec4<f32>, // x=diffuse enabled, y=has diffuse, z=specular enabled, w=has skybox
}
struct MaterialUniform {
    toon_shadow: vec4<f32>,      // rgb=shadow color, w=has ramp
    toon_ambient: vec4<f32>,     // rgb=ambient, w=receive shadow
    toon_specular: vec4<f32>,    // rgb=color, w=power
    toon_rim: vec4<f32>,         // rgb=color, w=intensity
    toon_params: vec4<f32>,      // x=rim power, y=sphere blend, z=coordinates, w=has sphere
    outline: vec4<f32>,
    pbr_params: vec4<f32>,       // x=normal scale, y=occlusion strength
}

@group(0) @binding(0) var<uniform> camera: CameraUniform;
@group(1) @binding(0) var t_diffuse: texture_2d<f32>;
@group(1) @binding(1) var t_normal: texture_2d<f32>;
@group(1) @binding(2) var t_emissive: texture_2d<f32>;
@group(1) @binding(3) var t_toon_ramp: texture_2d<f32>;
@group(1) @binding(4) var t_sphere: texture_2d<f32>;
@group(1) @binding(5) var s_material: sampler;
@group(1) @binding(6) var<uniform> material: MaterialUniform;
@group(1) @binding(7) var t_metallic_roughness: texture_2d<f32>;
@group(1) @binding(8) var t_occlusion: texture_2d<f32>;
@group(2) @binding(0) var<uniform> light: LightUniform;
@group(2) @binding(1) var<uniform> shadow: ShadowUniform;
@group(2) @binding(2) var t_shadow: texture_depth_2d_array;
@group(2) @binding(3) var s_shadow: sampler_comparison;
@group(2) @binding(4) var t_environment_diffuse: texture_2d<f32>;
@group(2) @binding(5) var t_environment_specular: texture_2d<f32>;
@group(2) @binding(6) var t_environment_brdf: texture_2d<f32>;
@group(2) @binding(7) var s_environment: sampler;
@group(2) @binding(8) var<uniform> environment: EnvironmentUniform;

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
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>, @location(0) color: vec3<f32>,
    @location(1) uv: vec2<f32>, @location(2) world_normal: vec3<f32>,
    @location(3) instance_color: vec4<f32>, @location(4) world_position: vec3<f32>,
    @location(5) emissive_and_model: vec4<f32>, @location(6) surface: vec4<f32>,
    @location(7) additional_uv: vec2<f32>, @location(8) world_tangent: vec4<f32>,
}

@vertex
fn vs_main(vertex: VertexInput, instance: InstanceInput) -> VertexOutput {
    let model = mat4x4<f32>(instance.model_0, instance.model_1, instance.model_2, instance.model_3);
    let model_linear = mat3x3<f32>(instance.model_0.xyz, instance.model_1.xyz, instance.model_2.xyz);
    let world_position = model * vec4<f32>(vertex.position, 1.0);
    var output: VertexOutput;
    output.clip_position = camera.view_proj * world_position;
    output.color = vertex.color;
    output.uv = vertex.uv * instance.uv_transform.xy + instance.uv_transform.zw;
    output.world_normal = normalize((model * vec4<f32>(vertex.normal, 0.0)).xyz);
    var world_tangent = vec3<f32>(0.0);
    if (dot(vertex.tangent.xyz, vertex.tangent.xyz) > 0.000001) {
        world_tangent = normalize(model_linear * vertex.tangent.xyz);
    }
    let orientation = select(-1.0, 1.0, determinant(model_linear) >= 0.0);
    output.world_tangent = vec4<f32>(world_tangent, vertex.tangent.w * orientation);
    output.instance_color = instance.color;
    output.world_position = world_position.xyz;
    output.emissive_and_model = instance.emissive_and_model;
    output.surface = instance.surface;
    output.additional_uv = vertex.outline_and_uv.yz;
    return output;
}

fn sample_shadow_cascade(layer: i32, view_proj: mat4x4<f32>, world_pos: vec3<f32>) -> vec2<f32> {
    let clip = view_proj * vec4<f32>(world_pos, 1.0);
    let ndc = clip.xyz / clip.w;
    if (abs(ndc.x) > 1.0 || abs(ndc.y) > 1.0 || ndc.z <= 0.0 || ndc.z >= 1.0) {
        return vec2<f32>(1.0, 0.0);
    }
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    let reference = ndc.z - shadow.params.x;
    var sum = 0.0;
    var weight_sum = 0.0;
    for (var dy: i32 = -1; dy <= 1; dy++) {
        for (var dx: i32 = -1; dx <= 1; dx++) {
            let weight_x = select(1.0, 2.0, dx == 0);
            let weight_y = select(1.0, 2.0, dy == 0);
            let weight = weight_x * weight_y;
            sum += textureSampleCompareLevel(
                t_shadow, s_shadow, uv + vec2<f32>(f32(dx), f32(dy)) * shadow.params.w,
                layer, reference,
            ) * weight;
            weight_sum += weight;
        }
    }
    return vec2<f32>(sum / weight_sum, 1.0);
}

fn shadow_visibility(world_pos: vec3<f32>, normal: vec3<f32>) -> f32 {
    if (shadow.params.z < 0.5 || material.toon_ambient.w < 0.5) { return 1.0; }
    let biased = world_pos + normal * shadow.params.y;
    let view_depth = max(-(camera.view * vec4<f32>(world_pos, 1.0)).z, 0.0);
    let blend_start = shadow.cascade_depths.x;
    let first_far = shadow.cascade_depths.y;

    let first = sample_shadow_cascade(0, shadow.light_view_proj_0, biased);
    if (view_depth < blend_start) {
        if (first.y > 0.5) { return first.x; }
        let second = sample_shadow_cascade(1, shadow.light_view_proj_1, biased);
        return select(1.0, second.x, second.y > 0.5);
    }

    let second = sample_shadow_cascade(1, shadow.light_view_proj_1, biased);
    if (view_depth <= first_far && first.y > 0.5 && second.y > 0.5 && first_far > blend_start) {
        let blend = smoothstep(blend_start, first_far, view_depth);
        return mix(first.x, second.x, blend);
    }
    if (view_depth <= first_far && first.y > 0.5) { return first.x; }
    if (second.y > 0.5) { return second.x; }
    if (first.y > 0.5) { return first.x; }
    return 1.0;
}

fn mapped_normal(
    world_position: vec3<f32>,
    geometric_normal: vec3<f32>,
    tangent_frame: vec4<f32>,
    uv: vec2<f32>,
) -> vec3<f32> {
    let normal = normalize(geometric_normal);
    var tangent_normal = textureSample(t_normal, s_material, uv).xyz * 2.0 - vec3<f32>(1.0);
    tangent_normal.x *= material.pbr_params.x;
    tangent_normal.y *= material.pbr_params.x;
    tangent_normal = normalize(tangent_normal);
    if (abs(tangent_frame.w) > 0.5) {
        let orthogonal = tangent_frame.xyz - normal * dot(normal, tangent_frame.xyz);
        if (dot(orthogonal, orthogonal) > 0.000001) {
            let tangent = normalize(orthogonal);
            let bitangent = normalize(cross(normal, tangent)) * tangent_frame.w;
            return normalize(mat3x3<f32>(tangent, bitangent, normal) * tangent_normal);
        }
    }

    let q1 = dpdx(world_position); let q2 = dpdy(world_position);
    let st1 = dpdx(uv); let st2 = dpdy(uv);
    let determinant = st1.x * st2.y - st1.y * st2.x;
    if (abs(determinant) < 0.000001) { return normal; }
    let tangent = normalize((q1 * st2.y - q2 * st1.y) / determinant);
    let bitangent = normalize((-q1 * st2.x + q2 * st1.x) / determinant);
    return normalize(mat3x3<f32>(tangent, bitangent, normal) * tangent_normal);
}

fn direction_uv(direction: vec3<f32>) -> vec2<f32> {
    let normalized = normalize(direction);
    let u = fract(atan2(normalized.z, normalized.x) / 6.283185307179586 + 0.5);
    let v = acos(clamp(normalized.y, -1.0, 1.0)) / 3.141592653589793;
    return vec2<f32>(u, v);
}

fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let grazing = max(vec3<f32>(1.0 - roughness), f0);
    return f0 + (grazing - f0) * pow(1.0 - cos_theta, 5.0);
}

fn standard_direct(
    base: vec3<f32>,
    n: vec3<f32>,
    l: vec3<f32>,
    v: vec3<f32>,
    surface: vec4<f32>,
    radiance: vec3<f32>,
    visibility: f32,
) -> vec3<f32> {
    let ndl = max(dot(n, l), 0.0);
    if (ndl <= 0.0) { return vec3<f32>(0.0); }
    let h = normalize(l + v);
    let ndv = max(dot(n, v), 0.0001);
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
    let diffuse_weight = (vec3<f32>(1.0) - fresnel) * (1.0 - metallic);
    return (diffuse_weight * base / 3.14159265 + specular) * radiance * visibility * ndl;
}

fn local_light_attenuation(distance: f32, range: f32) -> f32 {
    if (distance >= range || range <= 0.0) { return 0.0; }
    let normalized = distance / range;
    let cutoff = max(1.0 - normalized * normalized * normalized * normalized, 0.0);
    return cutoff * cutoff / max(distance * distance, 0.01);
}

fn standard_lit(
    base: vec3<f32>,
    n: vec3<f32>,
    l: vec3<f32>,
    v: vec3<f32>,
    world_position: vec3<f32>,
    surface: vec4<f32>,
    visibility: f32,
    occlusion: f32,
) -> vec3<f32> {
    var direct = standard_direct(
        base, n, l, v, surface, light.dir_color * light.dir_intensity, visibility,
    );

    for (var i: u32 = 0u; i < light.counts.x; i = i + 1u) {
        let point = light.point_lights[i];
        let to_light = point.position_range.xyz - world_position;
        let distance = length(to_light);
        if (distance > 0.0001) {
            let attenuation = local_light_attenuation(distance, point.position_range.w);
            let radiance = point.color_intensity.rgb * point.color_intensity.w * attenuation;
            direct += standard_direct(base, n, to_light / distance, v, surface, radiance, 1.0);
        }
    }

    for (var i: u32 = 0u; i < light.counts.y; i = i + 1u) {
        let spot = light.spot_lights[i];
        let to_light = spot.position_range.xyz - world_position;
        let distance = length(to_light);
        if (distance > 0.0001) {
            let l_local = to_light / distance;
            let cone_cosine = dot(normalize(spot.direction_outer.xyz), -l_local);
            let cone = smoothstep(spot.direction_outer.w, spot.params.x, cone_cosine);
            let attenuation = local_light_attenuation(distance, spot.position_range.w) * cone;
            let radiance = spot.color_intensity.rgb * spot.color_intensity.w * attenuation;
            direct += standard_direct(base, n, l_local, v, surface, radiance, 1.0);
        }
    }

    let ndv = max(dot(n, v), 0.0001);
    let roughness = clamp(surface.x, 0.04, 1.0); let metallic = clamp(surface.y, 0.0, 1.0);
    let f0 = mix(vec3<f32>(0.04), base, vec3<f32>(metallic));

    var indirect_diffuse = light.ambient_color * light.ambient_intensity * base * (1.0 - metallic);
    if (environment.params.x > 0.5) {
        if (environment.params.y > 0.5) {
            let irradiance = textureSample(
                t_environment_diffuse,
                s_environment,
                direction_uv(n),
            ).rgb;
            let environment_fresnel = fresnel_schlick_roughness(ndv, f0, roughness);
            let environment_diffuse_weight = (vec3<f32>(1.0) - environment_fresnel) * (1.0 - metallic);
            indirect_diffuse = irradiance * base * environment_diffuse_weight * environment.intensity;
        } else {
            indirect_diffuse = environment.diffuse_color
                * light.ambient_intensity
                * environment.intensity
                * base
                * (1.0 - metallic);
        }
    }

    var indirect_specular = vec3<f32>(0.0);
    if (environment.params.z > 0.5) {
        let reflection = reflect(-v, n);
        let max_lod = f32(textureNumLevels(t_environment_specular) - 1u);
        let prefiltered = textureSampleLevel(
            t_environment_specular,
            s_environment,
            direction_uv(reflection),
            roughness * max_lod,
        ).rgb;
        let brdf = textureSample(
            t_environment_brdf,
            s_environment,
            vec2<f32>(ndv, roughness),
        ).rg;
        indirect_specular = prefiltered * (f0 * brdf.x + vec3<f32>(brdf.y)) * environment.intensity;
    }

    return (indirect_diffuse + indirect_specular) * occlusion + direct;
}

fn toon_ramp_factor(ndl: f32, visibility: f32) -> f32 {
    // Push fully shadowed directional fragments to the authored dark end of
    // the ramp instead of collapsing them to the half-lit midpoint.
    let visible_ndl = mix(-1.0, clamp(ndl, -1.0, 1.0), clamp(visibility, 0.0, 1.0));
    let ramp_u = visible_ndl * 0.5 + 0.5;
    let generated_ramp = smoothstep(0.45, 0.55, ramp_u);
    // MMD toon ramps are authored as a vertical gradient (bright at v=0,
    // dark at v=1); the horizontal axis carries no information.
    let sampled_ramp = textureSampleLevel(
        t_toon_ramp,
        s_material,
        vec2<f32>(0.5, 1.0 - ramp_u),
        0.0,
    ).r;
    return select(generated_ramp, sampled_ramp, material.toon_shadow.w > 0.5);
}

fn toon_direct(
    base: vec3<f32>,
    n: vec3<f32>,
    l: vec3<f32>,
    v: vec3<f32>,
    radiance: vec3<f32>,
    visibility: f32,
) -> vec3<f32> {
    let ramp = toon_ramp_factor(dot(n, l), visibility);
    // The material shadow color is the one shared floor. Each light adds only
    // the bright-side delta, so adding local lights cannot stack dark terms.
    let diffuse = base * (vec3<f32>(1.0) - material.toon_shadow.rgb) * ramp;

    var specular = vec3<f32>(0.0);
    let half_vector = l + v;
    if (dot(half_vector, half_vector) > 0.000001) {
        let highlight = pow(
            max(dot(n, normalize(half_vector)), 0.0),
            max(material.toon_specular.w, 1.0),
        );
        let highlight_band = smoothstep(0.45, 0.55, highlight)
            * clamp(visibility, 0.0, 1.0);
        specular = material.toon_specular.rgb * highlight_band;
    }

    return (diffuse + specular) * radiance;
}

fn toon_lit(
    base: vec3<f32>,
    n: vec3<f32>,
    l: vec3<f32>,
    v: vec3<f32>,
    world_position: vec3<f32>,
    additional_uv: vec2<f32>,
    visibility: f32,
) -> vec3<f32> {
    var color = base * (material.toon_shadow.rgb + material.toon_ambient.rgb);
    color += toon_direct(
        base,
        n,
        l,
        v,
        light.dir_color * light.dir_intensity,
        visibility,
    );

    for (var i: u32 = 0u; i < light.counts.x; i = i + 1u) {
        let point = light.point_lights[i];
        let to_light = point.position_range.xyz - world_position;
        let distance = length(to_light);
        if (distance > 0.0001) {
            let attenuation = local_light_attenuation(distance, point.position_range.w);
            let radiance = point.color_intensity.rgb * point.color_intensity.w * attenuation;
            color += toon_direct(base, n, to_light / distance, v, radiance, 1.0);
        }
    }

    for (var i: u32 = 0u; i < light.counts.y; i = i + 1u) {
        let spot = light.spot_lights[i];
        let to_light = spot.position_range.xyz - world_position;
        let distance = length(to_light);
        if (distance > 0.0001) {
            let l_local = to_light / distance;
            let cone_cosine = dot(normalize(spot.direction_outer.xyz), -l_local);
            let cone = smoothstep(spot.direction_outer.w, spot.params.x, cone_cosine);
            let attenuation = local_light_attenuation(distance, spot.position_range.w) * cone;
            let radiance = spot.color_intensity.rgb * spot.color_intensity.w * attenuation;
            color += toon_direct(base, n, l_local, v, radiance, 1.0);
        }
    }

    let rim = pow(max(1.0 - max(dot(n, v), 0.0), 0.0), max(material.toon_params.x, 0.0001));
    color += material.toon_rim.rgb * material.toon_rim.w * rim;
    if (material.toon_params.w > 0.5) {
        let view_normal = normalize((camera.view * vec4<f32>(n, 0.0)).xyz);
        let normal_uv = vec2<f32>(view_normal.x * 0.5 + 0.5, 0.5 - view_normal.y * 0.5);
        let sphere_uv = select(normal_uv, additional_uv, material.toon_params.z > 0.5);
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
    let n = mapped_normal(input.world_position, input.world_normal, input.world_tangent, input.uv);
    let l = normalize(-light.dir_direction); let v = normalize(camera.world_position - input.world_position);
    let metallic_roughness = textureSample(t_metallic_roughness, s_material, input.uv);
    let pbr_surface = vec4<f32>(
        input.surface.x * metallic_roughness.g,
        input.surface.y * metallic_roughness.b,
        input.surface.z,
        input.surface.w,
    );
    let occlusion_sample = textureSample(t_occlusion, s_material, input.uv).r;
    let occlusion = mix(1.0, occlusion_sample, clamp(material.pbr_params.y, 0.0, 1.0));
    let visibility = shadow_visibility(input.world_position, n);
    let model = input.emissive_and_model.w;
    var shaded = base_color.rgb;
    if (model < 0.5) {
        shaded = standard_lit(
            base_color.rgb,
            n,
            l,
            v,
            input.world_position,
            pbr_surface,
            visibility,
            occlusion,
        );
    } else if (model < 1.5) {
        shaded = toon_lit(
            base_color.rgb,
            n,
            l,
            v,
            input.world_position,
            input.additional_uv,
            visibility,
        );
    }
    let emissive = textureSample(t_emissive, s_material, input.uv).rgb * input.emissive_and_model.rgb;
    return vec4<f32>(shaded + emissive, select(1.0, base_color.a, alpha_mode > 1.5));
}
