# Phase 21: Collision Detection

> **2026-06-13 再構成**: 旧 Phase 17。新 Phase 15〜20（エディタ実用化）の後に
> 実施する。先頭タスクとして 12-B Fixed Timestep を実装する。
> 旧→新対応表は `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` を参照。

## Goal

AABB（軸平行境界ボックス）の衝突判定を実装し、  
プレイヤーが壁を通り抜けない・当たり判定のあるゲームを作れるようにする。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**Phase 13 の PlayerController でプレイヤーは壁を通り抜ける**:  
Phase 13 では「動く・カメラが追う」を確認したが、衝突判定がなく壁を透過する。  
Phase 13 → Phase 16 でゲームの土台が固まった後、当たり判定を追加する。

**なぜ AABB から始めるか**:  
AABB は最も単純な衝突形状で実装コストが低い。  
`push-out`（押し返し）のロジックも AABB なら単純な重なり量の計算だけ。  
球体（Sphere）・カプセル（Capsule）は AABB の後で追加できる。  
OBB（向きを持つボックス）や mesh collision は Phase 21 以降。

**Fixed Update との関係**:  
衝突判定は Phase 12 で作った Fixed Update の中で実行する。  
可変フレームの update で行うと、フレームレートによって「すり抜け」の頻度が変わる。

**なぜ physics（Phase 18）より前か**:  
collision = 形状の重なりを検出して push-out する仕組み。  
physics = 重力・速度・加速度の計算。  
collision の基盤なしに physics を実装すると、物体が重なりっぱなしになる。

---

## Scope

### 作るもの

- `Collider` コンポーネント（`Aabb { half_extents: Vec3 }` variant から開始）
- `CollisionEvent` — 衝突した entity ペアのイベント
- ブロードフェーズ: 単純な O(n²) ペアチェック（entity 数が少い前提）
- ナローフェーズ: AABB 重なり判定 + push-out 量の計算
- `PhysicsBody` コンポーネント（Static / Kinematic）— physics 速度は Phase 18 に委譲
- `DebugLines` で collider の形を可視化（Phase 11 の debug draw を使う）

### 作らないもの

- 球体 / カプセル / Sphere collider（AABB のみ）
- OBB（向きを持つボックス）
- Mesh collision
- Continuous Collision Detection（CCD）— 高速移動時のすり抜け対策
- Trigger volume（Phase 18 以降）
- 空間分割（BVH / Octree）— entity 数が少い前提でナシ

---

## Design Decisions

### なぜ `Collider` enum にするか（Shape の種類を将来追加できるよう）

```rust
pub enum Collider {
    Aabb { half_extents: Vec3 },
    // 将来: Sphere { radius: f32 }, Capsule { radius: f32, height: f32 }
}
```

`match collider` で形状ごとの処理を書けば、Phase 21 で Sphere を追加する際に
既存コードを最小限の変更で拡張できる。

### なぜ Push-out を Kinematic と Static で分ける理由

- `Static` — 動かない壁・床。衝突しても動かない
- `Kinematic` — PlayerController のように自分で動く物体。Push-out を受けて位置が補正される

`Dynamic`（物理エンジンが動かす）は Phase 18 で追加する。  
Phase 17 では Static と Kinematic の 2 種類だけ。

### なぜ `CollisionEvent` を ECS Event として実装するか

コールバック関数（`on_collision`）方式より、ECS の event reader パターンの方が
system 間の依存が疎になる。  
`EventReader<CollisionEvent>` で衝突を読んだシステムが反応できる。  
複数のシステムが同じ collision event に独立して反応できる（スコア加算・SE 再生等）。

### push-out の優先軸選択

AABB overlap の最小軸で push-out する（Separating Axis Theorem の簡易版）:
```
dx = (half_a.x + half_b.x) - |center_a.x - center_b.x|
dy = (half_a.y + half_b.y) - |center_a.y - center_b.y|
dz = (half_a.z + half_b.z) - |center_a.z - center_b.z|
```
dx, dy, dz のうち最小の値の軸方向に push-out する。

---

## Implementation Plan

### 17-A: Collider コンポーネント

```rust
// crates/engine/src/collision.rs（新規）
#[derive(Clone, Debug)]
pub enum Collider {
    Aabb { half_extents: Vec3 },
}

impl Collider {
    pub fn aabb(half_extents: Vec3) -> Self { Self::Aabb { half_extents } }
    pub fn aabb_cube(half: f32) -> Self { Self::aabb(Vec3::splat(half)) }

    pub fn world_aabb(&self, transform: &GlobalTransform) -> WorldAabb {
        match self {
            Self::Aabb { half_extents } => WorldAabb {
                center: transform.translation,
                half_extents: *half_extents * transform.scale,  // スケール適用
            }
        }
    }
}

pub struct WorldAabb { pub center: Vec3, pub half_extents: Vec3 }

impl WorldAabb {
    pub fn overlaps(&self, other: &WorldAabb) -> Option<PushOut> {
        let dx = (self.half_extents.x + other.half_extents.x) - (self.center.x - other.center.x).abs();
        let dy = (self.half_extents.y + other.half_extents.y) - (self.center.y - other.center.y).abs();
        let dz = (self.half_extents.z + other.half_extents.z) - (self.center.z - other.center.z).abs();
        if dx > 0.0 && dy > 0.0 && dz > 0.0 {
            Some(PushOut::minimum_axis(dx, dy, dz, self.center - other.center))
        } else {
            None
        }
    }
}
```

