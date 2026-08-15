# Codex 作業指示書（2026-06-11）

発行: Claude Code（計画担当）。実装: Codex。
本書のタスクはすべて独立しており、上から順に 1 タスク = 1 コミットで進める。

## 必読（着手前）

1. `CLAUDE.md`（エントリポイント）
2. `docs/RUST_CODE_STYLE.md`（毎回必読）
3. `docs/AGENTS.md` のルーティングテーブルで作業場所に対応する文書のみ読む
   - Task 2 → G2 Editor（`docs/phases/phase-10-game-view.md` + ADR 0024 / 0026）
   - Task 3 → G3 Runtime Systems（`docs/phases/phase-12-runtime-foundation.md`）

## 前提条件（人間が先に行う）

作業ツリーに未コミットの完了済み作業（Phase 12-F / 10-E 実装、ADR 0026、
editor 安全性修正、計画ドキュメント）が残っている。**Codex 着手前にこれらを
コミットすること**。Phase 11 の WIP（`mesh.rs` / `mesh.wgsl` /
`phase-11-rendering.md`）は含めない。

## 絶対に触ってはいけないファイル（Phase 11 がローカル進行中のため）

- `crates/engine/src/mesh.rs`
- `crates/engine/src/shaders/mesh.wgsl`
- `crates/engine/src/render.rs`
- `docs/phases/phase-11-rendering.md`
- `Vertex` 型（破壊的変更プロトコル対象）

## 完了の定義（全タスク共通）

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

すべて成功し、警告が増えていないこと。`.rs` 内は英語のみ。

---

## Task 1: `target/` を git 追跡から外す（衛生・約 30 分）— 完了 2026-06-11

**根拠**: `docs/RUST_CODE_STYLE.md` §12 — 生成ドキュメントはコミット禁止。
現在 `GameEngine/target/doc/` 一式が追跡されており、`cargo doc` のたびに
巨大な diff が発生している。

**手順**:

1. リポジトリルート（`C:\RustProject\RustProject`）の `.gitignore` を確認し、
   `GameEngine/target/` が無視されるようエントリを追加する
2. `git rm -r --cached` で `GameEngine/target` を index から外す
   （ワーキングツリーのファイルは削除しない）
3. `git status` に `target/` 配下が一切出ないことを確認する

**注意**: このコミットは削除行が大量になる。他の変更と絶対に混ぜない。

---

## Task 2: 10-E-4 完了 — Capture Frame の PNG 保存（30 分〜1 時間）— 完了 2026-06-11

**現状**: `crates/editor/src/ui/mod.rs` の "Capture Frame" ボタンは
`FrameCapture` を取得し、diagnostic にサイズを表示するだけ。

**やること**: キャプチャを PNG ファイルとして保存する。

1. `crates/editor/Cargo.toml` に PNG エンコーダを追加する。`image` ではなく
   軽量な `png` クレートを使う（依存評価を PR 説明に 1 行書く。
   **engine には追加しない** — ADR 0026 の境界）
2. エンコード処理は UI から分離した関数にする（テスト可能にするため）:
   ```rust
   fn encode_frame_png(capture: &FrameCapture) -> Result<Vec<u8>, ...>
   ```
3. ボタンクリック時: `rfd::FileDialog`（導入済み）の save dialog で保存先を
   選ばせる（デフォルトファイル名 `capture.png`）→ エンコード → 書き込み
4. 診断:
   - 成功: 既存の `editor.runtime.frame_captured`（Info）のメッセージを
     保存先パス入りに変更
   - 失敗: 新コード `editor.runtime.capture_save_failed`（Error）
5. テスト: `FrameCapture` の固定データ（例: 2x2 の既知色）を encode →
   `png` で decode し、サイズとピクセルが一致することを assert
6. ドキュメント同期: `docs/phases/phase-10-game-view.md` の 10-E-4 を
   「完了」に更新し、`docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` の
   「残: 10-E-4 の PNG 保存のみ」の記載を解消する

