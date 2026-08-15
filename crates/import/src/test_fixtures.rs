//! Opt-in model-import fixtures shared by cross-crate acceptance tests.

/// One triangle skinned to a two-joint skeleton with one rotation clip.
pub const SKINNED_GLTF: &str = r#"{
    "asset": {"version": "2.0"},
    "scene": 0,
    "scenes": [{"nodes": [0, 2]}],
    "nodes": [
        {"name": "root_joint", "children": [1]},
        {"name": "tip_joint", "translation": [0.0, 1.0, 0.0]},
        {"name": "character", "mesh": 0, "skin": 0}
    ],
    "skins": [{
        "name": "skeleton",
        "joints": [0, 1],
        "inverseBindMatrices": 3
    }],
    "meshes": [{
        "name": "triangle",
        "primitives": [{
            "attributes": {"POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2}
        }]
    }],
    "animations": [{
        "name": "spin",
        "channels": [
            {"sampler": 0, "target": {"node": 1, "path": "rotation"}},
            {"sampler": 1, "target": {"node": 2, "path": "translation"}}
        ],
        "samplers": [
            {"input": 4, "output": 5, "interpolation": "LINEAR"},
            {"input": 4, "output": 6, "interpolation": "LINEAR"}
        ]
    }],
    "accessors": [
        {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
         "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]},
        {"bufferView": 1, "componentType": 5123, "count": 3, "type": "VEC4"},
        {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4"},
        {"bufferView": 3, "componentType": 5126, "count": 2, "type": "MAT4"},
        {"bufferView": 4, "componentType": 5126, "count": 2, "type": "SCALAR",
         "min": [0.0], "max": [1.0]},
        {"bufferView": 5, "componentType": 5126, "count": 2, "type": "VEC4"},
        {"bufferView": 0, "componentType": 5126, "count": 2, "type": "VEC3",
         "min": [0.0, 0.0, 0.0], "max": [1.0, 0.0, 0.0]}
    ],
    "bufferViews": [
        {"buffer": 0, "byteOffset": 0, "byteLength": 36},
        {"buffer": 0, "byteOffset": 36, "byteLength": 24},
        {"buffer": 0, "byteOffset": 60, "byteLength": 48},
        {"buffer": 0, "byteOffset": 108, "byteLength": 128},
        {"buffer": 0, "byteOffset": 236, "byteLength": 8},
        {"buffer": 0, "byteOffset": 244, "byteLength": 32}
    ],
    "buffers": [{
        "byteLength": 276,
        "uri": "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAABAAAAAAAAAAEAAAAAAAAAAQAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAD8AAAA/AAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAgD8AAAAAAAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAAAAAAAAAAAAgD8AAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAPMENT/zBDU/"
    }]
}"#;

/// Produces one self-contained character GLB with three named clips and a texture.
pub fn three_clip_character_glb() -> Vec<u8> {
    let mut document: serde_json::Value =
        serde_json::from_str(SKINNED_GLTF).expect("skinned fixture JSON");
    let uri = document["buffers"][0]["uri"]
        .as_str()
        .expect("embedded fixture buffer");
    let mut binary = decode_base64(uri.split_once(',').expect("data URI separator").1);
    document["buffers"][0]
        .as_object_mut()
        .expect("buffer object")
        .remove("uri");

    let animation = document["animations"][0].clone();
    document["animations"] = serde_json::Value::Array(
        ["idle", "attack", "damage"]
            .into_iter()
            .map(|name| {
                let mut animation = animation.clone();
                animation["name"] = serde_json::Value::String(name.into());
                animation
            })
            .collect(),
    );

    while !binary.len().is_multiple_of(4) {
        binary.push(0);
    }
    let image_offset = binary.len();
    let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([80, 120, 200, 255]),
    ));
    let mut png = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .expect("PNG fixture encoding");
    let png = png.into_inner();
    let png_length = png.len();
    binary.extend_from_slice(&png);
    while !binary.len().is_multiple_of(4) {
        binary.push(0);
    }

    document["bufferViews"]
        .as_array_mut()
        .expect("buffer views")
        .push(serde_json::json!({
            "buffer": 0,
            "byteOffset": image_offset,
            "byteLength": png_length
        }));
    document["buffers"][0]["byteLength"] = serde_json::json!(binary.len());
    document["images"] = serde_json::json!([{
        "bufferView": 6,
        "mimeType": "image/png",
        "name": "armor_albedo"
    }]);
    document["textures"] = serde_json::json!([{
        "source": 0,
        "name": "armor_albedo"
    }]);
    document["materials"] = serde_json::json!([{
        "name": "armor",
        "pbrMetallicRoughness": {
            "baseColorTexture": {"index": 0},
            "roughnessFactor": 0.45,
            "metallicFactor": 0.6
        }
    }]);
    document["meshes"][0]["primitives"][0]["material"] = serde_json::json!(0);

    let mut json = serde_json::to_vec(&document).expect("GLB JSON serialization");
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total_length = 12 + 8 + json.len() + 8 + binary.len();
    let mut glb = Vec::with_capacity(total_length);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2_u32.to_le_bytes());
    glb.extend_from_slice(&(total_length as u32).to_le_bytes());
    glb.extend_from_slice(&(json.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534A_u32.to_le_bytes());
    glb.extend_from_slice(&json);
    glb.extend_from_slice(&(binary.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942_u32.to_le_bytes());
    glb.extend_from_slice(&binary);
    glb
}

fn decode_base64(encoded: &str) -> Vec<u8> {
    let mut output = Vec::new();
    let mut bits = 0_u32;
    let mut bit_count = 0_u8;
    for byte in encoded.bytes().filter(|byte| *byte != b'=') {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        bits = (bits << 6) | u32::from(value);
        bit_count += 6;
        if bit_count >= 8 {
            bit_count -= 8;
            output.push((bits >> bit_count) as u8);
            bits &= (1_u32 << bit_count) - 1;
        }
    }
    output
}
