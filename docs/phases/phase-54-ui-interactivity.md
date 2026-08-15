# Phase 54: UI Interactivity & Authoring Integration

Status: 実装完了（2026-07-11）。4 ゲートすべてパス。

実装時の決定・逸脱（記録）:

- `ui_script_event_system` の自動登録と「ScriptEngine 未使用ゲームでも
  `ecs.update()` が失敗しない」を両立するため、`engine_ecs` に
  `SystemParam for Option<ResMut<T>>` を追加した（既存 `Option<Res<T>>` と
  対称の加算的実装。write アクセス登録・SAFETY コメント・テスト付き）。
- BT の `check_condition` は World リソースへ直接アクセスできないため、
  「`UiEventFrame` を読んで `BehaviorTreeBehaviorRegistry::set_condition`
  に翻訳してから tick する」のがサポートパターン（統合テストの doc
  comment に明記）。BT 実行器自体への World 注入は将来課題。
- built-in UI アセット ID は `asset_01JP0000000000000000000501`。

## Goal

Phase 53 の UI ドキュメントを (1) シーンから `engine.ui_document`
コンポーネントとして配置可能にし、(2) ボタンイベントを Rhai / BT / ゲーム
システムへ配線し、(3) `.ui.json` のホットリロードを可能にする。

## Why

- Phase 53 では `UiDocumentOverlay` を Rust で `add_ui_system` 登録する
  必要があり、エディタ・パッケージからまだ UI を置けない。
- `UiEvents` は積まれるだけで消費者がいない。クリック → ゲームロジックの
  経路が存在しない。

## Scope

- In:
  - `UiEventFrame` リソース + relay システム（マルチ消費者対応）
  - Rhai `on_event` へのディスパッチ（フレームスケジュール）
  - `engine.ui_document` コンポーネント（AssetRef・registry 12 番目）+
    built-in UI ドキュメントアセット + editor picker / Register Asset 対応
  - シーン配置 UI を描画する組み込みホスト（`add_ui_system` 不要化）
  - mtime ポーリングによるホットリロード（デスクトップのみ）
- Out:
  - image ノード・ドキュメント include（後続 Phase）
  - エディタでの UI WYSIWYG 編集（Phase 55 以降の任意課題）
  - BT 用の組み込み condition 実装（リソース読み取りパターンの提供と
    テストまで。ゲーム側が `BehaviorTreeBehaviorRegistry` に登録する）

## Design Decisions

### 1. イベント配線: `UiEventFrame` リレー方式

`UiEvents`（Mutex キュー）を直接 drain すると消費者が 1 つに限られる。
そこで毎フレーム先頭で relay システムが `UiEvents::take()` し、結果を
`UiEventFrame`（プレーンな `Vec<String>` + `contains(name)` ヘルパー。
毎フレーム置換）に載せ替える。Rhai ディスパッチ・BT condition・ゲーム
システムはすべて `Res<UiEventFrame>` を読むだけ（複数消費者・非破壊）。

- UI 描画はフレーム末尾（`run_ui_systems`）なので、フレーム N のクリックは
  フレーム N+1 の `UiEventFrame` に現れる（1 フレーム遅延。メニュー用途で
  許容。rustdoc に明記）。
- relay は `App::new` がフレームスケジュール先頭側に自動登録する
  （`particle_update_system` などと同じ扱い）。
- `UiEvents::push` は上限（256）でドロップ + 警告ログ（消費者不在時の
  無限成長防止）。

### 2. Rhai ディスパッチはフレームスケジュールの専用システム

`ui_script_event_system`（新規・自動登録）が `UiEventFrame` の各イベントを
enabled な全 `ScriptComponent` エンティティの `on_event(ctx, event)` に
ブロードキャストする（既存 `ScriptEngine::run_on_event` を使用）。
`scripting_update_system`（fixed）に載せないのは、fixed tick がフレームと
1:N になるとイベントが重複配信されるため。スクリプト側はイベント名で
フィルタする規約（ADR 0037 の on_event 契約どおり）。

### 3. `engine.ui_document` は whole-value AssetRef コンポーネント

`engine.mesh` と同型（`InspectorHint::AssetRef`）。`AssetKind` に
`UiDocument` variant を追加（additive）。

- spawn: manifest から path 解決 → `load_ui_document` → 失敗時は
  **non-blocking 診断 + 空ドキュメントへフォールバック**（mesh の
  fallback と同じ方針）。成功時はエンティティに `UiDocumentRef`
  コンポーネント（asset id・document・source_path・last mtime）を付与。
