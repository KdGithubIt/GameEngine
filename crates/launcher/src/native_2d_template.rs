//! First-party Native 2D proving-project template for ADR 0127.
//!
//! The Launcher owns only template presentation/orchestration. The standard
//! project scaffold remains authoritative; this module builds the 2D proving
//! content in a sibling staging project and publishes it with one final rename.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const TEXTURE_ASSET: &str = "asset_00000000000000000000000001";
const ATLAS_ASSET: &str = "asset_00000000000000000000000002";
const ANIMATION_ASSET: &str = "asset_00000000000000000000000003";
const TILE_SET_ASSET: &str = "asset_00000000000000000000000004";
const TILE_MAP_ASSET: &str = "asset_00000000000000000000000005";
const PLAYER_IDLE_SPRITE: &str = "sprite_00000000000000000000000001";
const PLAYER_STEP_SPRITE: &str = "sprite_00000000000000000000000002";
const SOLID_TILE: &str = "tile_00000000000000000000000001";
const ONE_WAY_TILE: &str = "tile_00000000000000000000000002";
const WORLD_LAYER: &str = "tile_layer_00000000000000000000000001";
const SORTING_LAYER: &str = "sorting_layer_00000000000000000000000000";

/// Project archetype offered by the Launcher.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProjectTemplate {
    /// Minimal standard project with an empty start scene.
    #[default]
    Empty,
    /// Playable-authoring proving content for the Native 2D first release.
    Native2d,
}

impl ProjectTemplate {
    pub fn description(self) -> &'static str {
        match self {
            Self::Empty => "Empty scene and standard project scaffold.",
            Self::Native2d => "Camera2D, sprites, animation, tiles, 2D bodies and one-way collision.",
        }
    }
}

