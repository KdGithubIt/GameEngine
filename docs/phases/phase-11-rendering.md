# Phase 11: Rendering + Camera Basics

**Current status (2026-06-11)**: 11-D-1 and 11-B are complete. `Vertex`
now includes object-space normals, the mesh shader accepts the updated
layout, and `Mesh::cube()` / `Mesh::plane()` / `Mesh::sphere()` are
implemented with validation tests. Next work: 11-C camera controllers or
11-D-2 lighting resources and WGSL lighting.

## Goal

Game View で 3D シーンがまともに見えるようにする。  
深度・ライト・メッシュプリミティブ・デバッグ描画を整備する。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**深度バッファは既に実装済み**:  
`render.rs` の pipeline に `depth_stencil: Some(DepthStencilState { depth_write_enabled: true, ... })` が設定済み。  
`app.rs` の `GpuState` が depth texture を作成し、リサイズ時に再作成している。  
Phase 11-A の作業は**不要**。すでに動いている。

**ライティングを Phase 11 に入れる理由**:  
ライティングなしでは全オブジェクトがフラットに見える。どのメッシュが前でどのメッシュが後ろかが
分かりにくく、ゲームプレイの確認が難しい。Game View が動いた直後にライティングを追加する。

**プリミティブとライティングを同一フェーズにまとめる理由**:  
ライティング追加には `Vertex` に `normal` フィールドの追加が必要。これは破壊的変更で、
`Mesh::triangle()` / `Mesh::quad()` / mesh.wgsl / RenderPipeline が全部変わる。  
`Mesh::cube()` 等のプリミティブも normals が必要なので、同じタイミングで実装する。

**Debug Draw を Phase 11 に入れる理由**:  
Phase 21（collision、旧 17）で collider の形を確認するための debug draw が必要。  
collision の前に debug draw を作っておかないと、実装が正しいか確認できない。

---

## Scope

### 作るもの

- `Mesh::cube()` / `Mesh::plane()` / `Mesh::sphere()` プリミティブ
- `Vertex` への `normal` フィールド追加（**破壊的変更**）
- `AmbientLight` / `DirectionalLight` ECS リソース
- Phong シェーディング（WGSL shader 更新）
- Light uniform buffer（render pipeline 更新）
- `DebugLines` リソースと描画システム
- `OrbitCamera` / `FollowCamera` コンポーネント + システム

### 作らないもの

- Point Light / Spot Light（DirectionalLight のみ）
- Shadow（複雑すぎる・後回し）
- PBR シェーディング（Phong で十分）
- WASM での動作確認（後回し）

---

## Design Decisions

### なぜ `Vertex` に `normal` を追加するのか（Blinn-Phong に必要）

頂点ごとに法線ベクトルを持つことで、ライトの方向と面の向きを GPU で計算できる。  
法線がない場合は全面が同じ明るさになりライティングの意味がない。

### なぜ法線変換に逆転置行列を使うべきか（でも最初は使わなくてよい）

モデル行列に非一様スケール（X だけ 2 倍等）がある場合、法線を `model * normal` で変換すると
法線が歪む。正しくは `(model^-1)^T * normal` を使う。  
ただし最初は一様スケールしか使わない前提で `model` の 3x3 部分で変換してよい。
非一様スケールへの対応は必要になった時点で追加する。

### なぜ DebugLines を毎フレームクリアするか

デバッグ描画は「今フレームに描きたいもの」を指定する pull モデル。前フレームの線が残ると
消えた collider の残影が画面に残り続けてデバッグの邪魔になる。

### `cube()` で共有頂点を使わない理由

立方体の各面で法線の向きが違う（+X / -X / +Y / -Y / +Z / -Z）。  
エッジを共有する頂点は同じ位置でも法線が違うため、面ごとに 4 頂点を持つ必要がある（計 24 頂点）。  
共有頂点を使うと法線が平均化されて角が丸く見える（スムーズシェーディング）。
ハードエッジの立方体には共有頂点は不適。

---

## Implementation Plan

> **実装順序（`docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` の正規順序に準拠）**:
> 11-A(確認のみ) → **11-D-1**(Vertex normal 追加・破壊的変更) → **11-B**(primitives) →
> **11-C**(camera) → **11-D-2**(lighting WGSL) → **11-E**(debug draw)
> 11-D-1 は 11-B の前提（primitives の全メッシュが `normal` フィールドを使用するため）。