- built-in アセット `BUILTIN_UI_DOCUMENT_ASSET_ID`（`asset_01JP...` 形式の
  新定数）: manifest 不要で解決される最小ドキュメント（"New UI" テキスト
  1 個）。schema の component_default に使う（built-in triangle と同じ
  パターン）。
- editor: `asset_choices_from_manifest` に UiDocument 分岐（built-in +
  manifest の `.ui.json` エントリ）、Register Asset の対象拡張子に
  `.ui.json` を追加。

### 4. シーン UI の描画は組み込みホスト

`run_ui_systems` が、登録済み `UiSystem` 群に加えて **world 内の全
`UiDocumentRef` を描画する**（Phase 53 の描画ロジックを
`draw_ui_document(...)` 自由関数に抽出して `UiDocumentOverlay` と共有）。
standalone runner の egui ゲート（`ui_systems.is_empty()` で egui パスを
スキップする最適化）は「`UiDocumentRef` が 1 つでも存在するか」も見るよう
拡張する（`App` に world を問い合わせる述語を追加）。

### 5. ホットリロードは mtime ポーリング

`ui_document_reload_system`（自動登録・`cfg(not(target_arch = "wasm32"))`）
が 0.5 秒間隔（`Time` 累積）で各 `UiDocumentRef` の source_path の mtime を
確認し、変化していたら再ロード。失敗（パース中・不正 JSON）は旧
ドキュメントを保持して警告ログのみ（panic・コンポーネント除去禁止）。

## Implementation Plan

- 54-A: `UiEventFrame` + relay + `ui_script_event_system` + `UiEvents` 上限
- 54-B: `UiDocumentRef` + `draw_ui_document` 抽出 + `run_ui_systems` 拡張 +
  egui ゲート拡張
- 54-C: `engine.ui_document` schema/spawn/registry + built-in アセット +
  editor picker / Register Asset
- 54-D: ホットリロード
- 54-E: テスト + 4 ゲート

## Tests

- relay: push → 次フレーム `UiEventFrame::contains` が true・その次で消える
- 上限: 257 個 push でドロップされ長さ 256
- Rhai: `on_event` を持つスクリプトが UI イベントを受け取る（既存
  scripting テストの温度感で。イベント名引数の一致を assert）
- BT: `UiEventFrame` を読む condition を `BehaviorTreeBehaviorRegistry` に
  登録し、イベント有無で Success/Failure が変わる統合テスト
- spawn: temp project + manifest 登録済み `.ui.json` → `UiDocumentRef` 付与。
  欠損アセット → 空ドキュメント + non-blocking 診断。built-in id →
  manifest なしで解決
- registry: 12 個目の定義・default_value が built-in AssetRef（既存の
  件数 assert テスト更新を含む）
- reload: temp ファイル書き換え → mtime 更新 → ドキュメント差し替わり。
  不正 JSON 書き込み → 旧ドキュメント保持
- headless egui: `UiDocumentRef` を持つ world で `run_ui_systems` 相当の
  描画が shapes を出す

## Cautions

- `.rs` に日本語禁止・英語 rustdoc 必須・回復可能エラーに unwrap 禁止
- `AssetKind` は公開 enum。variant 追加で editor 側の exhaustive match が
  コンパイルエラーになる場合は、同一 PR で全呼び出し元を更新する
  （破壊的変更プロトコル §3）
- mtime は同一秒解像度の FS があるため、テストでは mtime を明示的に
  過去へ設定するか内容比較フォールバックを検討（まず mtime 明示設定で）
- `run_ui_systems` は `&mut World` を取れない（`&World`）。リロードは
  通常のフレームシステム側（`&mut` 可）で行うこと

## Prohibited

- コールバックのシリアライズ・ECS パスバインド（ADR 0046 却下事項）
- `UiEvents` を直接 drain する消費者の追加（`UiEventFrame` を読む）
- 既存 `*.ui.json` / manifest フォーマットの非互換変更

## Completion Criteria

- エディタで entity に `engine.ui_document` を追加 → Play で HUD が出る
  （spawn 経路は自動テスト・目視は任意）
- ボタンイベントが Rhai `on_event` と BT condition に届く（テストで保証）
- `.ui.json` を書き換えると Play 中の表示が追従する（テストで保証）
- 4 ゲートパス

## Feeds Into

- Phase 60: Script API v2（スクリプトから `UiBindings` を書く）
- Phase 62: busters_lite の HUD / メニュー
