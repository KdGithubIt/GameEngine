# Phase 13: Minimal Playable Vertical Slice

## Goal

GUI で編集した scene を Play し、キーボードでプレイヤーを動かし、カメラで追える。  
「エディタでゲームを作れる感」が出る最初のマイルストーン。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**Phase 9〜12 の統合検証**:  
このフェーズは新機能を追加するというよりも、Phase 9（editor）・10（Game View）・
11（rendering）・12（fixed update）を組み合わせて、最初の「ゲームっぽい体験」を作る。
統合することで見えてくる問題（APIの使いにくさ、システム間の依存、パフォーマンス等）を
早期に発見して修正する。

**collision / physics より先にこのフェーズが来る理由**:  
PlayerController と Camera Controller は collision がなくても実装できる。
壁を通り抜けながらでも「動く・カメラが追う」ことは確認できる。
基本的な動きを確認してから、Phase 21-22（旧 17-18）で当たり判定を追加する方が開発しやすい。

**なぜ `PlayerController` をエンジンの組み込みにするか**:  
`minimal_playable.rs` の `player_move_system` は example のローカル関数として書かれている。
エンジンの組み込みにすることで、editor で scene を組み立てるだけで動くようになり、
毎回同じコードを書く必要がなくなる。

---

## Scope

### 作るもの

- `PlayerController` コンポーネント + `player_controller_system`
- `OrbitCamera` / `FollowCamera` コンポーネント + システム（Phase 11 で追加する）
- editor の Scene Hierarchy から `PlayerController` を entity に追加できること

### 作らないもの

- Collision（Phase 21、旧 17）
- 壁を通り抜けない処理（Phase 21-22）
- ジャンプ（重力が Phase 22 まで作れない）

---

## Design Decisions

### なぜ `PlayerController` を自動登録しないか

`App::new()` で `player_controller_system` を自動登録すると、プレイヤーのいないゲームでも
無駄にシステムが動く。ユーザーが `app.add_system(player_controller_system)` で明示的に登録する。
また editor で Play する際は `PlaySetup` の中で自動登録する（PlayerMarker を持つ entity があれば）。

### なぜ `MovePlane` で XZ / XY を選べるようにするか

3D ゲームでは XZ 平面（床の上を歩く）が一般的。  
2D ゲームや横スクロールでは XY 平面が必要。  
同じ `PlayerController` コンポーネントを両方のゲームで再利用できるようにする。

### なぜ `FollowCamera.spring_strength` を 0.0〜1.0 にするか

`spring_strength = 0.0` で target と同じ動きをする（遅延なし）。  
`spring_strength = 0.8` でゆっくり追いかける（3D アクション的な動き）。  
`lerp(current, target, 1.0 - spring_strength)` で実装できる。  
物理的に正確なバネではないが、直感的に調整できる。

### BT（Behavior Tree）の Phase 13 での確認方針

Phase 7 で BT の ECS 統合は完了している。このフェーズでは「Play 時に BT がある entity があれば
`register_behavior_tree_system` を呼ぶ」という接続を確認するだけ。  
BT の本格的なゲーム統合は Phase 25（フル sample game、旧 20）で行う。

---

## Implementation Plan

### 13-A: PlayerController

`crates/engine/src/player.rs` に追加（または既存の `PlayerMarker` と同じファイル）:

```rust
pub struct PlayerController {
    pub move_speed: f32,
    pub move_plane: MovePlane,
}

pub enum MovePlane {
    XZ,  // 3D ゲーム（床の上を歩く）
    XY,  // 2D ゲーム / 横スクロール
}

impl Default for PlayerController {
    fn default() -> Self {
        Self { move_speed: 3.0, move_plane: MovePlane::XZ }
    }
}

pub fn player_controller_system(
    keyboard: Res<Input<KeyCode>>,
    time: Res<Time>,
    mut query: Query<(&PlayerController, &mut Transform), With<PlayerMarker>>,
) {
    for (_, (controller, transform)) in &mut query {
        let speed = controller.move_speed * time.delta_seconds;
        let (fwd, right, up) = match controller.move_plane {
            MovePlane::XZ => (Vec3::NEG_Z, Vec3::X, Vec3::Y),
            MovePlane::XY => (Vec3::Y, Vec3::X, Vec3::Z),
        };
        if keyboard.pressed(KeyCode::KeyW) { transform.translation += fwd * speed; }
        if keyboard.pressed(KeyCode::KeyS) { transform.translation -= fwd * speed; }
        if keyboard.pressed(KeyCode::KeyA) { transform.translation -= right * speed; }
        if keyboard.pressed(KeyCode::KeyD) { transform.translation += right * speed; }
    }
}
```

### 13-B: Camera Controllers（Phase 11 で実装する OrbitCamera / FollowCamera を動作確認）

Phase 11 で追加した `orbit_camera_system` / `follow_camera_system` が正しく動くことを確認。  
必要であれば調整。新規実装はない。

### 13-C: Editor-authored Playable Scene

Phase 9-10-11-12 が完了していれば追加の実装は不要。  
検証: editor で以下の flow が動くことを確認する。

```
1. editor で scene を新規作成
2. player entity を追加、PlayerMarker + PlayerController + Transform を付与
3. camera entity を追加、Camera3D + OrbitCamera + Transform を付与
4. cube entity を追加（床として）、Mesh::plane() + Transform を付与
5. Ctrl+S で scene を保存
6. Play を押す
7. Game View に scene が表示される
8. WASD でプレイヤーが動く
9. マウスドラッグで camera が軌道を描く
```

### 13-D: BT Runtime チェック

Play 開始時のセットアップに追加:
```rust
// BehaviorTreeRunner を持つ entity が world にある場合のみ登録
if has_behavior_tree_entities(&world) {
    register_behavior_tree_system(&mut world);
}
```

または: 常に `register_behavior_tree_system` を呼ぶ（コストは無視できる）。

---

## Cautions（注意点・落とし穴）

**`PlayerController` と `OrbitCamera` の競合**:  
`OrbitCamera` がマウスドラッグを消費すると、`PlayerController` が mouse look に使えなくなる。
Phase 13 では FPS スタイルのマウス look は実装しない。WASD + orbit camera の組み合わせのみ。

**`FollowCamera` の `target: Entity` が despawn されたとき**:  
target entity が消えると `world.get_component::<GlobalTransform>(target)` が失敗する。
`follow_camera_system` は target が見つからない場合は何もしない（panic しない）。

**delta_seconds が 0 の最初のフレーム**:  
アプリ起動直後の最初のフレームは `delta_seconds = 0.0`。
`speed = move_speed * delta_seconds` が 0 になり動かない。問題ない（次フレームから動く）。

---

## Prohibited（禁止事項）

- `PlayerController` を `App::new()` で自動登録することを禁止（ユーザーが明示的に登録する）
- collision なしで「壁を通り抜けない」を実装しようとすることを禁止（Phase 21-22 の仕事）
- このフェーズで Action Mapping を実装することを禁止（Phase 25 まで待つ）

---

## Completion Criteria（完了基準）

- editor で scene を作り、Play して WASD でプレイヤーが動く
- OrbitCamera / FollowCamera のどちらかが Game View でプレイヤーを追える
- Stop で editor に戻り、authoring scene が変わっていない
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 14: editor で mesh/texture アセットを entity に割り当てて Play で表示される
- Phase 21（旧 17）: PlayerController で動くプレイヤーが壁に当たって止まる
