struct CameraUniform {
    view_proj: mat4x4<f32>,
    world_position: vec3<f32>,
    viewport_aspect: f32,
    view: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: CameraUniform;

@group(1) @binding(0)
var sprite_texture: texture_2d<f32>;

@group(1) @binding(1)
var sprite_sampler: sampler;

struct SpriteInstance {
    @location(0) model_0: vec4<f32>,
    @location(1) model_1: vec4<f32>,
    @location(2) model_2: vec4<f32>,
    @location(3) model_3: vec4<f32>,
    @location(4) size_pivot: vec4<f32>,
    @location(5) uv_rect: vec4<f32>,
    @location(6) tint: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
};

fn quad_corner(vertex_index: u32) -> vec2<f32> {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    return corners[vertex_index];
}

@vertex
fn vs_main(instance: SpriteInstance, @builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let corner = quad_corner(vertex_index);
    let size = instance.size_pivot.xy;
    let pivot = instance.size_pivot.zw;
    let local = vec4<f32>((corner - pivot) * size, 0.0, 1.0);
    let model = mat4x4<f32>(instance.model_0, instance.model_1, instance.model_2, instance.model_3);

    var output: VertexOutput;
    output.clip_position = camera.view_proj * model * local;
    output.uv = mix(instance.uv_rect.xy, instance.uv_rect.zw, corner);
    output.tint = instance.tint;
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(sprite_texture, sprite_sampler, input.uv) * input.tint;
}
