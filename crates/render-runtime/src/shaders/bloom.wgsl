// Multi-resolution bloom prefilter, downsample, upsample, and HDR composite.
// Bloom stays in linear HDR and is tone-mapped only after the pyramid has been
// reconstructed and added back to scene color.

struct BloomUniforms {
    threshold: f32,
    intensity: f32,
    radius: f32,
    _padding: f32,
}

@group(0) @binding(0) var source_a: texture_2d<f32>;
@group(0) @binding(1) var source_b: texture_2d<f32>;
@group(0) @binding(2) var bloom_sampler: sampler;
@group(0) @binding(3) var<uniform> bloom: BloomUniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(2.0, 0.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[index], 0.0, 1.0);
    output.uv = uvs[index];
    return output;
}

fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn source_a_box(uv: vec2<f32>) -> vec3<f32> {
    let texel = vec2<f32>(1.0) / vec2<f32>(textureDimensions(source_a));
    let spread = 0.5 + max(bloom.radius, 0.0) * 0.125;
    let offset = texel * spread;
    return 0.25 * (
        textureSample(source_a, bloom_sampler, uv + vec2<f32>(-offset.x, -offset.y)).rgb
        + textureSample(source_a, bloom_sampler, uv + vec2<f32>(offset.x, -offset.y)).rgb
        + textureSample(source_a, bloom_sampler, uv + vec2<f32>(-offset.x, offset.y)).rgb
        + textureSample(source_a, bloom_sampler, uv + vec2<f32>(offset.x, offset.y)).rgb
    );
}

fn source_b_tent(uv: vec2<f32>) -> vec3<f32> {
    let texel = vec2<f32>(1.0) / vec2<f32>(textureDimensions(source_b));
    let spread = max(1.0, bloom.radius * 0.25);
    let offset = texel * spread;
    let center = textureSample(source_b, bloom_sampler, uv).rgb * 4.0;
    let cross = (
        textureSample(source_b, bloom_sampler, uv + vec2<f32>(offset.x, 0.0)).rgb
        + textureSample(source_b, bloom_sampler, uv - vec2<f32>(offset.x, 0.0)).rgb
        + textureSample(source_b, bloom_sampler, uv + vec2<f32>(0.0, offset.y)).rgb
        + textureSample(source_b, bloom_sampler, uv - vec2<f32>(0.0, offset.y)).rgb
    ) * 2.0;
    let diagonals =
        textureSample(source_b, bloom_sampler, uv + vec2<f32>(offset.x, offset.y)).rgb
        + textureSample(source_b, bloom_sampler, uv + vec2<f32>(offset.x, -offset.y)).rgb
        + textureSample(source_b, bloom_sampler, uv + vec2<f32>(-offset.x, offset.y)).rgb
        + textureSample(source_b, bloom_sampler, uv - offset).rgb;
    return (center + cross + diagonals) * (1.0 / 16.0);
}

@fragment
fn fs_prefilter(input: VertexOutput) -> @location(0) vec4<f32> {
    let color = source_a_box(input.uv);
    let brightness = luminance(color);
    let contribution = max(brightness - bloom.threshold, 0.0) / max(brightness, 1.0e-5);
    return vec4<f32>(color * contribution, 1.0);
}

@fragment
fn fs_downsample(input: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(source_a_box(input.uv), 1.0);
}

@fragment
fn fs_upsample(input: VertexOutput) -> @location(0) vec4<f32> {
    let high_frequency = textureSample(source_a, bloom_sampler, input.uv).rgb;
    let low_frequency = source_b_tent(input.uv);
    let scatter = clamp(bloom.radius * 0.125, 0.0, 1.0);
    return vec4<f32>(high_frequency + low_frequency * scatter, 1.0);
}

@fragment
fn fs_composite(input: VertexOutput) -> @location(0) vec4<f32> {
    let scene = textureSample(source_a, bloom_sampler, input.uv).rgb;
    let reconstructed_bloom = textureSample(source_b, bloom_sampler, input.uv).rgb;
    return vec4<f32>(scene + reconstructed_bloom * bloom.intensity, 1.0);
}
