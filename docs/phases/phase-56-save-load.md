# Phase 56: Save / Load

Status: 実装完了（2026-07-11）。4 ゲートすべてパス。

実装時の決定・逸脱（記録）:

- atomic write は `engine_authoring::replace_file_contents`（Phase 16-B・
  Windows `MoveFileExW` 対応込み）を再利用。新規実装なし。
- `save_get` のスナップショットは `ScriptEngine::set_save_snapshot` 方式
  （input スナップショットと同型）とし、`run_on_start/update/event` の
  公開シグネチャは不変（完全 additive）。
- コマンドは `save_sets` / `save_ops` の 2 本の Vec（`ComponentSetCommand`
  と同型）。同一フック呼び出し内は「全 set → 全 persist」の順で適用される
  ため、`set(a); write(0); set(b); write(1)` のような交互列は厳密な
  スクリプト順にならない（両スロットとも a と b を含む）。phase doc の
  要求（set した値が write に含まれる）は満たす。厳密なインターリーブが
  必要になったら単一コマンドリスト化する。
- 共有適用ロジックは `apply_save_commands`（crate 公開）として
  `scripting_update_system` と `ui_script_event_system` の両方から呼ぶ。

## Goal

セーブスロット（進行状況の永続化）を editor Play とパッケージ済みゲームの
両方で使えるようにし、Rhai スクリプトから読み書き可能にする。

## Why

- パッケージ済みゲーム（ADR 0045）は終了で全状態を失い、ミッション進行や
  解放要素を持つ M1 ターゲットのゲームが成立しない。

## Scope

- In:
  - engine `save.rs`: `SaveValue` / `SaveData`（schema_version 1・
    `*.save.json`）/ `SaveStore`（atomic write・slot 管理）
  - `SaveData` / `SaveStore` の world リソース化とホスト配線
    （player = `<package root>/saves/`・editor Play = `<project root>/saves/`）
  - Rhai: `save_get` / `save_set` / `save_write(slot)` / `save_load(slot)`
    （ADR 0037 コマンドパターン準拠）
  - テスト
- Out:
  - OS 標準セーブディレクトリ（将来 ADR）
  - ネスト値・バイナリ形式・改ざん耐性
  - セーブ内容の editor UI（不要。ファイルは human-readable JSON）

## Design Decisions

ADR 0048 に従う。実装詳細:

- `SaveValue`: `Text(String) | Number(f64) | Flag(bool)`。JSON 表現は
  そのまま string / number / bool（タグなし。serde untagged）。
- `SaveData`: `schema_version: u32` + `BTreeMap<String, SaveValue>`。
  `get / set / remove / keys / clear`。`Default` = 空マップ + v1。
  version 欠落 = v1、未来 version = typed error（material_asset.rs の
  `UnsupportedVersion` パターン）。
- `SaveStore`:
  - `new(root: PathBuf)`・`write_slot(u32, &SaveData)`・`read_slot(u32)`・
    `list_slots()`（存在する slot 番号の昇順）・`delete_slot(u32)`・
    `last_error() -> Option<&str>`（直近の非同期系失敗の観測用。成功で
    クリア）
  - ファイル名 `slot_<n>.save.json`。atomic write（temp + rename。
    Phase 16-B の manifest atomic write パターンを探して踏襲）
  - root ディレクトリは write 時に `create_dir_all`
  - wasm32: `SceneLoader` と同じ形の no-op スタブ（`SaveStoreError` を
    返す）
- Rhai（scripting.rs。既存 `ComponentSetCommand` パターンを踏襲）:
  - `ScriptContextProxy` に `save_get(key) -> Dynamic`（呼び出し前に
    取った `SaveData` スナップショットから。欠落キーは `()`）
  - `save_set(key, value)`（Text/Number/Flag に変換可能な Dynamic のみ。
    不可なら script error ログ）→ `SaveSetCommand` として
    `ScriptCallResult` に蓄積
  - `save_write(slot: i64)` / `save_load(slot: i64)` → 永続化コマンド
  - ディスパッチ側（`scripting_update_system` / `ui_script_event_system`
    のコマンド適用箇所）: set → `SaveData` リソース更新、write →
    `SaveStore::write_slot(現在の SaveData)`、load → 成功時に `SaveData`
    リソース差し替え。IO 失敗は `log::error!` + `SaveStore::last_error`
  - スナップショット引き渡しは既存の input/transform スナップショットと
    同じ Arc 渡しスタイル
- ホスト配線:
  - `App::new`: `SaveData::default()` を挿入（`SaveStore` はホスト提供。
    未挿入で write/load コマンドが来たら error ログのみ）
  - player.rs: パッケージルート解決済みなので `SaveStore::new(root.join("saves"))`
  - editor `RuntimePlayState`（Phase 55 が触った start 系）: project root
    から `SaveStore::new(project_root.join("saves"))`

## Implementation Plan

- 56-A: `save.rs`（モデル + ストア + テスト）
- 56-B: リソース化 + ホスト配線
- 56-C: Rhai コマンド + ディスパッチ適用
- 56-D: テスト + 4 ゲート

## Tests

- serde: 全 `SaveValue` 型のラウンドトリップ・version 欠落 = v1・
  未来 version 拒否
- store: write → read 一致・list_slots 昇順・delete 後に消える・
  read 失敗（欠落 slot / 不正 JSON）が typed error・atomic write
  （書き込み後に temp ファイルが残らない）
- Rhai: `save_set` → リソース反映・`save_get` がスナップショット値を
  返す・`save_write` → ファイル生成 → 別 world で `save_load` → 値復元
  （temp dir。既存 scripting テストのヘルパーを使う）
- 型変換不能な `save_set`（配列等）が error ログになりリソースを汚さない
- `SaveStore` 未挿入で write コマンド → panic せず error ログ

## Cautions

- `.rs` に日本語禁止・英語 rustdoc・回復可能エラーに unwrap 禁止
- `saves/` を engine が .gitignore に書き込まない（ドキュメントで案内）
- atomic write の rename は Windows で上書き先が存在すると失敗し得る。
  既存 manifest atomic write の Windows 対応（remove → rename 等）を
  そのまま踏襲する
- スクリプトコマンドの適用順序: set → write の順（同一フレームで
  set した値が write に含まれること。テストで固定）

## Prohibited

- スクリプトからの直接ファイル IO（ADR 0037 サンドボックス）
- 新規依存（`dirs` 等）
- `*.save.json` フォーマットの無版数変更

## Completion Criteria

- Rhai スクリプトが `save_set` → `save_write(0)` → 再起動相当（新 world +
  `save_load(0)`）→ `save_get` で値を取り戻せる（テストで保証）
- editor Play とパッケージの保存先規則が ADR 0048 §3 どおり
- 4 ゲートパス

## Feeds Into

- Phase 60: Script API v2（追加のスクリプト面）
- Phase 62: busters_lite（ミッション進行・リザルトの保存）
