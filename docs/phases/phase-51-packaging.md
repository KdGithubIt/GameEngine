# Phase 51: Packaging End-to-End

Status: 実装完了（2026-07-05）。4 ゲートすべてパス。
tiny_goal_project を ADR 0045 レイアウトでパッケージし、`game.exe` 単体で
12 秒間の起動再生を確認済み（stderr 警告ゼロ）。

## Goal

エディタプロジェクト（データのみ）から、単体で実行できるデスクトップ
パッケージを生成する。OSS Pre-Public Plan Workstream 3 Option B の実施。

## Why

- Phase 39（ADR 0034）は解析のみで、実際のパッケージ出力が存在しなかった。
  `build_project` の cargo 呼び出しはデータのみのプロジェクトでは機能しない
  （Cargo.toml がない）。
- 「ゲームを配れる」ことはエンジンの実用性と OSS 再開時の信頼性に直結する。

## Scope

ADR 0045 の決定に従う。

- In: `player` バイナリ（`crates/engine/src/bin/player.rs`）/
  `plan_package`（純粋・MissingAsset を blocking に昇格）/
  `package_project`（レイアウト実出力）/ temp-dir 統合テスト /
  tiny_goal_project への `project_settings.json` 追加
- Out: クロスコンパイル / インストーラ / アセット埋め込み / WASM
  パッケージング / エディタ UI からのワンクリック実行（ボタン配線は
  将来課題。関数は公開済み）

## Design Decisions

- 実行ファイルは汎用 `player` バイナリ（engine クレート内 `[[bin]]`。
  新クレート・新依存なし）。パッケージ = player のコピー + データファイル。
- player は editor Play と同じシステム群（BT / player controller /
  camera controllers / sample game bridge）+ `animation_system` を登録する。
  「エディタで動く = パッケージで動く」を維持するため。
- パッケージルートは第 1 引数、省略時は exe のディレクトリ。
- `plan_package` は copy リストを返す純粋関数。`asset_manifest.json` は
  コピーでなくメモリ上の manifest を再シリアライズして出力する。
- packaging では MissingAsset が blocking（ADR 0034 の解析パスは
  non-blocking のまま互換維持）。

## Implementation Plan（実施済み）

- 51-A: `player.rs`（ProjectRoot::open → ProjectSettings → SceneLoader →
  spawn → システム登録 → App::run。start_scene 欠落は exit code 2）
- 51-B: `build.rs` に `PackageCopy` / `PackagePlan` / `plan_package` /
  `PackageError` / `package_project` + テスト 3 本
- 51-C: tiny_goal_project の実パッケージ起動確認

## Cautions

- player バイナリは対象プラットフォームでビルドされている必要がある
  （`cargo build --release --bin player -p engine`）。
- `project_settings.json` は packaging の前提（start_scene の供給元）。
- wasm32 ターゲットでは player は空 main（デスクトップ専用）。

## Prohibited

- パッケージ出力先をプロジェクト内部（assets/ 以下等）にすること
- `"$type": "asset_path"` の導入（ADR 0021。永久却下）

## Completion Criteria

- `package_project` が ADR 0045 レイアウトを出力する（テストで保証）
- manifest 記載ファイルの欠損が packaging を blocking で止める
- パッケージ出力の `game.exe` が単体でシーンを再生する（実機確認済み）
- 4 ゲートすべてパス

## Feeds Into

- エディタ UI の「Package」ボタン配線
- リリースチェックリスト / OSS 公開時の配布手順
- WASM パッケージング（将来 ADR）