/// Creates the ADR 0127 first-party 2D proving project without exposing a
/// partially-authored final directory.
pub fn create_native_2d_project(final_path: &Path, name: &str) -> Result<PathBuf, String> {
    if final_path.exists() {
        return Err(format!("destination already exists: {}", final_path.display()));
    }
    let parent = final_path
        .parent()
        .ok_or_else(|| "project destination has no parent directory".to_owned())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before UNIX epoch: {error}"))?
        .as_nanos();
    let staging = parent.join(format!(
        ".gameengine-native2d-{}-{stamp}-{}.staging",
        std::process::id(),
        name.replace('/', "_").replace('\\', "_")
    ));
    if staging.exists() {
        return Err(format!("temporary project path already exists: {}", staging.display()));
    }

    let result = (|| -> Result<PathBuf, String> {
        let project = engine_project_lifecycle::create_standard_project(&staging, name)
            .map_err(|error| error.to_string())?;
        write_proving_content(project.path())?;
        drop(project);
        fs::rename(&staging, final_path).map_err(|error| {
            format!(
                "could not publish Native 2D project {}: {error}",
                final_path.display()
            )
        })?;
        let opened = engine_project_lifecycle::inspect_project(final_path)
            .map_err(|error| format!("created project did not reopen cleanly: {error}"))?;
        Ok(opened.path().to_path_buf())
    })();

    if result.is_err() && staging.exists() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn write_proving_content(root: &Path) -> Result<(), String> {
    let native_dir = root.join("assets").join("native_2d");
    fs::create_dir_all(&native_dir)
        .map_err(|error| format!("could not create Native 2D asset directory: {error}"))?;

    fs::write(
        root.join("assets").join("textures").join("native_2d_proving.bmp"),
        proving_texture_bmp(),
    )
    .map_err(|error| format!("could not write proving texture: {error}"))?;

    write_json(
        &native_dir.join("proving.spriteatlas.json"),
        &json!({
            "schema_version": 1,
            "regions": [
                {
                    "id": PLAYER_IDLE_SPRITE,
                    "name": "Player Idle",
                    "source_texture": TEXTURE_ASSET,
                    "rect": { "x": 0, "y": 0, "width": 32, "height": 32 },
                    "pivot": [0.5, 0.5],
                    "pixels_per_unit": { "override": 32.0 },
                    "filtering": "nearest",
                    "extrusion_pixels": 0
                },
                {
                    "id": PLAYER_STEP_SPRITE,
                    "name": "Player Step",
                    "source_texture": TEXTURE_ASSET,
                    "rect": { "x": 32, "y": 0, "width": 32, "height": 32 },
                    "pivot": [0.5, 0.5],
                    "pixels_per_unit": { "override": 32.0 },
                    "filtering": "nearest",
                    "extrusion_pixels": 0
                }
            ]
        }),
    )?;
    write_json(
        &native_dir.join("player.spriteanim.json"),
        &json!({
            "schema_version": 1,
            "ticks_per_second": 12,
            "looping": true,
            "default_speed": 1.0,
            "frames": [
                { "sprite": { "atlas": ATLAS_ASSET, "sprite": PLAYER_IDLE_SPRITE }, "duration_ticks": 3 },
                { "sprite": { "atlas": ATLAS_ASSET, "sprite": PLAYER_STEP_SPRITE }, "duration_ticks": 3, "event": "footstep" }
            ]
        }),
    )?;
    write_json(
        &native_dir.join("proving.tileset.json"),
        &json!({
            "schema_version": 1,
            "tiles": [
                {
                    "id": SOLID_TILE,
                    "name": "Solid",
                    "sprite": { "atlas": ATLAS_ASSET, "sprite": PLAYER_IDLE_SPRITE },
                    "collision": [{ "kind": "box", "half_extents": [0.5, 0.5] }],
                    "collision_material": { "friction": 0.7, "restitution": 0.0 },
                    "one_way": false,
                    "tags": ["ground"],
                    "custom_values": {}
                },
                {
                    "id": ONE_WAY_TILE,
                    "name": "One Way",
                    "sprite": { "atlas": ATLAS_ASSET, "sprite": PLAYER_STEP_SPRITE },
                    "collision": [{ "kind": "box", "half_extents": [0.5, 0.18] }],
                    "collision_material": { "friction": 0.5, "restitution": 0.0 },
                    "one_way": true,
                    "tags": ["platform"],
                    "custom_values": {}
                }
            ]
        }),
    )?;

    let cells = (0..15)
        .map(|x| json!({ "cell": { "x": x, "y": 0 }, "tile": SOLID_TILE }))
        .collect::<Vec<_>>();
    write_json(
        &native_dir.join("proving.tilemap.json"),
        &json!({
            "schema_version": 1,
            "tile_set": TILE_SET_ASSET,
            "chunk_size": 32,
            "layers": [{
                "id": WORLD_LAYER,
                "name": "World",
                "enabled": true,
                "locked": false,
                "sorting_layer": SORTING_LAYER,
                "order_in_layer": -10,
                "chunks": [{ "coord": { "x": 0, "y": 0 }, "cells": cells }]
            }]
        }),
    )?;

    patch_project_settings(root)?;
    write_json(&root.join("asset_manifest.json"), &manifest_json())?;
    write_json(
        &root.join("assets").join("scenes").join("main.scene.json"),
        &scene_json(),
    )?;
    fs::write(
        root.join("NATIVE_2D_PROVING.md"),
        "# Native 2D proving project\n\nThis Launcher template intentionally puts ADR 0127 systems in one small scene: Camera2D pixel framing, stable SpriteRef rendering, deterministic Sprite Animation events, sparse TileMap rendering/collision, a CharacterController2D body, and a one-way platform. Open the Native 2D workspace to edit the atlas, animation, Tile Set, and Tile Map, then package the project through the normal Build workflow.\n",
    )
    .map_err(|error| format!("could not write proving-project notes: {error}"))?;
    Ok(())
}

fn patch_project_settings(root: &Path) -> Result<(), String> {
    let path = root.join("project_settings.json");
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("could not read project settings: {error}"))?;
    let mut settings: Value = serde_json::from_str(&text)
        .map_err(|error| format!("could not parse project settings: {error}"))?;
    settings["native_2d"] = json!({
        "default_pixels_per_unit": 32.0,
        "default_filtering": "nearest",
        "gravity": [0.0, -9.81],
        "pixel_preview": "pixel_perfect",
        "sorting_layers": [{ "id": SORTING_LAYER, "name": "Default" }]
    });
    write_json(&path, &settings)
}

