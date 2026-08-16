// Multi-resolution HDR bloom.
// Bright extraction and all filtering stay in linear HDR before tonemapping.

struct BloomUniforms {
    threshold: f32,
    radius: f32,
    intensity: f32,
    _padding: f32,
}

@group(0) @binding(0) var primary_texture: texture_2d<f32>;
@group(0) @binding(1) var secondary_texture: texture_2d<f32>;
@group(0) @binding(2) var bloom_sampler: sampler;
@group(0) @binding(3) var<uniform> bloom: BloomUniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
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
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = uvs[vertex_index];
    return output;
}

fn luminance(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

fn tent_primary(uv: vec2<f32>, radius: f32) -> vec3<f32> {
    let dimensions = vec2<f32>(textureDimensions(primary_texture));
    let texel = vec2<f32>(1.0) / dimensions;
    let offset = texel * radius;

    let center = textureSample(primary_texture, bloom_sampler, uv).rgb * 4.0;
    let axis =
        textureSample(primary_texture, bloom_sampler, uv + vec2<f32>( offset.x, 0.0)).rgb * 2.0 +
        textureSample(primary_texture, bloom_sampler, uv + vec2<f32>(-offset.x, 0.0)).rgb * 2.0 +
        textureSample(primary_texture, bloom_sampler, uv + vec2<f32>(0.0,  offset.y)).rgb * 2.0 +
        textureSample(primary_texture, bloom_sampler, uv + vec2<f32>(0.0, -offset.y)).rgb * 2.0;
    let diagonal =
        textureSample(primary_texture, bloom_sampler, uv + vec2<f32>( offset.x,  offset.y)).rgb +
        textureSample(primary_texture, bloom_sampler, uv + vec2<f32>(-offset.x,  offset.y)).rgb +
        textureSample(primary_texture, bloom_sampler, uv + vec2<f32>( offset.x, -offset.y)).rgb +
        textureSample(primary_texture, bloom_sampler, uv + vec2<f32>(-offset.x, -offset.y)).rgb;
    return (center + axis + diagonal) / 16.0;
}

fn tent_secondary(uv: vec2<f32>, radius: f32) -> vec3<f32> {
    let dimensions = vec2<f32>(textureDimensions(secondary_texture));
    let texel = vec2<f32>(1.0) / dimensions;
    let offset = texel * radius;

    let center = textureSample(secondary_texture, bloom_sampler, uv).rgb * 4.0;
    let axis =
        textureSample(secondary_texture, bloom_sampler, uv + vec2<f32>( offset.x, 0.0)).rgb * 2.0 +
        textureSample(secondary_texture, bloom_sampler, uv + vec2<f32>(-offset.x, 0.0)).rgb * 2.0 +
        textureSample(secondary_texture, bloom_sampler, uv + vec2<f32>(0.0,  offset.y)).rgb * 2.0 +
        textureSample(secondary_texture, bloom_sampler, uv + vec2<f32>(0.0, -offset.y)).rgb * 2.0;
    let diagonal =
        textureSample(secondary_texture, bloom_sampler, uv + vec2<f32>( offset.x,  offset.y)).rgb +
        textureSample(secondary_texture, bloom_sampler, uv + vec2<f32>(-offset.x,  offset.y)).rgb +
        textureSample(secondary_texture, bloom_sampler, uv + vec2<f32>( offset.x, -offset.y)).rgb +
        textureSample(secondary_texture, bloom_sampler, uv + vec2<f32>(-offset.x, -offset.y)).rgb;
    return (center + axis + diagonal) / 16.0;
}

@fragment
fn fs_downsample(input: VertexOutput) -> @location(0) vec4<f32> {
    let filtered = tent_primary(input.uv, bloom.radius);
    if luminance(filtered) <= bloom.threshold {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let extracted = max(filtered - vec3<f32>(bloom.threshold), vec3<f32>(0.0));
    return vec4<f32>(extracted, 1.0);
}

@fragment
fn fs_upsample(input: VertexOutput) -> @location(0) vec4<f32> {
    let current = textureSample(primary_texture, bloom_sampler, input.uv).rgb;
    let broader = tent_secondary(input.uv, bloom.radius);
    return vec4<f32>((current + broader) * 0.5, 1.0);
}

@fragment
fn fs_composite(input: VertexOutput) -> @location(0) vec4<f32> {
    let hdr = textureSample(primary_texture, bloom_sampler, input.uv).rgb;
    let bloom_color = textureSample(secondary_texture, bloom_sampler, input.uv).rgb;
    return vec4<f32>(hdr + bloom_color * bloom.intensity, 1.0);
}
