# Phase 55: Scene Management & Game Flow

Status: 実装完了（2026-07-11）。4 ゲートすべてパス。

実装時の決定・逸脱（記録）:

- mid-spawn 失敗のクリーンアップは新機構を作らず、
  `spawn_from_authoring_scene` 既存のロールバック（エンティティ +
  アセットストア変更を Err 前に巻き戻す）に依存する（rustdoc に明記）。
- `AuthoringToRuntimeMap::spawned_entities()` を additive に追加。
- phase doc が想定した `start_with_project` は実在せず（実名は
  `RuntimePlayState::start`）。シグネチャは変えず `start_with_scene_path` /
  `start_from_document` を追加し、editor の Play ボタン
  （`ui/mod.rs::start_play`）も後者に配線した。
- `cargo check -p engine --target wasm32-unknown-unknown` は master 時点で
  既に失敗（transitive `arboard`）。本 Phase とは無関係の既存リグレッション
  として別タスク化済み。scene_manager 自体はデスクトップ専用 API を
  使用していない。

## Goal

実行時のシーン切替（タイトル → ミッション → リザルト等のゲームフロー）を
可能にする。Phase 18-C の決着。

## Why

- ゲームは 1 シーンしかロードできず、M1 のミッションループが成立しない。
- UI イベント（Phase 54）やスクリプト/BT からの遷移先が必要。

## Scope

- In:
  - engine `scene_manager.rs`: `SceneManager` / `SceneSwitchState` リソース +
    切替実行ロジック
  - `App::process_scene_requests()` と両ホスト（EngineRunner / editor
    RuntimePlayState）の呼び出し配線
  - player 起動時の `SceneLoader` / `SceneManager` リソース挿入（editor
    Play 側も同様）
  - テスト
- Out:
  - 非同期ロード・ロード画面（将来 ADR。`SceneSwitchState` に将来
    `Loading` を足せる形にしておく）
  - additive scene loading（ADR 0047 で却下済み）
  - Rhai からの `request_scene`（Phase 60）
  - アセットストアの eviction（ADR 0047 §3）

## Design Decisions

ADR 0047 に従う。実装詳細:

- `SceneManager`:
  - `request_switch(path: impl Into<String>)` — 最後の要求のみ保持
    （同一フレーム複数要求は最後勝ち。上書き時は `log::warn!`）
  - `generation() -> u64`・`current_scene_path() -> Option<&str>`
  - 内部: `current_entities: Vec<Entity>`（bridge の entity map 由来）
- `SceneSwitchState`: `Idle` | `Failed { path: String, message: String }`
- `App::process_scene_requests()`:
  1. `SceneManager` の pending 要求を取り出す（なければ即 return）
  2. `SceneLoader` リソースで load（ファイル読み + parse + validate）。
     失敗 → `Failed` 記録 + `log::error!`、現行シーン無傷で return
  3. 旧シーンエンティティを despawn（despawn 失敗は個別 warn で続行）
  4. `spawn_from_authoring_scene` で新シーンを spawn。成功 → entity list
     と generation 更新・`Idle`。blocking エラー → 生成済みエンティティを
     despawn して `Failed`（ADR 0047 §4）
- ホスト配線:
  - EngineRunner のフレーム処理（`ecs update` の直前）に
    `process_scene_requests` を追加
  - editor `RuntimePlayState::update`（runtime.rs の
    `run_fixed_update`/`update` を呼んでいる箇所の直前）にも追加
  - player: 起動時に `SceneLoader`（ProjectRoot から構築済みのもの）と
    `SceneManager` を world に挿入。初回ロードは既存コードのままでよいが、
    初回 spawn の entity list を `SceneManager` に登録して「最初の切替で
    初期シーンが正しく消える」ことを保証する
  - editor Play（`start_with_project`）: ProjectRoot が既知なので同様に
    挿入 + 初回 entity list 登録
- `spawn_from_authoring_scene` の戻り値（bridge）から entity list を
  取得する。公開 API に entity 一覧アクセサがなければ additive に追加する
  （既存フィールドの変更は不可）

## Implementation Plan

- 55-A: `scene_manager.rs`（リソース + 切替ロジック + `App` メソッド）
- 55-B: ホスト配線（EngineRunner / editor runtime / player）
- 55-C: テスト + 4 ゲート

## Tests

- 切替成功: temp project に scene A / B → A spawn 登録 → B を request →
  process → A のエンティティ消滅・B のエンティティ存在・generation +1・
  `Idle`
- 切替失敗（存在しないパス）: 現行シーンのエンティティが無傷・
  `Failed { path, .. }`・generation 不変
- 切替失敗（不正 JSON）: 同上
- 同一フレーム二重 request: 最後の要求だけが実行される
- ゲーム生成エンティティ（bridge 外で spawn したもの）が切替後も生存
- 要求なしで process を呼んでも no-op（毎フレーム呼ばれる前提の軽さ）
- editor RuntimePlayState 経由の統合テスト（既存 runtime.rs テストの
  temp project パターンで、Play 中に request → 次 update で切替）

## Cautions

- `.rs` に日本語禁止・英語 rustdoc・回復可能エラーに unwrap 禁止
- `process_scene_requests` はスケジュール実行中に呼ばない（`&mut World`
  排他前提）。システム内から呼べない設計であることを rustdoc に明記
- 旧シーン despawn 前に新シーンの load/validate を済ませる順序を崩さない
  （ADR 0047 §4。テストで固定する）
- wasm32 ビルドを壊さない（`SceneLoader` の wasm スタブ（scene_loader.rs
  89 行目付近）と整合させる）

## Prohibited

- additive loading・非同期ロードの先行実装
- 切替時の world リソース削除（リソースは永続。ADR 0047 §3）
- `spawn_from_authoring_scene` の既存シグネチャ変更

## Completion Criteria

- UI イベント → ゲームシステム → `request_switch` → 次フレームで別シーンに
  切り替わる経路がテストで保証される
- 失敗時に現行シーンが無傷で `SceneSwitchState::Failed` が観測できる
- editor Play と player の両方で同じ切替実装が動く（editor 経由は統合
  テスト、player 経由はリソース挿入のユニットテストで担保）
- 4 ゲートパス

## Feeds Into

- Phase 56: Save/Load（シーン横断の永続状態と組み合わせてゲームフロー完成）
- Phase 60: Script API v2（`ctx.request_scene(path)`）
- Phase 62: busters_lite のミッションループ
