// Procedural gradient sky drawn as a fullscreen triangle before scene
// geometry. Depth writes stay off so meshes cover the sky wherever they
// pass the depth test.

struct SkyUniform {
    inv_view_proj: mat4x4<f32>,
    zenith: vec4<f32>,
    horizon: vec4<f32>,
    ground: vec4<f32>,
};

struct EnvironmentUniform {
    diffuse_color: vec3<f32>,
    intensity: f32,
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> sky: SkyUniform;
@group(1) @binding(0) var t_environment_source: texture_2d<f32>;
@group(1) @binding(4) var s_environment: sampler;
@group(1) @binding(5) var<uniform> environment: EnvironmentUniform;

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

fn direction_uv(direction: vec3<f32>) -> vec2<f32> {
    let n = normalize(direction);
    let u = fract(atan2(n.z, n.x) / 6.283185307179586 + 0.5);
    let v = acos(clamp(n.y, -1.0, 1.0)) / 3.141592653589793;
    return vec2<f32>(u, v);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Reconstruct the world-space view ray for this pixel.
    let near = sky.inv_view_proj * vec4<f32>(in.ndc, 0.0, 1.0);
    let far = sky.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let direction = normalize(far.xyz / far.w - near.xyz / near.w);
    if (environment.params.w > 0.5) {
        let radiance = textureSampleLevel(
            t_environment_source,
            s_environment,
            direction_uv(direction),
            0.0,
        ).rgb;
        return vec4<f32>(radiance * environment.intensity, 1.0);
    }

    let up = clamp(direction.y, -1.0, 1.0);
    var color: vec3<f32>;
    if (up >= 0.0) {
        color = mix(sky.horizon.rgb, sky.zenith.rgb, pow(up, 0.55));
    } else {
        color = mix(sky.horizon.rgb, sky.ground.rgb, pow(-up, 0.6));
    }
    return vec4<f32>(color, 1.0);
}
