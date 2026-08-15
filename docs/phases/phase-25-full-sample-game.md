# Phase 25 — Full Sample Game

## Goal

Demonstrate the complete engine feature set from Phases 20–24 in a single
playable example: WASD movement, AABB coin collection, enemy patrol AI, a
60-second countdown timer, HUD overlay, coin pickup sound effects, and a looping
BGM.

## Scope

| Feature | Location |
|---|---|
| 5 collectible coins (AABB + despawn) | `coin_pickup_system` |
| Patrolling red enemy (Z-axis bounce) | `enemy_patrol_system` |
| 60-second countdown timer | `game_timer_system` |
| Score + timer HUD (egui overlay) | `ScoreHud` (`UiSystem`) |
| Coin pickup SE (880 Hz WAV) | `coin_audio_system` (desktop only) |
| Looping BGM (110 Hz WAV) | `setup_audio` (desktop only) |
| Programmatic mesh/material creation | `make_box_mesh`, `make_ground_mesh` |

## Key Design Decisions

### Programmatic scene construction
The sample game builds its scene directly on the ECS world rather than loading
an authoring scene.  This keeps the example self-contained and exercises the
low-level `World::spawn` / `World::add_component` API that the editor runtime
also uses.

### Collision detection via CollisionEvents
`collision_detection_system` runs on the fixed-update schedule and writes all
overlapping AABB pairs into the `CollisionEvents` resource.
`coin_pickup_system` reads that resource on the normal-update schedule to
determine which coins to despawn.  A local `HashSet` of already-despawned
entities prevents double-decrement if the same coin appears in multiple events
within one step.

### Audio gated behind `#[cfg(not(target_arch = "wasm32"))]`
`AudioSystem`, `CoinSe`, `coin_audio_system`, and `setup_audio` are all
excluded from WASM builds so the example compiles on all targets.  On desktop,
`setup_audio` initialises rodio, starts the BGM loop, and returns `false` on
any initialisation failure — the game continues without audio rather than
panicking.

### HUD only visible in editor Game View
`App::run_ui_systems` is called by the editor's `RuntimePlayState` after each
wgpu frame, so `ScoreHud` draws on top of the game texture.  Standalone
execution skips the HUD call; the game logic runs correctly but no egui overlay
is shown.

### Sine-wave WAV generation
`sine_wav(freq_hz, duration_secs)` builds a valid 16-bit mono 44.1 kHz RIFF
WAV in memory.  No asset files are required; the bytes are fed directly to
`AudioAsset::from_bytes`.

## Completion Criteria

- `cargo run --example sample_game` opens a 1280×720 window with a green ground
  plane, a blue player, five gold coins, and a red patrolling enemy.
- WASD moves the player; walking over a coin despawns it.
- The editor Game View shows "Coins: N / 5" and "Time: 60s" updating in
  real time when Play is pressed.
- Collecting a coin plays a short beep; a 110 Hz tone loops as BGM.
- When all coins are collected the HUD shows "YOU WIN!"; when time runs out it
  shows "TIME UP!".
- `cargo test --workspace` passes.
