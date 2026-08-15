// Procedural gradient sky drawn as a fullscreen triangle before scene
// geometry. Depth writes stay off so meshes cover the sky wherever they
// pass the depth test.

struct SkyUniform {
    inv_view_proj: mat4x4<f32>,
    zenith: vec4<f32>,
    horizon: vec4<f32>,
    ground: vec4<f32>,
};

@group(0) @binding(0) var<uniform> sky: SkyUniform;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // One oversized triangle covers the viewport without a vertex buffer.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var out: VsOut;
    let p = positions[index];
    out.position = vec4<f32>(p, 1.0, 1.0);
    out.ndc = p;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Reconstruct the world-space view ray for this pixel.
    let near = sky.inv_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
    let far = sky.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let direction = normalize(far.xyz / far.w - near.xyz / near.w);
    let up = clamp(direction.y, -1.0, 1.0);
    var color: vec3<f32>;
    if (up >= 0.0) {
        color = mix(sky.horizon.rgb, sky.zenith.rgb, pow(up, 0.55));
    } else {
        color = mix(sky.horizon.rgb, sky.ground.rgb, pow(-up, 0.6));
    }
    return vec4<f32>(color, 1.0);
}
