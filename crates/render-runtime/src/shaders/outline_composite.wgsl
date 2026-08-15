// Fullscreen outline composite.
//
// A source pixel normally draws only into empty space or a different hierarchy
// group. A material may opt into a reduced boundary against a different
// material under the same root, allowing skin/clothing separation without
// restoring every overlapping mesh seam.

@group(0) @binding(0) var t_outline_style: texture_2d<f32>;
@group(0) @binding(1) var t_outline_group: texture_2d<u32>;

const OUTLINE_REFERENCE_HEIGHT: f32 = 1024.0;
const OUTLINE_MAX_RADIUS_REFERENCE_TEXELS: f32 = 4.0;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );

    var output: VertexOutput;
    output.clip_position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = positions[vertex_index] * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let dimensions = vec2<i32>(textureDimensions(t_outline_group));
    let pixel = clamp(
        vec2<i32>(input.uv * vec2<f32>(dimensions)),
        vec2<i32>(0),
        dimensions - vec2<i32>(1),
    );
    let current_identity = textureLoad(t_outline_group, pixel, 0).xy;
    let mask_density = f32(dimensions.y) / OUTLINE_REFERENCE_HEIGHT;
    let max_radius = OUTLINE_MAX_RADIUS_REFERENCE_TEXELS * mask_density;
    let search_radius = i32(ceil(max_radius));
    var best_distance = 1000.0;
    var best_color = vec3<f32>(0.0);
    var best_strength = 1.0;

    for (var y: i32 = -search_radius; y <= search_radius; y++) {
        for (var x: i32 = -search_radius; x <= search_radius; x++) {
            if (x == 0 && y == 0) {
                continue;
            }

            let distance = length(vec2<f32>(f32(x), f32(y)));
            if (distance > max_radius) {
                continue;
            }

            let sample_pixel = pixel + vec2<i32>(x, y);
            if (any(sample_pixel < vec2<i32>(0)) || any(sample_pixel >= dimensions)) {
                continue;
            }

            let sample_identity = textureLoad(t_outline_group, sample_pixel, 0).xy;
            if (sample_identity.x == 0u) {
                continue;
            }

            let style = textureLoad(t_outline_style, sample_pixel, 0);
            var boundary_strength = 1.0;
            if (sample_identity.x == current_identity.x) {
                let sample_material = sample_identity.y & 0x00ffffffu;
                let current_material = current_identity.y & 0x00ffffffu;
                boundary_strength = f32(sample_identity.y >> 24u) / 255.0;
                if (sample_material == current_material || boundary_strength <= 0.0) {
                    continue;
                }
            }

            let radius = style.a * max_radius * boundary_strength;
            if (radius > 0.0 && distance <= radius && distance < best_distance) {
                best_distance = distance;
                best_color = style.rgb;
                best_strength = boundary_strength;
            }
        }
    }

    if (best_distance > max_radius) {
        discard;
    }
    return vec4<f32>(best_color, best_strength);
}
