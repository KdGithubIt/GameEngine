const PI: f32 = 3.14159265358979323846;
const TWO_PI: f32 = 6.28318530717958647692;
const SAMPLE_COUNT: u32 = 64u;

struct BakeUniform {
    roughness: f32,
    _padding: vec3<f32>,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@group(0) @binding(2) var<uniform> bake: BakeUniform;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    let position = positions[index];
    out.position = vec4<f32>(position, 0.0, 1.0);
    out.uv = position * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    return out;
}

fn equirect_direction(uv: vec2<f32>) -> vec3<f32> {
    let phi = (uv.x * 2.0 - 1.0) * PI;
    let theta = clamp(uv.y, 0.0, 1.0) * PI;
    let sin_theta = sin(theta);
    return normalize(vec3<f32>(
        cos(phi) * sin_theta,
        cos(theta),
        sin(phi) * sin_theta,
    ));
}

fn direction_uv(direction: vec3<f32>) -> vec2<f32> {
    let n = normalize(direction);
    let u = fract(atan2(n.z, n.x) / TWO_PI + 0.5);
    let v = acos(clamp(n.y, -1.0, 1.0)) / PI;
    return vec2<f32>(u, v);
}

fn radical_inverse_vdc(value: u32) -> f32 {
    var bits = value;
    bits = (bits << 16u) | (bits >> 16u);
    bits = ((bits & 0x55555555u) << 1u) | ((bits & 0xAAAAAAAAu) >> 1u);
    bits = ((bits & 0x33333333u) << 2u) | ((bits & 0xCCCCCCCCu) >> 2u);
    bits = ((bits & 0x0F0F0F0Fu) << 4u) | ((bits & 0xF0F0F0F0u) >> 4u);
    bits = ((bits & 0x00FF00FFu) << 8u) | ((bits & 0xFF00FF00u) >> 8u);
    return f32(bits) * 2.3283064365386963e-10;
}

fn hammersley(index: u32) -> vec2<f32> {
    return vec2<f32>(f32(index) / f32(SAMPLE_COUNT), radical_inverse_vdc(index));
}

fn tangent_basis(normal: vec3<f32>) -> mat3x3<f32> {
    let helper = select(
        vec3<f32>(0.0, 1.0, 0.0),
        vec3<f32>(1.0, 0.0, 0.0),
        abs(normal.y) > 0.999,
    );
    let tangent = normalize(cross(helper, normal));
    let bitangent = cross(normal, tangent);
    return mat3x3<f32>(tangent, normal, bitangent);
}

fn cosine_sample_hemisphere(xi: vec2<f32>, normal: vec3<f32>) -> vec3<f32> {
    let radius = sqrt(xi.x);
    let phi = TWO_PI * xi.y;
    let local = vec3<f32>(
        radius * cos(phi),
        sqrt(max(1.0 - xi.x, 0.0)),
        radius * sin(phi),
    );
    return normalize(tangent_basis(normal) * local);
}

fn importance_sample_ggx(
    xi: vec2<f32>,
    normal: vec3<f32>,
    roughness: f32,
) -> vec3<f32> {
    let alpha = max(roughness * roughness, 0.001);
    let alpha2 = alpha * alpha;
    let phi = TWO_PI * xi.x;
    let cos_theta = sqrt((1.0 - xi.y) / (1.0 + (alpha2 - 1.0) * xi.y));
    let sin_theta = sqrt(max(1.0 - cos_theta * cos_theta, 0.0));
    let local = vec3<f32>(sin_theta * cos(phi), cos_theta, sin_theta * sin(phi));
    return normalize(tangent_basis(normal) * local);
}

fn geometry_schlick_ggx(ndotv: f32, roughness: f32) -> f32 {
    let k = roughness * roughness * 0.5;
    return ndotv / max(ndotv * (1.0 - k) + k, 0.0001);
}

fn geometry_smith(ndotv: f32, ndotl: f32, roughness: f32) -> f32 {
    return geometry_schlick_ggx(ndotv, roughness)
        * geometry_schlick_ggx(ndotl, roughness);
}

@fragment
fn fs_diffuse(in: VsOut) -> @location(0) vec4<f32> {
    let normal = equirect_direction(in.uv);
    var irradiance = vec3<f32>(0.0);
    for (var index = 0u; index < SAMPLE_COUNT; index = index + 1u) {
        let direction = cosine_sample_hemisphere(hammersley(index), normal);
        irradiance += textureSampleLevel(source_texture, source_sampler, direction_uv(direction), 0.0).rgb;
    }
    // Cosine-weighted sampling estimates the Lambert-normalized irradiance
    // directly, so StandardLit can multiply this result by albedo.
    irradiance /= f32(SAMPLE_COUNT);
    return vec4<f32>(irradiance, 1.0);
}

@fragment
fn fs_specular(in: VsOut) -> @location(0) vec4<f32> {
    let normal = equirect_direction(in.uv);
    let roughness = clamp(bake.roughness, 0.0, 1.0);
    if (roughness <= 0.0001) {
        return vec4<f32>(
            textureSampleLevel(source_texture, source_sampler, direction_uv(normal), 0.0).rgb,
            1.0,
        );
    }

    // The prefilter convention uses N == V for each environment direction,
    // matching the split-sum approximation consumed by StandardLit.
    let view = normal;
    var prefiltered = vec3<f32>(0.0);
    var total_weight = 0.0;
    for (var index = 0u; index < SAMPLE_COUNT; index = index + 1u) {
        let half_vector = importance_sample_ggx(hammersley(index), normal, roughness);
        let light = normalize(2.0 * dot(view, half_vector) * half_vector - view);
        let ndotl = max(dot(normal, light), 0.0);
        if (ndotl > 0.0) {
            prefiltered += textureSampleLevel(
                source_texture,
                source_sampler,
                direction_uv(light),
                0.0,
            ).rgb * ndotl;
            total_weight += ndotl;
        }
    }
    prefiltered /= max(total_weight, 0.0001);
    return vec4<f32>(prefiltered, 1.0);
}

@fragment
fn fs_brdf(in: VsOut) -> @location(0) vec4<f32> {
    let ndotv = clamp(in.uv.x, 0.0001, 1.0);
    let roughness = clamp(in.uv.y, 0.0, 1.0);
    let normal = vec3<f32>(0.0, 1.0, 0.0);
    let view = vec3<f32>(sqrt(max(1.0 - ndotv * ndotv, 0.0)), ndotv, 0.0);
    var scale = 0.0;
    var bias = 0.0;

    for (var index = 0u; index < SAMPLE_COUNT; index = index + 1u) {
        let half_vector = importance_sample_ggx(hammersley(index), normal, roughness);
        let light = normalize(2.0 * dot(view, half_vector) * half_vector - view);
        let ndotl = max(light.y, 0.0);
        let ndoth = max(half_vector.y, 0.0);
        let vdoth = max(dot(view, half_vector), 0.0);
        if (ndotl > 0.0) {
            let geometry = geometry_smith(ndotv, ndotl, roughness);
            let visibility = geometry * vdoth / max(ndoth * ndotv, 0.0001);
            let fresnel = pow(1.0 - vdoth, 5.0);
            scale += (1.0 - fresnel) * visibility;
            bias += fresnel * visibility;
        }
    }

    let normalization = 1.0 / f32(SAMPLE_COUNT);
    return vec4<f32>(scale * normalization, bias * normalization, 0.0, 1.0);
}
