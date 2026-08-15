//! Phase 25 — Full Sample Game
//!
//! Collect all five gold coins before the 60-second timer runs out.
//! WASD moves the blue player.  The red enemy patrols; avoid it.
//! Score and timer appear as a HUD overlay in the editor Game View.

use std::collections::HashSet;

use egui::{Align2, Area, Color32, Id, Vec2};

use engine::ecs::{Entity, World};
use engine::glam::Vec3;
use engine::time::Time;
use engine::{
    character_controller_system, collision_detection_system, player_character_motor_system,
    player_controller_system, App, Assets, Camera3D, Collider, CollisionEvents, Commands,
    GlobalTransform, Handle, KinematicCharacterController, Material, Mesh, MovePlane, PhysicsBody,
    PlayerController, PlayerMarker, Query, Res, ResMut, Transform, UiSystem, Vertex,
};

#[cfg(not(target_arch = "wasm32"))]
use engine::{AudioAsset, AudioSystem};

// ── constants ────────────────────────────────────────────────────────────────

const GAME_DURATION: f32 = 60.0;
const ENEMY_SPEED: f32 = 3.0;

const COIN_POSITIONS: [(f32, f32); 5] = [
    (-4.0, -4.0),
    (4.0, -4.0),
    (0.0, -7.0),
    (-5.0, 0.0),
    (5.0, 0.0),
];

// ── resources ────────────────────────────────────────────────────────────────

struct Score {
    collected: u32,
    total: u32,
}

struct GameTimer {
    remaining: f32,
}

struct GameState {
    active: bool,
}

struct LastScore(u32);

#[cfg(not(target_arch = "wasm32"))]
struct CoinSe(AudioAsset);

// ── components ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Coin;

struct Enemy {
    patrol_start_z: f32,
    patrol_end_z: f32,
    direction: f32,
}

// ── HUD ──────────────────────────────────────────────────────────────────────

struct ScoreHud;

impl UiSystem for ScoreHud {
    fn run(&mut self, ctx: &egui::Context, world: &World) {
        let (collected, total) = world
            .get_resource::<Score>()
            .map(|s| (s.collected, s.total))
            .unwrap_or((0, 0));
        let remaining = world
            .get_resource::<GameTimer>()
            .map(|t| t.remaining.max(0.0))
            .unwrap_or(0.0);
        let active = world
            .get_resource::<GameState>()
            .map(|s| s.active)
            .unwrap_or(false);

        Area::new(Id::new("hud_score"))
            .fixed_pos(egui::pos2(10.0, 10.0))
            .show(ctx, |ui| {
                ui.visuals_mut().override_text_color = Some(Color32::WHITE);
                ui.label(format!("Coins: {collected} / {total}"));
                ui.label(format!("Time:  {remaining:.0}s"));
            });

        if !active {
            Area::new(Id::new("hud_over"))
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.visuals_mut().override_text_color = Some(Color32::YELLOW);
                    if collected >= total && total > 0 {
                        ui.heading("YOU WIN!");
                    } else {
                        ui.heading("TIME UP!");
                    }
                });
        }
    }
}

// ── systems ──────────────────────────────────────────────────────────────────

fn coin_pickup_system(
    events: Res<CollisionEvents>,
    coin_query: Query<&Coin>,
    player_query: Query<&PlayerMarker>,
    mut score: ResMut<Score>,
    mut commands: Commands,
) {
    let coins: HashSet<Entity> = coin_query.iter().map(|(e, _)| e).collect();
    let players: HashSet<Entity> = player_query.iter().map(|(e, _)| e).collect();
    let mut despawned: HashSet<Entity> = HashSet::new();

    for event in events.iter() {
        let coin = if coins.contains(&event.entity_a) && players.contains(&event.entity_b) {
            Some(event.entity_a)
        } else if coins.contains(&event.entity_b) && players.contains(&event.entity_a) {
            Some(event.entity_b)
        } else {
            None
        };

        if let Some(coin_entity) = coin
            && despawned.insert(coin_entity) {
                commands.despawn(coin_entity);
                score.collected += 1;
            }
    }
}