### 17-B: PhysicsBody と CollisionEvent

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum PhysicsBody {
    Static,      // 動かない（壁・床）
    Kinematic,   // 自分で動く（push-out を受ける）
    // Dynamic は Phase 18 で追加
}

pub struct CollisionEvent {
    pub entity_a: Entity,
    pub entity_b: Entity,
    pub push_out: Vec3,  // entity_a を押し返すベクトル
}
```

### 17-C: 衝突検出システム（Fixed Update に登録）

```rust
pub fn collision_detection_system(
    mut query: Query<(Entity, &Collider, &PhysicsBody, Option<&mut Transform>), With<GlobalTransform>>,
    mut events: EventWriter<CollisionEvent>,
) {
    // O(n²) ブロードフェーズ兼ナローフェーズ
    let entities: Vec<_> = query.iter_mut().collect();
    for i in 0..entities.len() {
        for j in (i+1)..entities.len() {
            let (e_a, collider_a, body_a, transform_a) = &mut entities[i];
            let (e_b, collider_b, body_b, transform_b) = &entities[j];

            let world_a = collider_a.world_aabb(/* GlobalTransform from query */);
            let world_b = collider_b.world_aabb(/* GlobalTransform from query */);

            if let Some(push) = world_a.overlaps(&world_b) {
                events.send(CollisionEvent { entity_a: *e_a, entity_b: *e_b, push_out: push.vector });

                // Kinematic を Push-out
                if *body_a == PhysicsBody::Kinematic {
                    if let Some(t) = transform_a { t.translation += push.vector; }
                }
                if *body_b == PhysicsBody::Kinematic {
                    if let Some(t) = transform_b { t.translation -= push.vector; }
                }
            }
        }
    }
}
```

### 17-D: Debug Draw 統合

```rust
pub fn collider_debug_draw_system(
    query: Query<(&Collider, &GlobalTransform)>,
    mut debug_lines: ResMut<DebugLines>,
) {
    for (collider, global_transform) in &query {
        match collider {
            Collider::Aabb { half_extents } => {
                debug_lines.aabb(global_transform.translation, *half_extents * global_transform.scale, Vec3::new(0.0, 1.0, 0.0));
            }
        }
    }
}
```

`App::new()` でデバッグ描画システムを条件付きで登録（`#[cfg(debug_assertions)]`）。

---

## Cautions（注意点・落とし穴）

**O(n²) のスケーリング**:  
entity 数が 100 を超えると 5000 ペアのチェックになる。  
サンプルゲーム（Phase 20）の規模では問題ないが、entity が増えたら空間分割が必要になる。  
Phase 17 のスコープとして明記し、今は対応しない。

**Push-out の連鎖**:  
entity A が B を押し返し、B が C を押し返す連鎖処理は 1 フレームでは解決しない。  
イテレーションを複数回行う（Solver iteration）か、Phase 18 で物理エンジンに委ねる。  
Phase 17 では 1 フレーム 1 回の push-out で妥協する。

**スケール付き Collider の世界座標変換**:  
`Transform.scale` が 2.0 なら `half_extents` も 2 倍になる。  
`GlobalTransform` のスケールを掛け忘れると、大きい enemy が当たり判定の小さい collider を持つ状態になる。

**Fixed Update での Transform 変更**:  
`collision_detection_system` は Fixed Update で動く。  
`Transform` を変更した結果は `GlobalTransform` に伝播されるが、通常の update より 1 フレーム遅れる可能性がある。  
`transform_propagation_system` を Fixed Update にも登録することで解決する。

---

## Prohibited（禁止事項）

- Continuous Collision Detection（CCD）をこのフェーズで実装することを禁止
- 空間分割（BVH / Octree）をこのフェーズで実装することを禁止
- Trigger volume をこのフェーズで実装することを禁止（Dynamic は Phase 18 の責任）
- collision_detection_system を通常 update（可変フレーム）で実行することを禁止

---

## Completion Criteria（完了基準）

- `Collider::aabb` + `PhysicsBody::Static` の壁に PlayerController が衝突して止まる
- `DebugLines` で AABB collider の形が緑色で可視化される
- `EventReader<CollisionEvent>` でイベントを受信できる
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 18: `PhysicsBody::Dynamic` 追加、`Velocity` component との連携
- Phase 20: 敵と弾の衝突、スコア加算に `CollisionEvent` を使う
