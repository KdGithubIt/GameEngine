# Phase 57: Action Collision Toolkit

Status: 実装完了（2026-07-11）。4 ゲートすべてパス。

実装時の決定・制限（記録）:

- 線分 vs AABB / 線分 vs 線分の最近接点は交互射影（4 反復）で近似。
  閉形式解ではないため、ほぼ平行な浅い角度の構成では微小な残差があり得る
  （結果は sphere 判定にのみ入力されるためゲームプレイ深度では実害なし。
  rustdoc に明記）。
- **character controller 同士は現状すり抜ける**（controller の障害物
  スキャンは Static/Kinematic のうち controller 以外を対象とする。
  キャラ同士の押し合いは仕様未定義として Phase 62 で必要なら再訪）。
- clippy `type_complexity` 対策として `CollisionQueryData` 型エイリアスを
  collision.rs / character_controller.rs で共有。
- 新形状の debug draw は外接 AABB 表示（ワイヤーフレーム球は作らない）。

## Goal

アクションゲームに必要な衝突基盤を揃える: カプセル/スフィア形状・
collision layers・trigger volume・kinematic character controller・
スクリプトからの衝突観測。

## Why

- 現状は AABB のみ・レイヤーフィルタなし・trigger なし。キャラクター同士の
  押し合い・攻撃判定・エリア検知が作れず M1 が成立しない。
- Phase 34（ADR 0031）で project settings に layers（0〜31）が定義済みだが
  runtime に適用されていない。

## Scope

- In:
  - `Collider` に `Sphere { radius }` / `CapsuleY { half_height, radius }`
    を追加（capsule は Y 軸固定。傾いたカプセルは対象外）
  - 全形状ペアの overlap + push-out（aabb/sphere/capsuleY の 6 組）
  - `CollisionLayers { membership: u32, mask: u32 }` コンポーネント +
    detection system でのフィルタ
  - `TriggerVolume` マーカー + `CollisionEvent.is_trigger`
  - `KinematicCharacterController` + `character_controller_system`
  - authoring 統合: `engine.collider` / `engine.physics_body` /
    `engine.character_controller`（registry 13〜15 番目）
  - Rhai: `ctx.collisions()`（self entity の当該 step の衝突一覧）
  - テスト
- Out:
  - 任意軸カプセル・メッシュコライダー・swept/CCD（高速移動の
    トンネリングは既知の制限として文書化）
  - 外部物理エンジン統合（M1 非目標）
  - enter/exit 状態遷移イベント（現行の per-step overlap モデルを維持。
    必要になれば後続で）
  - editor での collider gizmo 表示改善（既存 debug draw を流用）

## Design Decisions

- **capsule は Y 軸固定**（`CapsuleY`）。キャラクター用途では標準で、
  oriented capsule の数学を排除できる。線分 vs 点/線分/AABB の最近接点で
  sphere 判定に帰着させる（capsule = 線分 + radius）。
- **layers はビットマスク 2 本**（membership + mask）。ペア (a, b) が
  衝突するのは `(a.membership & b.mask) != 0 && (b.membership & a.mask) != 0`
  のとき。コンポーネント未付与 = `membership: 1, mask: !0`（layer 0・
  全対象）。project settings の `Layer` 名は index → bit の対応
  （`1 << index`）で使う。
- **trigger は `TriggerVolume` マーカーコンポーネント**。どちらかが
  trigger のペアは push-out を適用せず、`CollisionEvent { is_trigger: true }`
  のみ発行する。
- **`CollisionEvent` に `is_trigger: bool` を追加**。engine 内でのみ
  構築される struct なので破壊は同一 PR のテスト更新で完結
  （破壊的変更プロトコル §3。呼び出し元を全部直す）。
- **character controller は move-then-resolve**:
  `KinematicCharacterController { velocity: Vec3, gravity_scale: f32,
  grounded: bool（システムが書く読み取り用）, max_resolve_iterations
  (default 3), skin?なし }`。`character_controller_system`（fixed・opt-in。
  physics 系と同じ扱いで `collision_detection_system` の**前**に登録する
  規約）: velocity * dt + gravity を Transform に適用 → 自分の collider と
  static/kinematic collider 群に対して push-out を最大 N 回反復 → 上向き
  push-out を受けたら `grounded = true`（下向き速度をゼロ化）。
  PlayerController（13-A）は変更しない（併存。busters_lite は
  character controller 側を使う）。
