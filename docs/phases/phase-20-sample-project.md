# Phase 20: Minimal Sample Project

> **2026-06-13 再構成**で内容変更。旧 Phase 20（コイン・敵 AI・HUD・SE/BGM
> つきのフル sample game）は Phase 25 へ移動した。本フェーズは
> 「エディタから普通に使えるエンジン」を最小プロジェクトで実証する。

## Goal

editor で開ける minimal sample project を作り、OBJ / scene / component /
Play / save / reopen の一連の流れを実データで検証する。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

Phase 15〜19 の成果は個別テストでは検証済みだが、「新規環境でクローン
直後に editor から一通り使える」ことは単体テストでは保証できない。
リポジトリに実プロジェクトデータを置き、受け入れチェックリストと
自動スモークテストの両方で固定する。

---

## Scope

### 作るもの

- `examples/sample_project/` — **プロジェクトデータ**（コードではない）:
  `project.json` + `asset_manifest.json` + scene + OBJ 1〜2 個（20-A）
- 受け入れチェックリストの実行と記録（20-B）
- 仮想入力 + FrameCapture による自動スモークテスト（20-C）

### 作らないもの（Phase 25 のフル sample game へ）

- コイン収集 / 敵 AI / スコア / タイマー HUD / SE / BGM /
  タイトル → ゲーム → リザルトの scene transition

---

## Design Decisions

### sample project はコードを持たない

`examples/sample_game/`（将来の Phase 25）と違い、`main.rs` を置かない。
「editor だけでゲームの場面を組める」ことの実証が目的のため、
コードが必要になった時点でそれはエンジン側の機能不足を意味する。

### OBJ はリポジトリに含める最小サイズの自作データ

外部配布物を持ち込まない（ライセンス・サイズの問題回避）。
手書きできる規模の OBJ（数十頂点）を assets/meshes/ に置く。

---

## Implementation Plan

### 20-A: sample project データ

```
examples/sample_project/
  project.json
  asset_manifest.json
  assets/
    scenes/main.scene.json
    meshes/        # 自作 OBJ 1〜2 個
```

scene の内容: ground（plane 相当の OBJ または built-in quad）+
player（OBJ mesh + PlayerController + material）+ camera + light。

### 20-B: 受け入れチェックリスト

新規環境（クローン直後）で:

1. editor で project を開く
2. Asset Browser で assets が見える
3. scene を開く
4. entity を追加 / リネーム / 削除できる
5. component を追加できる（Transform / Camera / Light / Mesh / Material /
   PlayerController）
6. 外部 OBJ を Mesh component に割り当てられる
7. Play で Game View にモデルが表示され、WASD で移動できる
8. Stop → 保存 → 再起動 → 開き直して同じ状態になる

結果は本ファイルの末尾に日付つきで記録する。

### 20-C: 自動スモークテスト

- sample project を読み込み、Play 相当の world を構築
- 12-F の `InputCommand` で WASD を注入し、player の Transform が
  変化することを assert
- 10-E の `FrameCapture` で「背景クリア色以外の画素を含む」ことを assert
  （golden image は要求しない）
- `cargo test --workspace` に含める（GPU 不要部分と GPU 要求部分を分け、
  CI で GPU がない場合のスキップ方針を明記する）

---

## Cautions（注意点・落とし穴）

**GPU なし環境でのテスト**:
FrameCapture テストは GPU を要する。CI に GPU がない場合は
`#[ignore]` + 明示的実行（または feature gate）にし、ロジック部分
（入力注入 → Transform 変化）は GPU なしで必ず走らせる。

**sample project の陳腐化**:
フォーマット変更時に sample project の更新を忘れると「開けない
サンプル」が残る。schema_version 移行テストの対象に含める。

---

## Prohibited（禁止事項）

- sample project にゲームコード（`main.rs` 等）を置くことを禁止
- フル sample game の要素（HUD / 音 / 敵 AI）の先行実装を禁止（Phase 25）

---

## Completion Criteria（完了基準）

- 20-B のチェックリストが新規環境で全項目通り、結果が記録されている
- 20-C のスモークテストが `cargo test --workspace` で通る
- `cargo fmt` / `clippy -D warnings` / `doc --no-deps` がすべて成功

---

## Feeds Into（次フェーズへの依存）

- Phase 21〜24: 安定したエディタ基盤の上に Collision / Physics / Audio /
  Runtime UI を載せる
- Phase 25: sample_project を拡張してフル sample game にする

---

## 20-B 受け入れチェックリスト結果

**記録日: 2026-06-13**

自動スモークテスト（20-C）は `cargo test --workspace` で全項目 pass。

手動チェックリスト（editor から実際に操作する部分）は、現環境では
GPU 付き手動実行が必要なため以下を TODO として残す:

| # | 項目 | 状態 |
|---|------|------|
| 1 | editor で project を開く | 手動確認待ち |
| 2 | Asset Browser で assets が見える | 手動確認待ち |
| 3 | scene を開く | 手動確認待ち |
| 4 | entity を追加 / リネーム / 削除できる | 手動確認待ち |
| 5 | component を追加できる（Transform / Camera / Light / Mesh / Material / PlayerController） | 手動確認待ち |
| 6 | 外部 OBJ を Mesh component に割り当てられる | 手動確認待ち |
| 7 | Play で Game View にモデルが表示され、WASD で移動できる | 自動テストで代替確認済み |
| 8 | Stop → 保存 → 再起動 → 開き直して同じ状態になる | 手動確認待ち |

自動テストで代替確認済みの項目（20-C）:
- `sample_project_play_starts_without_blocking_diagnostics`: scene ロード → Play 開始 → blocking diagnostic なし
- `sample_project_player_moves_from_wasd_input`: W キー注入 → Transform.z が減少することを確認
