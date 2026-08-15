# Phase 58: Targeting & Action Camera

Status: 実装完了（2026-07-11）。4 ゲートすべてパス。

実装時の決定・記録:

- 線分遮蔽ヘルパー（`segment_blocked_by_static` / `static_obstacle_aabbs`）
  は collision.rs 側に配置（`WorldAabb` の private min/max を使うため）。
- `engine.lock_on_camera` の source 未解決診断
  （`scene_bridge.lock_on_camera_unresolved_source`・non-blocking）は
  follow_camera と同型だが、公開パイプラインでは
  `AuthoringScene::validate()` の `scene.bad_entity_ref`（blocking）が
  先に弾くため実質防御コード（follow_camera の既存挙動と同じ。
  テストは SpawnContext 直構築で通している）。
- 定数: 壁 margin 0.2・カメラ最小距離 0.5（camera.rs）。

## Goal

ロックオン（対象選択・維持・解除）と、対象を画面に収めるロックオン
カメラ + 壁衝突回避（spring arm）を提供する。

## Why

- M1 のアクション戦闘はロックオン前提（バスターズの基本操作）。
- 既存カメラ（Orbit/Follow）は対象追尾のみで、戦闘用のフレーミングと
  壁へのめり込み回避がない。

## Scope

- In:
  - `LockOnTarget` コンポーネント（`team: u32`）= ロックオン可能マーカー
  - `TargetLock` リソース（現在の対象 + Acquire/Cycle/Release 要求 API）
  - `lock_on_system`（要求処理 + 毎フレームの対象妥当性検証）
  - `LockOnCamera` コンポーネント + `lock_on_camera_system`
    （source と対象を収めるフレーミング + 壁衝突回避）
  - 遮蔽/壁判定用の線分 vs Static コライダー群ヘルパー（保守的:
    外接 AABB に対する slab 法）
  - authoring: `engine.lock_on_target` / `engine.lock_on_camera`
    （registry 16〜17 番目）
  - テスト
- Out:
  - Rhai からのロックオン操作（Phase 60。`TargetLock` は既にリソース
    なので BT は Phase 54 パターンで読める）
  - FollowCamera / OrbitCamera への壁回避の後付け（将来課題。
    既存 struct を変えない）
  - 精密な形状別レイキャスト（外接 AABB で保守的に判定。文書化）
  - 入力バインド（どのキーで Acquire するかはゲーム側）

## Design Decisions

- **`TargetLock` はリソース**（シングルプレイヤー前提の M1 スコープ。
  複数ロックオン主体は将来課題として rustdoc に明記）:
  - `current() -> Option<Entity>`
  - `request_acquire()` / `request_cycle()` / `request_release()`
    （最後の要求のみ保持）
  - system が処理後に要求をクリア
- **選択規則**: Acquire = 有効対象のうち source から最近傍。Cycle =
  距離昇順リストで現在の次（末尾なら先頭）。有効対象 =
  `LockOnTarget` を持ち・source 以外・距離 <= `max_target_distance`・
  `team_filter` 一致（-1 = 全チーム）・（`require_line_of_sight` 時）
  source から遮蔽なし。
- **毎フレーム検証**: 現在の対象が despawn / 距離超過 / LOS 喪失で
  自動解除（`current = None`）。ヒステリシスなし（v1 簡素化）。
- **遮蔽と壁回避は同じヘルパー**を使う:
  `segment_blocked_by_static(colliders, from, to) -> Option<f32>`
  （最初のヒットの t 値）。対象は `PhysicsBody::Static` かつ非 Trigger の
  コライダーの**外接 AABB**（`Collider::world_aabb`）に対する slab 法。
  カプセル/球も外接 AABB で保守的に扱う（rustdoc に明記）。