fn manifest_json() -> Value {
    let mut assets = serde_json::Map::new();
    assets.insert(
        TEXTURE_ASSET.to_owned(),
        json!({ "path": "textures/native_2d_proving.bmp", "name": "Native 2D Proving Texture" }),
    );
    assets.insert(
        ATLAS_ASSET.to_owned(),
        json!({ "path": "native_2d/proving.spriteatlas.json", "name": "Proving Sprite Atlas" }),
    );
    assets.insert(
        ANIMATION_ASSET.to_owned(),
        json!({ "path": "native_2d/player.spriteanim.json", "name": "Player Walk" }),
    );
    assets.insert(
        TILE_SET_ASSET.to_owned(),
        json!({ "path": "native_2d/proving.tileset.json", "name": "Proving Tile Set" }),
    );
    assets.insert(
        TILE_MAP_ASSET.to_owned(),
        json!({ "path": "native_2d/proving.tilemap.json", "name": "Proving Tile Map" }),
    );
    json!({ "schema_version": 2, "assets": assets })
}

fn transform(x: f64, y: f64, z: f64, scale_x: f64, scale_y: f64) -> Value {
    json!({
        "x": x, "y": y, "z": z,
        "rotation_x_degrees": 0.0, "rotation_y_degrees": 0.0, "rotation_z_degrees": 0.0,
        "scale_x": scale_x, "scale_y": scale_y, "scale_z": 1.0
    })
}

