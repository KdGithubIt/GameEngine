# Phase 53: Declarative UI Foundation

Status: 実装完了（2026-07-11）。4 ゲートすべてパス。
authoring `ui` モジュール（15 テスト）+ engine `ui_document.rs`（18 テスト）+
`App::add_ui_font`/`install_ui_fonts`（standalone runner と editor Play の
両ホスト配線済み）。

実装時の仕様逸脱（記録）: ヘッドレス egui のクリック合成は egui 内部の
レイアウト/スペーシングに対して brittle だったため、「描画出力が非空で
あること」の検証 + `UiEvents` push/take・診断機構の単体テストに分割した
（テストの rustdoc に理由を記載）。クリック→イベントの end-to-end 検証は
Phase 54 のイベント配線テスト or Phase 62 vertical slice で回収する。

## Goal

UI をシリアライズ可能なドキュメント（`*.ui.json`）として宣言し、engine が
毎フレーム egui に解釈描画する基盤を作る。M1（バスターズ級アクション RPG）
の HUD / メニューの土台。

## Why

- ランタイム UI が Rust コード専用（Phase 24）で、エディタ・スクリプト・
  AI エージェントから UI を作れない。
- ADR 0046 で「VDOM は作らず、データ + egui インタープリタ」と決定済み。

## Scope

- In:
  - authoring `ui` モジュール: `UiDocument` / `UiNode`（panel / text /
    button / spacer）/ `UiAnchor` / バインディング表現 / `schema_version` /
    serde ラウンドトリップ / `validate()`（stable code 付き診断）
  - engine `ui_document.rs`: `UiBindings` / `UiEvents` リソース、
    `UiDocumentOverlay`（`UiSystem` 実装のインタープリタ）、
    `load_ui_document(path)`
  - `App::add_ui_font(name, bytes)`（CJK フォント注入）
  - テスト（下記）
- Out（Phase 54 以降）:
  - `engine.ui_document` コンポーネント / registry / manifest 統合
  - image ノード・ドキュメント include・ホットリロード
  - Rhai / BT へのイベント配線（`UiEvents` に積むところまでが本 Phase）
  - エディタでの UI 編集

## Design Decisions

ADR 0046 §3-§6 に従う。実装上の詳細:

- ノード `id` はドキュメント内一意の文字列。egui の `Id` 生成と診断に使う。
- バインディング可能な文字列プロパティは
  `"content": "literal"` または `"content": { "$bind": "score" }`。
  Rust 側は `enum UiString { Literal(String), Bind(String) }` として
  serde(untagged) で表現する。
- `UiBindings` は `BTreeMap<String, UiBindingValue>`
  （`UiBindingValue = Text(String) | Number(f64) | Flag(bool)`）。
  数値は表示時に整形（整数値なら小数点なし）。
- 未解決バインディングは `--` を描画し、ドキュメント+名前につき 1 回だけ
  警告診断を `UiRuntimeDiagnostics` リソースに積む（panic 禁止）。
- `UiEvents` は `Vec<String>`。ボタンクリックで push、ホストがフレーム毎に
  drain する。`App::run_ui_systems` 呼び出し前に前フレーム分をクリア。
- anchor は 9 方位（top_left 〜 bottom_right）。表示矩形（`UiContext` が持つ
  `UiViewport::rect()`）を基準に `egui::Area::fixed_pos` で配置する。
  オーサリング値のスケールはターゲット画面サイズ側で決まる（ADR 0090）。
- validation 診断コードは `ui.duplicate_node_id` / `ui.empty_node_id` /
  `ui.unsupported_version` / `ui.non_finite_number` / `ui.empty_event_name`
  の形式（既存 `scene.*` コードの命名に合わせる）。
- インタープリタの純粋部分（バインディング解決・数値整形・anchor 座標計算）
  は egui 非依存の関数に切り出してユニットテストする。egui 実描画の検証は
  `egui::Context::run` に合成入力を渡すヘッドレステストで行う（egui は
  ウィンドウなしで動作する）。

## Implementation Plan

- 53-A: authoring `ui` モジュール（データモデル + validate + テスト）
- 53-B: engine `ui_document.rs`（リソース + インタープリタ + loader）
- 53-C: `App::add_ui_font` + egui context へのフォント install 配線
  （standalone runner と editor Play ホスト両方）
- 53-D: テストと 4 ゲート

## Tests

- authoring: serde ラウンドトリップ（全ノード種 + `$bind`）、version 欠落
  （v1 扱い）、未来 version 拒否、重複 id / 空 id / 非有限数 / 空 event 名の
  診断、`default()` ドキュメントが valid であること
- engine: バインディング解決（literal / bind 解決 / bind 欠落で `--` +
  診断 1 回のみ）、数値整形、anchor 9 方位の座標計算、ヘッドレス egui で
  text が描画されること・ボタンクリック合成入力で `UiEvents` に event 名が
  積まれること、`load_ui_document` の成功・失敗（JSON 破損 / version 超過）

## Cautions

- `.rs` に日本語を書かない。`*.ui.json` は永続フォーマットなので
  `schema_version` を最初から入れ、変更時は移行テスト必須。
- egui はすでに engine の依存（Phase 24）。新規クレート・新規依存を
  追加しない。
- `renderer` / `ecs` / `authoring` の依存方向を変えない
  （engine → authoring の既存エッジのみ使用）。
- フォント install は egui context ごとに 1 回。毎フレーム
  `set_fonts` を呼ぶとフォントアトラス再構築で激重になるので注意。

## Prohibited

- VDOM / reconciler / retained ツリーの導入（ADR 0046 で却下済み）
- UI ドキュメントから ECS コンポーネントパスへの直接バインド（v1 却下）
- コールバック（関数）のシリアライズ
- egui 以外の UI ライブラリ追加

## Completion Criteria

- `*.ui.json` を `load_ui_document` で読み、`UiDocumentOverlay` を
  `add_ui_system` に登録すると HUD が描画される（ヘッドレステストで保証）
- ボタンクリックが `UiEvents` に event 名として届く
- `UiBindings` の値変更が次フレームの表示に反映される
- 4 ゲート（fmt / clippy / test / doc）パス

## Feeds Into

- Phase 54: UI Interactivity & Authoring Integration（registry /
  イベント配線 / ホットリロード / image）
- Phase 62: busters_lite の HUD / メニュー