**禁止**: engine クレートへの画像コーデック追加。OS スクリーンキャプチャ。

---

## Task 3: 12-A 完了 — `Time.frame_count`（30 分〜1 時間）— 完了 2026-06-11

**仕様**（`docs/phases/phase-12-runtime-foundation.md` §12-A）:

1. `crates/engine/src/time.rs` の `Time` に `pub frame_count: u64` を追加
   （rustdoc 必須。`Default` 経由の構築は影響なしだが、**struct literal で
   `Time` を構築している箇所を workspace 全体で検索して更新する**）
2. インクリメント箇所は 2 つ:
   - `crates/engine/src/app.rs` の `RedrawRequested` 内、schedule 実行前
   - `crates/editor/src/runtime.rs` の `RuntimePlayState::tick` 内
     （windowed runtime と editor Play の両方で進む値にする）
3. テスト:
   - editor 側: 既存の tick テストに「tick 後に `frame_count` が増える」
     assert を追加
4. ドキュメント同期: phase-12 doc の 12-A と
   `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` の 12-A を完了に更新

---

## Task 4: ADR 0022 レガシー結合フォーマット読み込みの撤去（30 分〜1 時間）— **完了 2026-06-11**

`docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` §9-C の「ADR 0019 形式
（`format_version: 1` の結合 `.json`）の読み込み対応は Phase 10 出荷後に
撤去予定」に基づく。Phase 10 完了済みのため、人間承認のうえ撤去する。

**撤去対象（調査済み）**:

1. `crates/editor/src/document.rs`:
   - `open_legacy_from_path`（line 238 付近）と `derive_legacy_target_paths`
   - `OpenDocumentError::LegacyFormatVersionMissing` バリアント
     （`Display` / `source` の対応アームも）
   - legacy 系テスト一式（`open_legacy_from_path_*` /
     `open_legacy_target_paths_*`）
   - `CurrentDocument` の rustdoc にある legacy 言及（line 57 付近）
2. `crates/editor/src/session.rs`:
   - `EditorSession::open_legacy_combined`（line 697 付近）と import
   - テスト `open_legacy_combined_loads_and_marks_dirty` /
     `open_legacy_combined_fails_on_unsaved_changes` と
     テストヘルパー `write_legacy_combined`
3. `crates/editor/src/ui/mod.rs` の open ディスパッチ（line 357-363 付近）:
   `.scene.json` / `.graph.json` 以外の else 分岐が legacy open を
   呼んでいる。撤去後は診断
   `editor.open_unsupported_file`（Warning、
   "only .scene.json and .graph.json documents can be opened" の趣旨）
   を push して open しない。Asset Browser のダブルクリック経路も
   同じディスパッチを通ることを確認する

**ドキュメント同期**:

- `docs/adr/0022-project-document-file-layout.md` に撤去日
  （2026-06-11 以降の実施日）を追記
- `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` §9-C のレガシー段落を
  「撤去済み」に更新
- `docs/AGENTS.md` §6 の「ファイル保存（Phase 8-C, ADR 0019 の combined
  format）」の行を現状（ADR 0022 分離形式のみ・legacy 読み込み撤去済み）に合わせる

**テスト**: 撤去後、「`format_version` 付き結合 `.json` を開こうとすると
`editor.open_unsupported_file` 診断が出て document が変わらない」ことを
確認するテストを 1 本追加する（黙って壊れたのではなく、意図して
非対応にしたことを回帰で固定するため）。

---

## 進め方のルール

- 1 タスク = 1 コミット。コミットメッセージは英語で変更内容を要約
- 単一クレート内の判断は PR 説明に記録（ADR 不要・`docs/AGENTS.md` §4）
- 迷ったら最小・可逆な変更を選び、勝手にスコープを広げない
- 本書のタスク完了後、この指示書（`docs/HANDOFF_CODEX_2026-06-11.md`）の
  該当タスクに「完了」と日付を追記する