fn enemy_patrol_system(time: Res<Time>, mut query: Query<(&mut Enemy, &mut Transform)>) {
    for (_, (enemy, transform)) in &mut query {
        transform.translation.z += enemy.direction * ENEMY_SPEED * time.delta_seconds;
        if transform.translation.z >= enemy.patrol_end_z {
            transform.translation.z = enemy.patrol_end_z;
            enemy.direction = -1.0;
        } else if transform.translation.z <= enemy.patrol_start_z {
            transform.translation.z = enemy.patrol_start_z;
            enemy.direction = 1.0;
        }
    }
}

fn game_timer_system(time: Res<Time>, mut timer: ResMut<GameTimer>, mut state: ResMut<GameState>) {
    if state.active {
        timer.remaining -= time.delta_seconds;
        if timer.remaining <= 0.0 {
            timer.remaining = 0.0;
            state.active = false;
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn coin_audio_system(
    score: Res<Score>,
    mut last: ResMut<LastScore>,
    audio: Res<AudioSystem>,
    coin_se: Res<CoinSe>,
) {
    if score.collected > last.0 {
        let _ = audio.play_se(&coin_se.0);
        last.0 = score.collected;
    }
}

// ── mesh helpers ─────────────────────────────────────────────────────────────

fn make_box_mesh(hx: f32, hy: f32, hz: f32, color: [f32; 3]) -> Mesh {
    let face_defs: [([f32; 3], [[f32; 3]; 4]); 6] = [
        (
            [1.0, 0.0, 0.0],
            [[hx, -hy, -hz], [hx, hy, -hz], [hx, hy, hz], [hx, -hy, hz]],
        ),
        (
            [-1.0, 0.0, 0.0],
            [
                [-hx, -hy, hz],
                [-hx, hy, hz],
                [-hx, hy, -hz],
                [-hx, -hy, -hz],
            ],
        ),
        (
            [0.0, 1.0, 0.0],
            [[-hx, hy, -hz], [hx, hy, -hz], [hx, hy, hz], [-hx, hy, hz]],
        ),
        (
            [0.0, -1.0, 0.0],
            [
                [-hx, -hy, hz],
                [hx, -hy, hz],
                [hx, -hy, -hz],
                [-hx, -hy, -hz],
            ],
        ),
        (
            [0.0, 0.0, 1.0],
            [[hx, -hy, hz], [hx, hy, hz], [-hx, hy, hz], [-hx, -hy, hz]],
        ),
        (
            [0.0, 0.0, -1.0],
            [
                [-hx, -hy, -hz],
                [-hx, hy, -hz],
                [hx, hy, -hz],
                [hx, -hy, -hz],
            ],
        ),
    ];

    let corner_uvs = [[0.0_f32, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);

    for (face_i, (normal, positions)) in face_defs.iter().enumerate() {
        let base = (face_i * 4) as u32;
        for j in 0..4 {
            vertices.push(Vertex {
                position: positions[j],
                normal: *normal,
                color,
                uv: corner_uvs[j],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    Mesh {
        vertices,
        indices: Some(indices),
        skinning: None,
        tangents: None,
        submeshes: Vec::new(),
    }
}

fn make_ground_mesh(width: f32, depth: f32, color: [f32; 3]) -> Mesh {
    let hx = width * 0.5;
    let hz = depth * 0.5;
    let normal = [0.0_f32, 1.0, 0.0];
    Mesh {
        vertices: vec![
            Vertex {
                position: [-hx, 0.0, -hz],
                normal,
                color,
                uv: [0.0, 0.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [hx, 0.0, -hz],
                normal,
                color,
                uv: [1.0, 0.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [hx, 0.0, hz],
                normal,
                color,
                uv: [1.0, 1.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
            Vertex {
                position: [-hx, 0.0, hz],
                normal,
                color,
                uv: [0.0, 1.0],
                outline_scale: 1.0,
                additional_uv: [0.0; 2],
            },
        ],
        indices: Some(vec![0, 1, 2, 0, 2, 3]),
        skinning: None,
        tangents: None,
        submeshes: Vec::new(),
    }
}

// ── audio helpers ─────────────────────────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
fn sine_wav(freq_hz: f32, duration_secs: f32) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 44_100;
    let num_samples = (SAMPLE_RATE as f32 * duration_secs) as usize;
    let data_size = (num_samples * 2) as u32;
    let riff_size = 36 + data_size;

    let mut buf = Vec::with_capacity(44 + data_size as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&riff_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    buf.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for i in 0..num_samples {
        let t = i as f32 / SAMPLE_RATE as f32;
        let sample = (f32::sin(std::f32::consts::TAU * freq_hz * t) * 16_000.0) as i16;
        buf.extend_from_slice(&sample.to_le_bytes());
    }
    buf
}

#[cfg(not(target_arch = "wasm32"))]
fn setup_audio(app: &mut App) -> bool {
    let coin_se = match AudioAsset::from_bytes(sine_wav(880.0, 0.15)) {
        Ok(a) => a,
        Err(_) => return false,
    };
    let bgm = match AudioAsset::from_bytes(sine_wav(110.0, 1.0)) {
        Ok(a) => a,
        Err(_) => return false,
    };
    let mut audio = match AudioSystem::new() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let _ = audio.play_bgm(&bgm);
    app.insert_resource(CoinSe(coin_se));
    app.insert_resource(audio);
    true
}

// ── spawn helpers ─────────────────────────────────────────────────────────────

fn add_mesh(world: &mut engine::ecs::World, mesh: Mesh) -> Handle<Mesh> {
    world
        .get_resource_mut::<Assets<Mesh>>()
        .expect("Assets<Mesh> must be present")
        .add(mesh)
}

fn add_material(world: &mut engine::ecs::World, mat: Material) -> Handle<Material> {
    world
        .get_resource_mut::<Assets<Material>>()
        .expect("Assets<Material> must be present")
        .add(mat)
}

// ── entry point ───────────────────────────────────────────────────────────────

fn main() {
    let mut app = App::new()
        .with_title("Full Sample Game — Collect the coins!")
        .with_size(1280, 720);

    {
        let world = app.world_mut();

        world.insert_resource(Score {
            collected: 0,
            total: COIN_POSITIONS.len() as u32,
        });
        world.insert_resource(GameTimer {
            remaining: GAME_DURATION,
        });
        world.insert_resource(GameState { active: true });
        world.insert_resource(LastScore(0));
        world.insert_resource(CollisionEvents::default());

        // Player — blue box
        let player_mesh = add_mesh(world, make_box_mesh(0.4, 0.5, 0.4, [0.2, 0.4, 1.0]));
        let player_mat = add_material(world, Material::color(0.2, 0.4, 1.0));
        let player = world.spawn().expect("player spawn");
        world
            .add_component(
                player,
                Transform::from_translation(Vec3::new(0.0, 0.5, 2.0)),
            )
            .expect("player transform");
        world
            .add_component(player, GlobalTransform::default())
            .expect("player gt");
        world
            .add_component(player, PlayerMarker)
            .expect("player marker");
        world
            .add_component(
                player,
                PlayerController {
                    move_speed: 5.0,
                    move_plane: MovePlane::Xz,
                    ..PlayerController::default()
                },
            )
            .expect("player controller");
        world
            .add_component(player, KinematicCharacterController::default())
            .expect("player character motor");
        world
            .add_component(player, Collider::aabb(Vec3::new(0.4, 0.5, 0.4)))
            .expect("player collider");
        world
            .add_component(player, PhysicsBody::Kinematic)
            .expect("player body");
        world
            .add_component(player, player_mesh)
            .expect("player mesh handle");
        world
            .add_component(player, player_mat)
            .expect("player mat handle");

        // Coins — flat gold discs (thin boxes)
        for &(cx, cz) in &COIN_POSITIONS {
            let mesh = add_mesh(world, make_box_mesh(0.35, 0.12, 0.35, [1.0, 0.85, 0.0]));
            let mat = add_material(world, Material::color(1.0, 0.85, 0.0));
            let coin = world.spawn().expect("coin spawn");
            world
                .add_component(coin, Transform::from_translation(Vec3::new(cx, 0.12, cz)))
                .expect("coin transform");
            world
                .add_component(coin, GlobalTransform::default())
                .expect("coin gt");
            world.add_component(coin, Coin).expect("coin marker");
            world
                .add_component(coin, Collider::aabb(Vec3::new(0.35, 0.12, 0.35)))
                .expect("coin collider");
            world
                .add_component(coin, PhysicsBody::Static)
                .expect("coin body");
            world.add_component(coin, mesh).expect("coin mesh handle");
            world.add_component(coin, mat).expect("coin mat handle");
        }

        // Enemy — red box patrolling on Z axis
        let enemy_mesh = add_mesh(world, make_box_mesh(0.5, 0.7, 0.5, [1.0, 0.2, 0.2]));
        let enemy_mat = add_material(world, Material::color(1.0, 0.2, 0.2));
        let enemy_e = world.spawn().expect("enemy spawn");
        world
            .add_component(
                enemy_e,
                Transform::from_translation(Vec3::new(3.0, 0.7, -4.0)),
            )
            .expect("enemy transform");
        world
            .add_component(enemy_e, GlobalTransform::default())
            .expect("enemy gt");
        world
            .add_component(
                enemy_e,
                Enemy {
                    patrol_start_z: -7.0,
                    patrol_end_z: 3.0,
                    direction: 1.0,
                },
            )
            .expect("enemy component");
        world
            .add_component(enemy_e, enemy_mesh)
            .expect("enemy mesh handle");
        world
            .add_component(enemy_e, enemy_mat)
            .expect("enemy mat handle");

        // Ground — green quad
        let ground_mesh = add_mesh(world, make_ground_mesh(20.0, 20.0, [0.3, 0.5, 0.3]));
        let ground_mat = add_material(world, Material::color(0.3, 0.5, 0.3));
        let ground = world.spawn().expect("ground spawn");
        world
            .add_component(ground, Transform::default())
            .expect("ground transform");
        world
            .add_component(ground, GlobalTransform::default())
            .expect("ground gt");
        world
            .add_component(ground, ground_mesh)
            .expect("ground mesh handle");
        world
            .add_component(ground, ground_mat)
            .expect("ground mat handle");

        // Camera — looking down from above and behind
        let camera = world.spawn().expect("camera spawn");
        world
            .add_component(
                camera,
                Transform::from_translation(Vec3::new(0.0, 12.0, 14.0)),
            )
            .expect("camera transform");
        world
            .add_component(camera, GlobalTransform::default())
            .expect("camera gt");
        world
            .add_component(camera, Camera3D::default())
            .expect("camera component");
    }

    #[cfg(not(target_arch = "wasm32"))]
    let _audio_ready = setup_audio(&mut app);

    app.add_system(player_controller_system);
    app.add_system(coin_pickup_system);
    app.add_system(enemy_patrol_system);
    app.add_system(game_timer_system);
    app.ecs_mut()
        .add_fixed_system(player_character_motor_system);
    app.ecs_mut().add_fixed_system(character_controller_system);
    app.ecs_mut().add_fixed_system(collision_detection_system);

    #[cfg(not(target_arch = "wasm32"))]
    if _audio_ready {
        app.add_system(coin_audio_system);
    }

    app.add_ui_system(ScoreHud);
    app.run().expect("event loop must run");
}
