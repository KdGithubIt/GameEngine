// Post-processing pass: tone mapping and bloom (Phase 45, ADR 0040).
//
// Reads from an Rgba16Float HDR texture and writes to the swapchain surface.
// A fullscreen triangle is emitted from vertex indices (no vertex buffer).

struct PostProcessUniforms {
    exposure: f32,
    tone_map_op: u32,      // 0 = ACES fitted, 1 = Reinhard
    bloom_enabled: u32,    // 0 = disabled
    bloom_threshold: f32,
    bloom_intensity: f32,
    bloom_radius: f32,
    grading_enabled: u32,
    grading_saturation: f32,
    grading_contrast: f32,
    grading_gamma: f32,
    grading_tint_r: f32,
    grading_tint_g: f32,
    grading_tint_b: f32,
    output_srgb_encode: u32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var hdr_sampler: sampler;
@group(0) @binding(2) var<uniform> pp: PostProcessUniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VertexOutput {
    // Single large triangle covering the clip-space quad.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 3.0,  1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(2.0, 0.0),
    );
    var out: VertexOutput;
    out.position = vec4<f32>(positions[vi], 0.0, 1.0);
    out.uv = uvs[vi];
    return out;
}

fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn reinhard(c: vec3<f32>) -> vec3<f32> {
    return c / (vec3<f32>(1.0) + c);
}

fn aces_fitted(c: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let cv = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp(
        (c * (a * c + vec3<f32>(b))) / (c * (cv * c + vec3<f32>(d)) + vec3<f32>(e)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

fn apply_tone_map(c: vec3<f32>, op: u32) -> vec3<f32> {
    if op == 1u {
        return reinhard(c);
    }
    return aces_fitted(c);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let clamped = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let low = clamped * 12.92;
    let high = vec3<f32>(1.055) * pow(clamped, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(high, low, clamped <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let hdr = textureSample(hdr_texture, hdr_sampler, in.uv).rgb;

    // Simple 9-tap neighbourhood bloom extraction (Phase 45 MVP approximation).
    // A dedicated multi-pass blur is deferred to a later phase.
    var bloom = vec3<f32>(0.0);
    if pp.bloom_enabled != 0u {
        let dims = vec2<f32>(textureDimensions(hdr_texture));
        let texel = vec2<f32>(1.0) / dims;
        let spread = max(pp.bloom_radius, 0.0);
        let threshold = pp.bloom_threshold;
        let intensity = pp.bloom_intensity;
        for (var dy: i32 = -1; dy <= 1; dy++) {
            for (var dx: i32 = -1; dx <= 1; dx++) {
                let offset = vec2<f32>(f32(dx), f32(dy)) * texel * spread;
                let s = textureSample(hdr_texture, hdr_sampler, in.uv + offset).rgb;
                if luminance(s) > threshold {
                    bloom += max(s - vec3<f32>(threshold), vec3<f32>(0.0)) * intensity;
                }
            }
        }
        bloom = bloom / 9.0;
    }

    let exposed = (hdr + bloom) * max(pp.exposure, 0.0);
    var mapped = apply_tone_map(exposed, pp.tone_map_op);
    if pp.grading_enabled != 0u {
        mapped *= vec3<f32>(pp.grading_tint_r, pp.grading_tint_g, pp.grading_tint_b);
        let gray = vec3<f32>(luminance(mapped));
        mapped = mix(gray, mapped, max(pp.grading_saturation, 0.0));
        mapped = (mapped - vec3<f32>(0.5)) * max(pp.grading_contrast, 0.0) + vec3<f32>(0.5);
        mapped = pow(max(mapped, vec3<f32>(0.0)), vec3<f32>(1.0 / max(pp.grading_gamma, 0.001)));
    }
    if pp.output_srgb_encode != 0u {
        mapped = linear_to_srgb(mapped);
    }
    return vec4<f32>(mapped, 1.0);
}
