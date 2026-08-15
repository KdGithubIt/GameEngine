# Phase 22: Physics — Velocity / Gravity / Rigidbody

> **2026-06-13 再構成**: 旧 Phase 18。新 Phase 21（Collision）の後に実施する。
> 旧→新対応表は `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` を参照。

## Goal

重力・速度・加速度を持つ `Dynamic` rigidbody を実装し、  
オブジェクトが物理法則に従って落ちる・跳ねる・転がるような挙動を作れるようにする。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**Phase 17（collision）の後にこのフェーズが来る理由**:  
physics は collision の結果（どこで止まるか）に依存する。  
collision なしで physics を実装すると、物体が床を貫通しながら加速し続ける。  
必ず collision → physics の順で実装する。

**Phase 12（Fixed Update）の後にこのフェーズが来る理由**:  
physics の計算はフレームレートに依存してはいけない（Fixed Timestep が前提）。  
Phase 12 で `add_fixed_system` が完成しているため、この上に physics を乗せる。

**なぜ本格的な物理エンジン（rapier 等）を使わないか**:  
外部の物理エンジンは API が独自で、ECS との統合に相当な接着コードが必要。  
学習目的のエンジンでは自作の方がアーキテクチャを理解できる。  
サンプルゲーム（Phase 20）程度の規模では自作 physics で十分。  
rapier 等は Phase 21 以降で「高度な物理が必要になったら検討」とする。

---

## Scope

### 作るもの

- `Velocity` コンポーネント（`Vec3` の線形速度）
- `Gravity` resource（`Vec3` — デフォルト `[0, -9.81, 0]`）
- `PhysicsBody::Dynamic` — Velocity + Gravity に従って動く
- `gravity_system` — Dynamic body に重力加速度を加算
- `velocity_system` — Velocity に従って Transform を移動
- `restitution`（反発係数）— 床との衝突で跳ね返る
- `PhysicsBody::Dynamic` と Phase 17 の collision の統合

### 作らないもの

- 回転（angular velocity / torque）— 平進のみ
- 摩擦（friction）
- Constraint / Joint（ヒンジ・スプリング等）
- 多物体の連鎖衝突の solver（Phase 17 の push-out を再利用）
- 水・風などの環境力

---

## Design Decisions

### なぜ angular velocity を作らないか

回転の物理（慣性テンソル・角運動量の保存）は平進より大幅に複雑。  
ゲームプレイに回転物理が必要なケースは少なく、アニメーションで代用できることが多い。  
スコープを絞り、サンプルゲームに必要な最小限の物理に留める。

### なぜ `Gravity` を resource にするか（component ではなく）

重力はゲーム全体のグローバル設定。entity ごとに別の重力を持つケースは稀。  
resource にすることで「宇宙ゲーム（重力なし）」では `Gravity(Vec3::ZERO)` を設定するだけ。  
entity に `gravity_scale: f32` を持たせることで個別調整もできる（デフォルト 1.0）。

### physics の実行順序（Fixed Update 内）

```
1. gravity_system          — Velocity に重力を加算
2. velocity_system         — Velocity * fixed_delta で Transform を移動
3. collision_detection_system — 重なりを検出
4. push_out_resolution     — Dynamic body を push-out
5. velocity_restitution    — 衝突面の法線方向の速度に反発係数を掛ける
6. transform_propagation   — GlobalTransform を更新
```

この順序を変えると「速度で動いた後に壁を突き抜ける」「重力が 1 フレーム遅れる」等の問題が起きる。

### Velocity と Kinematic の共存

- `Kinematic` はシステム（PlayerController）が Transform を直接動かす。Velocity を持たない。
- `Dynamic` は physics システムが Velocity 経由で Transform を動かす。直接 Transform を書き換えない。
- `Static` は動かない。

この分類を崩すと「PlayerController が Velocity を上書きする」「physics が Input の移動を消す」等のバグが起きる。

---

## Implementation Plan

### 18-A: Velocity コンポーネントと GravityScale

```rust
// crates/engine/src/physics.rs（新規）
pub struct Velocity {
    pub linear: Vec3,
}

impl Default for Velocity {
    fn default() -> Self { Self { linear: Vec3::ZERO } }
}

pub struct GravityScale(pub f32);  // デフォルト 1.0

pub struct Gravity(pub Vec3);  // デフォルト Vec3::new(0.0, -9.81, 0.0)

impl Default for Gravity {
    fn default() -> Self { Self(Vec3::new(0.0, -9.81, 0.0)) }
}
```