- **`LockOnCamera`**: `source: Entity`（プレイヤー等）・`distance`・
  `height`・`spring_strength`（FollowCamera と同じ指数減衰規約）・
  `max_target_distance`・`require_line_of_sight: bool`・
  `team_filter: i64`（-1 = any）。
  - ロックオン中: source の背後（source→target の逆方向）`distance`・
    高さ `height` に位置し、source と target の中点を注視。
  - 非ロックオン中: FollowCamera 相当（source 背後の固定オフセット追尾。
    直前の視線方向を維持）。
  - **壁回避**: 注視点（source の頭上 = source 位置 + height の半分）から
    理想カメラ位置への線分が Static に遮られたら、ヒット t の手前
    （margin 0.2）にカメラを引き寄せる。
- **システム登録**: `lock_on_system` → `lock_on_camera_system` の順で
  frame スケジュール。既存カメラ系（orbit/follow）が登録されている場所
  （player.rs と editor Play の両方）に同じく追加する。
- **authoring**（フラットフィールド規約）:
  - `engine.lock_on_target`: `team`（I64・default 0・0..=u32::MAX 検証）
  - `engine.lock_on_camera`: `distance`（default 6.0・正）・`height`
    （default 2.5）・`spring_strength`（default 0.85・0..=1）・
    `max_target_distance`（default 20.0・正）・`require_line_of_sight`
    （Bool・default true）・`team_filter`（I64・default -1・
    -1..=u32::MAX 検証）。source は follow_camera の spawn がやっている
    方式（player marker 解決等）を踏襲する — spawn_follow_camera_component
    を読んで同じ解決規則にすること

## Implementation Plan

- 58-A: `lock_on.rs`（`LockOnTarget` / `TargetLock` / `lock_on_system` +
  線分遮蔽ヘルパー。ヘルパーは collision.rs 側でも可 — 既存構造に合わせる）
- 58-B: `LockOnCamera` + `lock_on_camera_system`（camera.rs）
- 58-C: authoring 2 コンポーネント + registry（16〜17 番目・件数テスト更新）
- 58-D: システム登録（player.rs / editor Play）+ テスト + 4 ゲート

## Tests

- 選択: 複数対象から最近傍 Acquire・Cycle の順送り（末尾→先頭の wrap）・
  team_filter 除外・max_target_distance 外の除外
- LOS: source と対象の間に Static AABB を置くと Acquire 不成立
  （require_line_of_sight = false なら成立）
- 検証: 対象 despawn / 距離超過 / 遮蔽発生で自動解除
- 要求 API: 同一フレーム複数要求は最後勝ち・処理後クリア
- カメラ: ロックオン中に source-target 逆方向 `distance` に配置される・
  非ロックオン中は追尾・壁があるとカメラが手前に引き寄せられる
  （t とmargin の位置検証）
- 遮蔽ヘルパー: 線分 vs AABB の hit/miss/t 値（軸平行・斜め・内部開始）
- authoring: 2 コンポーネントの spawn/デフォルト/不正値診断・registry 17 件

## Cautions

- `.rs` に日本語禁止・英語 rustdoc・回復可能エラーに unwrap 禁止
- `FollowCamera` / `OrbitCamera` の既存 struct・挙動を変えない
- `Vec3::normalize` のゼロベクトル（source と target が同位置）で
  panic しない（`normalize_or_zero` + フォールバック方向）
- カメラの引き寄せで distance が 0 に張り付かないよう下限（0.5）を設ける

## Prohibited

- 複数同時ロックオン・マルチソース対応の先行実装
- 精密レイキャストの独自実装（外接 AABB slab 法のみ）

## Completion Criteria

- 対象取得 → カメラが対象をフレーミング → 障害物でカメラが押し込まれる →
  対象消失で自動解除、の一連がテストで保証される
- エディタから `engine.lock_on_target` / `engine.lock_on_camera` を配置
  できる（spawn 経路テスト）
- 4 ゲートパス

## Feeds Into

- Phase 60: Script API v2（`ctx.lock_target()` 等）
- Phase 62: busters_lite の戦闘カメラ