- **Rhai は per-entity スナップショット**: dispatch 側が
  `CollisionEvents` から self entity の分を抽出して
  （input/save スナップショットと同型の Arc 渡し）`ctx.collisions()` が
  rhai 配列（各要素は map: `other`（entity 文字列）・`push_x/y/z`・
  `is_trigger`）を返す。イベント名文字列への埋め込みはしない。
- **BT 配線は既存パターン**: `CollisionEvents` はリソースなので
  Phase 54 の UiEventFrame と同じ「tick 前に読んで condition に翻訳」
  パターンで到達可能。新機構なし（テストで実演のみ）。
- **authoring スキーマ**（フラットフィールド規約・Phase 52 と同様）:
  - `engine.collider`: `shape`（string enum: `"aabb"|"sphere"|"capsule_y"`）・
    `half_extent_x/y/z`（aabb 用）・`radius`・`half_height`・
    `is_trigger`（bool → TriggerVolume 付与）・`membership`・`mask`
    （I64。u32 範囲検証）。使わないフィールドは無視（例: sphere で
    half_extent_*）。spawn は `Collider` + `CollisionLayers`
    （+ trigger 時 `TriggerVolume`）を付与
  - `engine.physics_body`: `kind`（string enum: `"static"|"kinematic"|"dynamic"`）
  - `engine.character_controller`: `gravity_scale`（default 1.0）・
    `max_resolve_iterations`（default 3）

## Implementation Plan

- 57-A: 形状追加 + ペア overlap 関数（純粋関数・ユニットテスト）
- 57-B: layers / trigger / event 拡張 + detection system 更新
- 57-C: character controller + system
- 57-D: authoring 3 コンポーネント（schema/spawn/registry/テスト。
  registry 件数 assert の更新を含む）
- 57-E: Rhai `ctx.collisions()` + テスト
- 57-F: 4 ゲート

## Tests

- 形状ペア: 各組み合わせの「重なる/重ならない/push-out の向きと量」
  （sphere-sphere・sphere-aabb・capsule-aabb・capsule-capsule・
  capsule-sphere は境界ケース込み。既存 aabb-aabb テストは無変更で通す）
- layers: mask 不一致ペアがイベントも push-out も生じない・片方向 mask
  （a は b を見るが b は a を見ない）は衝突しない（両方向必要）
- trigger: 重なってもTransform 不変・`is_trigger: true` のイベントが出る
- controller: 床（static aabb）に落下 → 接地して `grounded = true`・
  壁に向かって移動 → 壁手前で停止（押し出し）・trigger は接地/停止に
  寄与しない
- authoring: 3 コンポーネントの spawn / デフォルト / 不正値診断
  （shape 不明文字列・負 radius・u32 範囲外 membership）・registry 15 件
- Rhai: 衝突中のエンティティのスクリプトで `ctx.collisions()` が
  other/push/is_trigger を返す・衝突なしで空配列

## Cautions

- `.rs` に日本語禁止・英語 rustdoc・回復可能エラーに unwrap 禁止
- `Collider` は公開 enum。variant 追加で壊れる match（debug draw・editor
  等）を同一 PR で全部直す（破壊的変更プロトコル §3）
- `world_aabb()` は全形状で「外接 AABB」を返すよう拡張する（debug draw と
  broad phase 互換のため）。狭域判定は新しいペア関数側で行う
- 反復 push-out の無限振動対策として iteration 上限を必ず入れる
- capsule 数学は「線分上の最近接点 + sphere 判定」に帰着させ、
  独自の解析解を書かない（バグ源）

## Prohibited

- 外部物理クレート（rapier 等）の導入
- `collision_detection_system` の登録方式変更（opt-in fixed のまま）
- PlayerController（13-A）の挙動変更

## Completion Criteria

- カプセルキャラクターが AABB の床/壁で構成されたアリーナ内を移動でき、
  壁で止まり床に接地する（テストで保証）
- 攻撃判定用途の trigger sphere が重なりイベントを出し、スクリプトから
  `ctx.collisions()` で観測できる（テストで保証）
- layers で「敵の攻撃は敵に当たらない」型のフィルタが機能する
- 4 ゲートパス

## Feeds Into

- Phase 58: Targeting & Action Camera（ロックオン対象の空間クエリ）
- Phase 62: busters_lite の戦闘（攻撃 hitbox・キャラ押し合い・エリア検知）
