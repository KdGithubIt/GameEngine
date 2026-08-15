# GameEngine — Agent Entry Point

## 毎回必ず読む

```
docs/RUST_CODE_STYLE.md
docs/DEVELOPMENT_WORKFLOW.md
```

## タスクに応じて読む文書を決める

`docs/AGENTS.md` を開き、作業場所に対応するグループを探す。
そのグループに書かれたドキュメントだけを読む。それ以外は読まない。

## ローカルでの引き渡し前ゲート

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Windows では `scripts/validate.ps1`、Linux/macOS では `scripts/validate.sh` でこの full-workspace core gate を一括実行できる。

## CIでの完了判定

Windows Validation の `full` mode は次の5コマンドを実行する。

```
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

通常のPRは `affected`、文書のみのPRは `docs` になることがある。選択された mode と scope は `docs/DEVELOPMENT_WORKFLOW.md` を正とし、`affected` 成功を workspace 全体の5検証成功として扱わない。

## 絶対にやってはいけないこと

- `Vertex` のフィールド構成・順序を黙って変えない（破壊的変更プロトコルを守る）
- シリアライズ済みフォーマット・`StableId` のフォーマットを無断変更しない
- 公開 API やクレート境界を変えたとき、呼び出し元の更新を同一 PR に含めない
- クレートの循環依存を作らない
- ライブラリコードの回復可能なエラーに `unwrap()` / `panic!()` を使わない
- `.rs` ファイル内に日本語を書かない

## 設計上の判断に迷ったとき

1. 最小・可逆な変更を優先する
2. `crates/authoring` と `crates/engine` の分離を崩さない
3. クレート境界をまたぐ決定は ADR を書いてから実装する
4. フォーマットや安定 ID を黙って変えない
