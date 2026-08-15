# Phase 16: Asset / OBJ / Render の editor 統合

> **2026-06-13 再構成**で新設。旧 Phase 16（Runtime UI）は
> `phase-24-runtime-ui.md` へ後ろ倒し。本フェーズは Phase 14 の
> 完了基準のうち editor 経由で未達だった部分（manifest ベース picker、
> editor Play での外部アセット表示）を引き取る。

## Goal

editor で `asset_manifest.json` を読み書きし、Inspector の Mesh component
で manifest 登録済みの `.obj` を選び、Play で外部 OBJ が Game View に
表示される導線を完成させる。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**現状の問題**: `RuntimePlayState::start(scene)`
（`crates/editor/src/runtime.rs`）は scene しか受け取らず、
`AssetServer` / `AssetManifest` を world resource として挿入しない。
そのため 14-C の manifest 解決は editor Play から到達不能で、
Phase 14 の完了基準「manifest 登録した AssetId が Game View に表示される」
は editor 経由では実質未達。Inspector の asset picker もビルトイン 4 種の
ハードコード（`builtin_asset_choices()`。phase-14 doc が
「14-C 実装時の置き換え対象」と予告済み）。

**Phase 15 の後にこのフェーズが来る理由**: `InspectorHint::AssetRef` で
asset-backed component の picker 挙動を registry 駆動にできるため、
component type 文字列の match を増やさずに manifest picker を実装できる。

---

## Scope

### 作るもの

- editor での `asset_manifest.json` 読み込み・保持・atomic 保存（16-A）
- Asset Browser の「Register Asset」操作 — `.obj` に `AssetId`（ULID）を
  発行して manifest に追記（16-B）
- Inspector の Mesh picker を built-in + manifest 登録済み mesh に置換（16-C）
- `RuntimePlayState::start` への `ProjectRoot` 接続 —
  `AssetServer::with_assets_root` + `AssetManifest` を world resource として
  挿入してから `spawn_from_authoring_scene` を呼ぶ（16-D、破壊的変更）
- temp project + 実 OBJ の統合テスト（16-E）

### 作らないもの

- texture の manifest 解決（GPU の device/queue 依存。後送り継続）
- アセットの hot reload / バックグラウンドロード
- GLTF / GLB（Phase 14 と同じ方針: OBJ のみ）
- manifest の GUI フルエディタ（rename / 削除 UI は最小限。
  手編集 + 再読み込みで足りる範囲は作らない）

---

## Design Decisions

### manifest 保存は `replace_file_contents` を必ず使う

自前の `fs::write` + rename を実装しない（Phase 9-D と同じ方針）。
途中クラッシュで manifest が壊れると全シーンのアセット参照が死ぬため、
atomic write は必須。

### Register Asset は manifest 追記のみ（ファイル移動・コピーをしない）

対象ファイルは既に assets root 内にある前提。assets root 外のファイルを
選んだ場合はエラー診断（`ProjectRoot` のパストラバーサル検証に従う）。
同一パスの二重登録は拒否し、既存エントリを返す。

### built-in アセットは picker に残す

ADR 0021 のビルトインフォールバックは維持。picker の選択肢 =
built-in（triangle / quad）+ manifest 登録済み mesh。
project 未オープンで scene だけ開いている場合は built-in のみ + 診断で
Play 続行する。

### `RuntimePlayState::start` のシグネチャ変更は破壊的変更プロトコル対象

engine 側は変更不要（`spawn_from_authoring_scene` は world resource から
読む設計が 14-C で済んでいる）が、editor 内の呼び出し元と
`ProjectRoot` の受け渡しが変わる。影響範囲を PR に明記する。

---

## Implementation Plan

### 16-A: editor での manifest 読み込み

- Project open 時に `<project root>/asset_manifest.json` を読み込む
  （なければ空 manifest として扱う。ADR 0021 §2 の形式）
- パースエラーは診断に出し、editor は起動を継続する

### 16-B: Register Asset 操作

- Asset Browser のコンテキストメニューに「Register Asset」を追加
- `AssetId`（ULID）発行 → `ManifestEntry { path, name }` 追記 → atomic 保存
- `name` slug は ファイル名から生成し、重複時はサフィックスで一意化
- 未登録ファイルに warning バッジ（`asset.unregistered_file` の可視化）

### 16-C: Mesh picker の manifest 化

- `builtin_asset_choices()` のハードコードを削除し、
  `InspectorHint::AssetRef { kind }` + manifest 一覧から picker を構築
- 表示は `name`、保存値は `Value::AssetRef(AssetId)`（形式変更なし）

### 16-D: RuntimePlayState への接続

```rust
// 変更前
pub fn start(scene: &AuthoringScene) -> Result<PlayStart, PlayError>;
// 変更後（案）
pub fn start(scene: &AuthoringScene, project: Option<&ProjectRoot>)
    -> Result<PlayStart, PlayError>;
```

- `Some(project)`: `AssetServer::with_assets_root(project.assets_root())` と
  読み込み済み `AssetManifest` を world に insert してから spawn
- `None`: 従来どおり built-in のみ（後方互換）+ 診断

### 16-E: 統合テスト

- temp dir に project + manifest + 最小 OBJ（手書き三角形）を生成
- manifest 登録 → scene の `asset_ref` 参照 → spawn → mesh が built-in
  fallback でないことを assert
- ファイル欠損ケース: `asset.missing_file` 診断 + fallback で続行

---

## Cautions（注意点・落とし穴）

**OBJ のスケール・単位はモデル依存**:
巨大 / 極小モデルは「表示されない」ように見える。Play 開始時の
bounding box 診断（情報レベル）か、最低限ドキュメントに記載する。

**manifest と Asset Browser の整合**:
manifest はプロジェクト直下（assets/ の外）にあり Browser の走査対象外
（ADR 0021 §3 の意図した仕様）。Browser に表示するのは
「登録状態のバッジ」であって manifest ファイル自体ではない。

**Windows のパス比較**:
二重登録判定はパス文字列の単純比較をしない。`ProjectRoot` の正規化を
通した上で比較する（大文字小文字・セパレータ）。

---

## Prohibited（禁止事項）

- `"$type": "asset_path"` の導入を禁止（ADR 0021 / spec §7.4。永久却下）
- manifest を自前の `fs::write` で保存することを禁止
- `AssetId` を picker 以外の場所でデタラメに生成することを禁止（ADR 0025）
- texture の manifest 解決をこのフェーズで実装することを禁止（後送り）

---

## Completion Criteria（完了基準）

- Asset Browser から `.obj` を manifest に登録できる
- Inspector の Mesh picker に manifest 登録済み mesh が表示され、選択できる
- Play で外部 OBJ が Game View に表示される
- built-in triangle / quad は従来どおり動く
- ファイル欠損時は fallback + `asset.missing_file` 診断でゲーム続行
- temp project + 実 OBJ の統合テストが `cargo test --workspace` で通る

---

## Feeds Into（次フェーズへの依存）

- Phase 17: 外部アセット込みの編集ループ安定化
- Phase 20: minimal sample project が manifest + OBJ を実データとして使う
