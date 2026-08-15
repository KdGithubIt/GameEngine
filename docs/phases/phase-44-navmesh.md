# Phase 44: Navigation Mesh & Pathfinding

> **着手前に ADR 0039 必須**（NavMesh ライブラリ選択と bake 戦略）。

## Goal

障害物を考慮した経路探索を追加する。`NavMeshAgent` component を持つ entity が
target 位置まで自動的に経路を計算して移動できる。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

Phase 25 のフル sample game では敵 AI が直線移動しかできない。
Phase 42（Scripting）の後に置くことで、スクリプトから
`ctx.nav_move_toward(target_pos)` を呼ぶ API をシームレスに整備できる。
Collision（Phase 21）が前提で、Phase 44 ではコリジョンメッシュからの
NavMesh 生成を行う。

---

## Scope

### 作るもの

- NavMesh 生成（offline bake、ADR 0039 ゲート）（44-A）
- A* Pathfinding — `NavMeshQuery` resource（44-B）
- `NavMeshAgent` component — target 指定で自動移動（44-B）
- Editor: Scene View での NavMesh ワイヤーフレーム表示 + Bake ボタン（44-C）

### 作らないもの

- ランタイム NavMesh 再生成（動的障害物対応）
- 複数エージェントの衝突回避（RVO 等）
- 3D 傾斜面・段差の高精度対応
- NavLink（ジャンプポイント等）

---

## Design Decisions

### NavMesh ライブラリは ADR 0039 で決定（候補: recast-rs, oxidized_navigation）

| 選択肢 | 特徴 |
|--------|------|
| `recast-rs` | C++ Recast/Detour バインディング。実績あり。ビルド複雑 |
| `oxidized_navigation` | Pure Rust、Bevy 由来。WASM 親和性高い。WIP |

ADR 0039 で比較して Accept してからコードに触る。

### Bake は project_root 直下の `navmesh.bin` に保存

editor の "Bake NavMesh" ボタンが静的バイナリを生成。
runtime はそのファイルをロードして使う。動的再生成は作らない。

### pathfinding は A\* on NavMesh polygons

ポリゴングラフ上の A\*（ヒューリスティック: ユークリッド距離）。
string pulling（Funnel algorithm）で straight-line パスに変換する。

### `NavMeshAgent` は fixed update で動く

Phase 21（12-B Fixed Timestep）の `FixedTime` を使い、
物理・衝突と同じタイムステップで経路追従する。

---

## Implementation Plan

### 44-A: NavMesh 生成（ADR 0039 ゲート）

1. `crates/engine/src/navmesh.rs`（新規）
2. `NavMeshSettings { cell_size, cell_height, agent_radius, agent_height }` resource
3. `bake_navmesh(world, settings, output_path)` — editor から呼ぶ offline 関数
4. bake 結果を `navmesh.bin`（bincode か独自フォーマット、ADR 0039 で決定）へ保存

### 44-B: Pathfinding & NavMeshAgent

```rust
pub struct NavMeshQuery { /* 読み込み済み NavMesh を保持 */ }
impl NavMeshQuery {
    pub fn find_path(&self, start: Vec3, end: Vec3) -> Option<Vec<Vec3>>;
}

pub struct NavMeshAgent {
    pub target: Option<Vec3>,
    pub speed: f32,
    pub stopping_distance: f32,
    current_path: Vec<Vec3>,
    path_index: usize,
}
```

- `nav_mesh_agent_system` を fixed update に登録
- パスが空またはターゲット到達で停止
- 再経路計算は target 変更時のみ（毎フレーム再計算しない）

### 44-C: Editor Integration

- Scene View で NavMesh を緑のワイヤーフレームで描画（`DebugLines` 利用）
- Editor toolbar に「Bake NavMesh」ボタン → `bake_navmesh()` 実行
- `NavMeshAgent` component が Inspector で追加・編集できる
  （`target`, `speed`, `stopping_distance` フィールド）

---

## Cautions（注意点・落とし穴）

**大規模レベルの bake 時間**:
recast の bake はサイズに比例する。editor がフリーズしないよう
bake を別スレッドで実行し、完了を Diagnostic で通知する。

**NavMesh と Collision の食い違い**:
NavMesh は bake 時点のシーンを反映する。Collider を後から追加した場合は
再 bake が必要であることをドキュメントに明記する。

**A\* の経路が壁を貫通する**:
NavMesh ポリゴンのエッジを正しくリンクしないと起きる。
bake 後にサンプルパスを可視化してデバッグする。

---

## Prohibited（禁止事項）

- ADR 0039 の Accept 前にコードを書くことを禁止
- ランタイム NavMesh 再生成をこのフェーズで実装することを禁止
- RVO / steering behavior の本格実装をこのフェーズで行うことを禁止

---

## Completion Criteria（完了基準）

- 障害物を避けながら `NavMeshAgent` がターゲットまで移動する
- Editor の "Bake NavMesh" で `navmesh.bin` が生成される
- Scene View で NavMesh ワイヤーフレームが表示される
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 42: Scripting — `ctx.nav_move_toward(target_pos)` の実装
- Phase 47: Instancing — NavMesh 上の群衆描画でインスタンシングが効く