### 11-A: 深度バッファ — 作業不要

既に `render.rs` の pipeline と `app.rs` の `GpuState` に実装済み。確認のみ。

### 11-D-1: Vertex に normal を追加（破壊的変更・11-D の前半）

**影響ファイル一覧（全部同一 PR で更新）:**
- `crates/engine/src/mesh.rs` — `Vertex` struct + `LAYOUT`
- `crates/engine/src/mesh.rs` — `Mesh::triangle()` / `Mesh::quad()` の全頂点に `normal` を追加
- `crates/engine/src/shaders/mesh.wgsl` — `VertexInput` / `VertexOutput` / vs_main / fs_main
- `crates/engine/src/render.rs` — `RenderState::new()` のパイプラインレイアウトに light BGL 追加

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal:   [f32; 3],   // 追加
    pub color:    [f32; 3],
    pub uv:       [f32; 2],
}

impl Vertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3,  // position
            1 => Float32x3,  // normal   ← 追加
            2 => Float32x3,  // color
            3 => Float32x2,  // uv
        ],
    };
}
```

既存メッシュへの normal 追加:
- `Mesh::triangle()` → 全頂点 `normal: [0.0, 0.0, 1.0]`（+Z 向き）
- `Mesh::quad()` → 全頂点 `normal: [0.0, 0.0, 1.0]`（+Z 向き）

### 11-B: メッシュプリミティブ追加（前提: 11-D-1 完了後）

`crates/engine/src/mesh.rs` に追加:

**`Mesh::cube()`**: 24 頂点 + 36 インデックス
- 6 面 × 4 頂点（各面で法線が一意）
- 各面の法線: +X, -X, +Y, -Y, +Z, -Z
- UV: 各面 `(0,0)-(1,1)` のフル UV

**`Mesh::plane(width: f32, depth: f32)`**: 4 頂点 + 6 インデックス
- XZ 平面、Y=0 に配置
- 法線: `[0.0, 1.0, 0.0]`（+Y 向き）

**`Mesh::sphere(rings: u32, sectors: u32)`**: UV 球
- `(rings+1) * (sectors+1)` 頂点
- 頂点計算: `theta = PI * ring / rings`, `phi = 2*PI * sector / sectors`
  - `x = sin(theta) * cos(phi)`, `y = cos(theta)`, `z = sin(theta) * sin(phi)`
- 法線 = 位置ベクトルを正規化（球面なので一致する）
- 推奨デフォルト: `Mesh::sphere(16, 32)`

### 11-D-2: Light リソースと WGSL 更新（11-D の後半）

```rust
// crates/engine/src/light.rs（新規）
pub struct AmbientLight { pub color: Vec3, pub intensity: f32 }
pub struct DirectionalLight {
    pub direction: Vec3,   // 正規化済み、光が向かう方向
    pub color: Vec3,
    pub intensity: f32,
}

impl Default for AmbientLight {
    fn default() -> Self { Self { color: Vec3::ONE, intensity: 0.15 } }
}
impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            direction: Vec3::new(-0.5, -1.0, -0.3).normalize(),
            color: Vec3::ONE,
            intensity: 1.0,
        }
    }
}
```

`App::new()` で自動挿入:
```rust
ecs.insert_resource(AmbientLight::default());
ecs.insert_resource(DirectionalLight::default());
```

WGSL シェーダーの更新概要:
```wgsl
struct LightUniform {
    ambient_color:    vec3<f32>,
    ambient_intensity: f32,
    dir_direction:    vec3<f32>,
    dir_intensity:    f32,
    dir_color:        vec3<f32>,
}
@group(3) @binding(0) var<uniform> light: LightUniform;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex = textureSample(t_diffuse, s_diffuse, in.uv) * object.color * vec4(in.color, 1.0);
    let n = normalize(in.world_normal);
    let l = normalize(-light.dir_direction);
    let diffuse = max(dot(n, l), 0.0) * light.dir_color * light.dir_intensity;
    let ambient = light.ambient_color * light.ambient_intensity;
    return vec4((ambient + diffuse) * tex.rgb, tex.a);
}
```

`render.rs` 側:
- `LightUniform` の uniform buffer を追加（bind group 3）
- `GpuState::render()` で `AmbientLight` / `DirectionalLight` リソースを読み取り、buffer を更新

### 11-E: Debug Draw

```rust
// crates/engine/src/debug_draw.rs（新規）
pub struct DebugLine { pub from: Vec3, pub to: Vec3, pub color: [f32; 4] }

