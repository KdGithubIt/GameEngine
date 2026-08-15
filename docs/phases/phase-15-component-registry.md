# Phase 15: Component Definition / Registry / Inspector 基盤

> **2026-06-13 再構成**で新設。旧 Phase 15（Scene Management）は
> `phase-18-runtime-scene-loading.md` へ縮小移設した。
> 本フェーズは ADR 0025 が「Phase 15」に割り当てた
> `PropertyPath` / `SetProperty` を含む（割当意図は不変）。

## Goal

新しい component を追加するとき、schema / default value / spawn /
inspector metadata の登録が 1 箇所で完結する基盤を作り、Inspector を
field 単位編集 + クリーンな undo に引き上げる。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**現状の問題**: component を 1 つ足すと最低 4 箇所に手が要る:

1. engine の component struct + system
2. `scene_bridge.rs` の type 定数と spawn 分岐
3. `ComponentSchemaRegistry`（authoring）または editor 側
   `addable_component_registry()` での schema + default 登録
4. editor の inspector 特殊扱い（`builtin_asset_choices()` 等）

実害も出ている: 13-A で実装済みの `PlayerController` / `OrbitCamera` /
`FollowCamera` は registry 未登録でエディタから追加できない。
Camera / Light は authorable な component type 自体が存在しない。

**なぜ Phase 16（asset 統合）より先か**: Phase 16 で manifest ベースの
Mesh picker を作るとき、asset-backed component の inspector 挙動を
`InspectorHint` で駆動できるようにしておくと、component type 文字列の
match をこれ以上増やさずに済む。

---

## Scope

### 作るもの

- ADR 0027 の Accept（15-A）
- engine 所有の `ComponentRegistry` / `ComponentDefinition`（15-B）
- 既存 4 component（transform / player_marker / mesh / material）の
  挙動不変の移行（15-B）
- `engine.camera` / `engine.directional_light`（+ ambient）/
  `engine.player_controller` の authorable 化（15-C）
- `PropertyPath` / `AuthoringCommand::SetProperty`（15-D、ADR 0025）
- undo coalescing: ドラッグ 1 回 = undo 1 ステップ（15-E）

### 作らないもの

- リフレクション / derive macro による自動 component discovery
  （Open Decision のまま。registry は手書き登録）
- texture の manifest 解決（GPU 依存、後送り継続）
- `OrbitCamera` / `FollowCamera` の authorable 化（Phase 19-B）

---

## Design Decisions

### ComponentRegistry は engine 所有（ADR 0027）

spawn 関数が `World` / `AssetServer` 等の engine 型を要するため、
authoring には置けない（依存方向は authoring ← engine のまま）。
authoring の `ComponentSchemaRegistry` は schema 専用契約として温存し、
CLI / MCP は変更なし。詳細・代替案は
`docs/adr/0027-component-definition-registry.md`。

### Light は entity component として author し resource へミラー

`AmbientLight` / `DirectionalLight` は現在 world resource。
entity component（`engine.directional_light` 等）として author し、
system が毎フレーム resource へミラーする。Hierarchy / Inspector / undo の
既存導線にそのまま乗るのが利点。複数 directional light が置かれた場合は
最初の 1 個（決定的順序）を採用し、残りに診断を出す。

### scene に camera があれば default camera を挿入しない

`RuntimePlayState::start` の default camera 仮挿入は
「scene に `engine.camera` が 1 つもない場合」のフォールバックに限定する。

### undo coalescing は commit-on-release 方式

ドラッグ中は preview 値を UI ローカルに保持し、`drag_stopped()` で
1 transaction をコミットする。AuthoringSession のクローン式 undo
（ADR 0005）は 1 コミット 1 クローンのため、毎フレームコミットの
性能問題も同時に解決する。マージウィンドウ方式（時間ベース結合）は
複雑さに見合わないため採らない。

---

## Implementation Plan

### 15-A: ADR 0027 の Accept

人間レビューで Accept し、`docs/adr/README.md` の表を移動する。

### 15-B: ComponentRegistry 実装と既存 component の移行

1. `ComponentDefinition { schema, spawn, inspector }` と
   `ComponentRegistry` を engine に新設
2. `engine::components::builtin_registry()`（パスは実装時決定）に
   既存 4 component を登録
3. `scene_bridge` の spawn 分岐を registry ディスパッチに置換
4. editor の `addable_component_registry()` / `builtin_asset_choices()` を
   registry 由来に置換
5. ゲート: 既存 bridge / editor テストが**無変更で**通ること

### 15-C: Camera / Light / PlayerController の authorable 化

- schema（fields / defaults / display metadata）+ spawn + inspector hint を
  registry に登録
- light ミラー system と「複数 light 診断」を追加
- default camera 挿入の抑止条件を変更
- 事前確認: 未知 component type を含む scene が旧エディタ・CLI で
  保持されること（additive 変更の確認）

### 15-D: PropertyPath / SetProperty

- spec 既定義の `PropertyPath` 仕様に従う（独自構文禁止）
- apply / inverse / 診断（存在しない path・型不一致）をテストで固める
- Inspector を field 単位編集へ切替。`SetComponentValue` は丸ごと置換用に共存

### 15-E: undo coalescing

- DragValue 系 widget の編集を gesture 単位で transaction 化
- テスト: ドラッグ 1 回で undo スタックが 1 エントリしか増えない

---

## Cautions（注意点・落とし穴）

**authoring command の追加はシリアライズ契約**:
`SetProperty` は CLI / MCP / editor が共有する契約。診断コードを固定し、
spec とずれないこと。

**registry 移行 PR に機能追加を混ぜない**:
15-B は挙動不変リファクタとして単独で出す。15-C 以降を混ぜると
回帰の切り分けができない。

**`egui` の drag 終了検出**:
`drag_stopped()` がフォーカス喪失・ウィンドウ切替で発火しないケースを
確認する。取りこぼすと未コミットの編集が消える。

---

## Prohibited（禁止事項）

- リフレクション / 自動 discovery の先取り実装を禁止
- `AssetId` を inspector でデタラメに生成することを禁止（ADR 0025）
- path 形状の component 値の導入を禁止（ADR 0021）
- `.rs` ファイル内の日本語を禁止（CLAUDE.md）

---

## Completion Criteria（完了基準）

- 新規 component の追加 = registry 登録 1 箇所 + テストで、
  Add Component / Inspector / Play に反映される
- Camera / Light / PlayerController をエディタから追加・編集できる
- scene に camera があるとき default camera が挿入されない
- 数値ドラッグ 1 回が undo 1 ステップになる
- `SetProperty` の apply / inverse / 診断テストが通過する
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 16: `InspectorHint::AssetRef` が manifest ベース picker の土台
- Phase 17: camera / light 編集の安定化は 15-C が前提
- Phase 19: `OrbitCamera` / `FollowCamera` の authorable 化が
  registry 登録 1 箇所で済む