fn scene_json() -> Value {
    let asset_ref = |id: &str| json!({ "$type": "asset_ref", "id": id });
    json!({
        "schema_version": 1,
        "entities": [
            {
                "id": "entity_00000000000000000000000001",
                "name": "camera_2d", "display_name": "Camera 2D",
                "description": "Pixel-perfect proving camera.",
                "components": {
                    "engine.transform": transform(0.0, 1.0, 10.0, 1.0, 1.0),
                    "engine.camera_2d": {
                        "enabled": true, "priority": 10, "orthographic_height": 8.0, "zoom": 1.0,
                        "near": -1000.0, "far": 1000.0, "pixel_perfect": true,
                        "reference_pixels_per_unit": 32.0, "reference_width": 320, "reference_height": 180, "fit": "fit"
                    }
                }
            },
            {
                "id": "entity_00000000000000000000000002",
                "name": "player", "display_name": "Player",
                "description": "CharacterController2D + SpriteAnimator2D proving entity.",
                "components": {
                    "engine.transform": transform(0.0, 1.0, 0.0, 1.0, 1.0),
                    "engine.sprite_renderer_2d": {
                        "atlas": asset_ref(ATLAS_ASSET), "sprite_id": PLAYER_IDLE_SPRITE,
                        "tint_r": 1.0, "tint_g": 1.0, "tint_b": 1.0, "tint_a": 1.0,
                        "flip_x": false, "flip_y": false, "sorting_layer": SORTING_LAYER,
                        "order_in_layer": 10, "visible": true, "blend": "alpha"
                    },
                    "engine.sprite_animator_2d": {
                        "clip": asset_ref(ANIMATION_ASSET), "autoplay": true, "speed": 1.0, "initial_frame": 0
                    },
                    "engine.collider_2d": {
                        "shape": "box", "half_extent_x": 0.42, "half_extent_y": 0.48,
                        "radius": 0.5, "half_height": 0.5, "points": [], "sensor": false,
                        "friction": 0.4, "restitution": 0.0, "membership": 1, "mask": 4294967295_i64, "one_way": false
                    },
                    "engine.rigid_body_2d": {
                        "mode": "kinematic", "velocity_x": 0.0, "velocity_y": 0.0,
                        "angular_velocity": 0.0, "gravity_scale": 1.0, "continuous": true
                    },
                    "engine.character_controller_2d": {
                        "half_extent_x": 0.42, "half_extent_y": 0.48, "skin": 0.02,
                        "slope_limit_degrees": 45.0, "ground_snap": 0.12, "collision_mask": 4294967295_i64
                    }
                }
            },
            {
                "id": "entity_00000000000000000000000003",
                "name": "tile_world", "display_name": "Tile World",
                "description": "Sparse chunked TileMap2D proving layer.",
                "components": {
                    "engine.transform": transform(-7.0, -1.5, 0.0, 1.0, 1.0),
                    "engine.tile_map_2d": { "tile_map": asset_ref(TILE_MAP_ASSET), "visible": true }
                }
            },
            {
                "id": "entity_00000000000000000000000004",
                "name": "one_way_platform", "display_name": "One-way Platform",
                "description": "Standalone one-way Collider2D proving surface.",
                "components": {
                    "engine.transform": transform(3.0, 1.5, 0.0, 3.0, 0.4),
                    "engine.sprite_renderer_2d": {
                        "atlas": asset_ref(ATLAS_ASSET), "sprite_id": PLAYER_STEP_SPRITE,
                        "tint_r": 1.0, "tint_g": 1.0, "tint_b": 1.0, "tint_a": 1.0,
                        "flip_x": false, "flip_y": false, "sorting_layer": SORTING_LAYER,
                        "order_in_layer": 1, "visible": true, "blend": "alpha"
                    },
                    "engine.collider_2d": {
                        "shape": "box", "half_extent_x": 0.5, "half_extent_y": 0.5,
                        "radius": 0.5, "half_height": 0.5, "points": [], "sensor": false,
                        "friction": 0.5, "restitution": 0.0, "membership": 1, "mask": 4294967295_i64, "one_way": true
                    },
                    "engine.rigid_body_2d": {
                        "mode": "fixed", "velocity_x": 0.0, "velocity_y": 0.0,
                        "angular_velocity": 0.0, "gravity_scale": 0.0, "continuous": false
                    }
                }
            }
        ]
    })
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("could not serialize {}: {error}", path.display()))?;
    text.push('\n');
    fs::write(path, text).map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn proving_texture_bmp() -> Vec<u8> {
    const WIDTH: usize = 64;
    const HEIGHT: usize = 32;
    const ROW_BYTES: usize = WIDTH * 3;
    const PIXEL_BYTES: usize = ROW_BYTES * HEIGHT;
    const FILE_BYTES: usize = 54 + PIXEL_BYTES;
    let mut bytes = vec![0_u8; FILE_BYTES];
    bytes[0..2].copy_from_slice(b"BM");
    bytes[2..6].copy_from_slice(&(FILE_BYTES as u32).to_le_bytes());
    bytes[10..14].copy_from_slice(&54_u32.to_le_bytes());
    bytes[14..18].copy_from_slice(&40_u32.to_le_bytes());
    bytes[18..22].copy_from_slice(&(WIDTH as i32).to_le_bytes());
    bytes[22..26].copy_from_slice(&(HEIGHT as i32).to_le_bytes());
    bytes[26..28].copy_from_slice(&1_u16.to_le_bytes());
    bytes[28..30].copy_from_slice(&24_u16.to_le_bytes());
    bytes[34..38].copy_from_slice(&(PIXEL_BYTES as u32).to_le_bytes());
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let offset = 54 + y * ROW_BYTES + x * 3;
            let (red, green, blue) = if x < WIDTH / 2 {
                (50_u8, 205_u8, 245_u8)
            } else {
                (245_u8, 125_u8, 70_u8)
            };
            bytes[offset] = blue;
            bytes[offset + 1] = green;
            bytes[offset + 2] = red;
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proving_texture_is_valid_24_bit_bmp_shape() {
        let bmp = proving_texture_bmp();
        assert_eq!(&bmp[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[18..22].try_into().unwrap()), 64);
        assert_eq!(u32::from_le_bytes(bmp[22..26].try_into().unwrap()), 32);
        assert_eq!(u16::from_le_bytes(bmp[28..30].try_into().unwrap()), 24);
        assert_eq!(bmp.len(), u32::from_le_bytes(bmp[2..6].try_into().unwrap()) as usize);
    }

    #[test]
    fn proving_documents_keep_stable_cross_references() {
        let scene = scene_json().to_string();
        let manifest = manifest_json().to_string();
        assert!(scene.contains(ATLAS_ASSET));
        assert!(scene.contains(ANIMATION_ASSET));
        assert!(scene.contains(TILE_MAP_ASSET));
        assert!(manifest.contains(TEXTURE_ASSET));
        assert!(manifest.contains(TILE_SET_ASSET));
    }
}