pub struct DebugLines { pub lines: Vec<DebugLine> }

impl DebugLines {
    pub fn line(&mut self, from: Vec3, to: Vec3, color: Vec3);
    pub fn aabb(&mut self, center: Vec3, half_extents: Vec3, color: Vec3);
    // 12 本のラインでボックスを描く
    pub fn axes(&mut self, transform: &Transform, length: f32);
    // X(赤) / Y(緑) / Z(青) の 3 本
}
```

GPU 実装:
- `PrimitiveTopology::LineList` の専用パイプライン
- 動的頂点バッファ（毎フレーム `write_buffer`）
- メインレンダーパスの後に描画（`DebugLines` が空なら描画をスキップ）
- フレーム終了後に `DebugLines.lines.clear()`

### 11-C: カメラコントローラー

```rust
// crates/engine/src/camera.rs に追加
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    pub yaw: f32,    // ラジアン
    pub pitch: f32,  // ラジアン（上下）
    pub pitch_min: f32,
    pub pitch_max: f32,
    pub orbit_speed: f32,
    pub zoom_speed: f32,
}

pub struct FollowCamera {
    pub target: engine_ecs::Entity,
    pub offset: Vec3,
    pub spring_strength: f32,  // 0.0 = 即時追従
}
```

システム:
- `orbit_camera_system`: 右クリックドラッグで yaw/pitch 変更、スクロールで distance 変更
- `follow_camera_system`: target entity の `GlobalTransform` + offset に向かって lerp

---

## Cautions（注意点・落とし穴）

**`Vertex` 変更後に `bytemuck::cast_slice` が壊れないか確認**:  
`Vertex` は `Pod` + `Zeroable` derive が必要。`normal` フィールドを追加した後も
`#[repr(C)]` が保たれ、`Pod` 条件を満たすことを確認する。

**shader の `group(3)` の追加順序**:  
`pipeline_layout` の `bind_group_layouts` は `[camera, texture, model, light]` の順。
shader の `@group(0..3)` に対応する。順序がずれると validation error になる。

**Debug Draw の頂点バッファサイズ**:  
毎フレームラインが増減するため、バッファを固定サイズにすると overflow する。
`write_buffer` は必要サイズ以下のバッファには書けない。フレームごとに必要なサイズで
バッファを再作成するか、大きめの固定バッファを確保してラインが多すぎる場合は切り捨てる。

**OrbitCamera の pitch の gimbal lock**:  
pitch を `±PI/2` 付近まで近づけると gimbal lock が起きる。`pitch_min` / `pitch_max` で
`-PI/2 + 0.05` から `PI/2 - 0.05` に制限する。

---

## Prohibited（禁止事項）

- `Vertex` への `normal` 追加と他の変更を別 PR に分割することを禁止（全呼び出し元を同一 PR で更新）
- Shadow の実装（複雑すぎる・このフェーズのスコープ外）
- PBR の実装（Phong で十分・PBR は Phase 26+〔Advanced Authoring〕で検討）
- `DebugLines` の `lines` を毎フレームクリアし忘れることを禁止（前フレームの残影が残る）

---

## Completion Criteria（完了基準）

- 複数の `Mesh::cube()` を前後に置いても正しい Z 順で描画される
- cube / sphere の面の向きで明るさが変わる（ライティングが効いている）
- `app.add_system(debug_draw::clear_system)` で DebugLines がクリアされる
- `DebugLines::aabb()` で描いたボックスが Game View に表示される
- `cargo test --workspace` が通る（`Vertex` 変更後も既存テストが壊れない）

---

## Feeds Into（次フェーズへの依存）

- Phase 13: PlayerController と OrbitCamera / FollowCamera を組み合わせてゲームを操作する
- Phase 21（旧 17）: `DebugLines::aabb()` を使って collider を可視化する