`App::new()` で自動挿入:
```rust
ecs.insert_resource(Gravity::default());
```

### 18-B: PhysicsBody::Dynamic 追加

Phase 17 の `PhysicsBody` enum に `Dynamic` を追加:
```rust
pub enum PhysicsBody {
    Static,
    Kinematic,
    Dynamic,   // Velocity + Gravity に従って動く ← 追加
}
```

### 18-C: physics systems

```rust
// gravity_system — Fixed Update
pub fn gravity_system(
    gravity: Res<Gravity>,
    fixed_time: Res<FixedTime>,
    mut query: Query<(&PhysicsBody, &mut Velocity, Option<&GravityScale>)>,
) {
    for (body, velocity, gravity_scale) in &mut query {
        if *body == PhysicsBody::Dynamic {
            let scale = gravity_scale.map(|s| s.0).unwrap_or(1.0);
            velocity.linear += gravity.0 * scale * fixed_time.fixed_delta;
        }
    }
}

// velocity_system — Fixed Update
pub fn velocity_system(
    fixed_time: Res<FixedTime>,
    mut query: Query<(&PhysicsBody, &Velocity, &mut Transform)>,
) {
    for (body, velocity, transform) in &mut query {
        if *body == PhysicsBody::Dynamic {
            transform.translation += velocity.linear * fixed_time.fixed_delta;
        }
    }
}
```

### 18-D: Restitution（反発係数）

```rust
// Phase 17 の CollisionEvent を受け取って速度を反発させる
pub fn restitution_system(
    mut events: EventReader<CollisionEvent>,
    mut query: Query<(&PhysicsBody, &mut Velocity)>,
) {
    for event in events.read() {
        // entity_a が Dynamic の場合、衝突法線方向の速度を反転して restitution を掛ける
        if let Ok((PhysicsBody::Dynamic, mut velocity)) = query.get_mut(event.entity_a) {
            let normal = event.push_out.normalize();
            let dot = velocity.linear.dot(normal);
            if dot < 0.0 {
                // 衝突面に向かっている場合のみ反発
                let restitution = 0.3;  // TODO: Collider component から取得
                velocity.linear -= (1.0 + restitution) * dot * normal;
            }
        }
    }
}
```

### 18-E: Fixed Update への登録順序

```rust
app
    .add_fixed_system(gravity_system)
    .add_fixed_system(velocity_system)
    .add_fixed_system(collision_detection_system)  // Phase 17
    .add_fixed_system(restitution_system)
    .add_fixed_system(transform_propagation_system);
```

---

## Cautions（注意点・落とし穴）

**velocity_system より先に gravity_system を実行する**:  
重力を加算してから移動させる。逆順だと重力が 1 step 遅れて適用される。

**Terminal velocity（終端速度）がない場合の問題**:  
重力がかかり続けると `Velocity.linear.y` が負の方向に無限に大きくなる。  
Terminal velocity（例: `-50 m/s`）をクランプしないと、長時間落ちた後に壁を貫通するリスクがある。  
Phase 18 で基本的なクランプを入れておく。

**Push-out 後の velocity の向きが不正**:  
`push_out_resolution` で Transform を補正した後も、Velocity は衝突前の向きを向いている可能性がある。  
`restitution_system` で反発処理しないと、次のフレームも同じ方向に動いて再び衝突する。

**Kinematic との衝突**:  
`Dynamic` と `Kinematic` が衝突した場合、`Kinematic` は push-out を受けるが Velocity はない。  
`Dynamic` の Velocity は反発させる。`Kinematic` 側は Phase 17 の push-out のみ。

---

## Prohibited（禁止事項）

- Angular velocity（回転物理）をこのフェーズで実装することを禁止
- Friction をこのフェーズで実装することを禁止
- rapier 等の外部物理エンジンをこのフェーズで導入することを禁止
- `velocity_system` を通常 update（可変フレーム）で実行することを禁止
- Fixed Update の実行順序（gravity → velocity → collision → restitution）を変えることを禁止

---

## Completion Criteria（完了基準）

- `PhysicsBody::Dynamic` entity が重力で落下する
- 落下した entity が `PhysicsBody::Static` の床で止まる
- `GravityScale(0.0)` で重力が無効になる
- Restitution により床で軽く跳ね返る
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 20: 弾が重力で放物線を描く / 敵が地面に乗る挙動を physics で実現
