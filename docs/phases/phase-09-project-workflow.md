# Phase 9: Project / Asset / Scene Workflow

## Goal

エディタで「プロジェクトを開き、assets/ を見て、scene / graph を編集・保存・再読み込みできる」状態を作る。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**なぜ今か**:  
Phase 8 までで Behavior Tree のグラフキャンバスは動いている。しかしファイルベースの保存がなく、
毎回コードで scene を組み立てる必要がある。この状態ではゲームを作ることができない。  
「ゲームを作る場所」を先に作らないと、Phase 11 以降に追加するシステムをテストする環境がない。

**なぜ collision や physics より先か**:  
collision がどれほど正しく動いても、editor から scene を組み立てて Play して確認できなければ
開発ループが成立しない。基盤を先に作る。

**なぜ `ProjectRoot` を `engine-authoring` に置くか（ADR 0023）**:  
`ProjectConfig` / `ProjectRoot` は GUI に依存しない。`crates/authoring/src/project.rs` に置くことで
CLI / MCP が後から同じ型を再利用できる。editor crate への仮置きは公開 API の移動コストが生じるため却下。
仕様 §16・§17 により、パスの安全検証は editor / CLI / MCP の共有境界（authoring）で行う必要がある。

---

## Scope

### 作るもの

- `ProjectConfig` / `ProjectRoot` — プロジェクトフォルダの管理
- `AssetBrowser` — assets/ 以下のファイル一覧（GUI）
- Scene / Graph のファイルからの読み込み（`EditorSession::open_scene` / `open_graph`）
- 保存 / Save As / Dirty Flag
- Scene Hierarchy パネル（エンティティ一覧）
- Entity Inspector パネル（コンポーネント編集）

### 作らないもの（次フェーズ以降）

- Play / Stop ボタン（Phase 10）
- runtime でのゲーム実行（Phase 10）
- OBJ / PNG ファイルのロード（Phase 14）
- 複数シーンの切り替え（Phase 18 以降、旧 15）

---

## Design Decisions

### なぜ `ProjectRoot::resolve_asset()` でパスを検証するか

assets/ の外のファイルを editor が読み書きできると、ユーザーが意図せずシステムファイルを
上書きするリスクがある。パストラバーサル（`../../etc/passwd` 等）を防ぐため、
全アセットパスは `resolve_asset()` を通して assets_root 内に制限する。

### なぜ Scene Hierarchy と Entity Inspector を Phase 9-E にまとめるか

Hierarchy と Inspector は独立して使える機能ではない。Hierarchy なしで Inspector だけある状態は
「選択対象がない」ため動作しない。セットで実装することで「開いて、選んで、編集して、保存できる」が
一気通貫になり、Phase 10 の Play 動作確認環境として機能する。

### なぜアトミック書き込みを使うか

保存中にクラッシュすると元ファイルが壊れる。`engine_authoring::persist::replace_file_contents` が
`.tmp` 書き込み → `rename` の原子書き込みを実装済み（Windows では `MoveFileExW` を使用）。
Phase 9-D はこの共有ヘルパーを直接使う。手動での `.tmp` / `fs::write` / `rename` の再実装は禁止。

### なぜ Save 時にファイルダイアログに `rfd` クレートを使うか

Windows / macOS / Linux で native のファイルダイアログを出せる。egui 組み込みのダイアログは
見た目が OS と合わないため UX が悪い。`rfd` は非同期にも対応しており eframe との相性が良い。

### Asset Browser のリフレッシュタイミング

毎フレーム `read_dir` を呼ぶとパフォーマンスへの影響が大きい。  
ウィンドウがフォーカスを取得した時 + 明示的な「Refresh」ボタン操作時のみ再走査する。

---

## Implementation Plan

### 9-A: ProjectConfig と ProjectRoot

場所: `crates/authoring/src/project.rs`（GUI 依存なし）

```rust
pub struct ProjectConfig {
    pub name: String,
    pub version: u32,
}

pub struct ProjectRoot {
    path: PathBuf,
    config: ProjectConfig,
}

impl ProjectRoot {
    pub fn open(path: &Path) -> Result<Self, ProjectError>;
    pub fn create(path: &Path, config: ProjectConfig) -> Result<Self, ProjectError>;
    pub fn resolve_asset(&self, relative: &str) -> Result<PathBuf, ProjectError>;
    // assets/ 外のパスは PathTraversal エラーを返す
}
```

`project.json` の形式（`schema_version` は ADR 0020 ポリシーに従い必須）:
```json
{ "schema_version": 1, "name": "MyGame" }
```

標準ディレクトリ（`create()` 時に自動生成）:
- `assets/scenes/`
- `assets/graphs/`
- `assets/textures/`
- `assets/meshes/`
- `assets/audio/`

### 9-B: Asset Browser

場所: `crates/editor/src/asset_browser.rs`

```rust
pub enum AssetKind {
    Scene,      // .scene.json
    Graph,      // .graph.json
    GraphView,  // .graph.view.json
    Texture,    // .png .jpg .jpeg
    Mesh,       // .obj .gltf .glb
    Audio,      // .wav .ogg .mp3
    Unknown,
}

pub struct AssetEntry {
    pub path: PathBuf,   // assets/ からの相対パス
    pub kind: AssetKind,
    pub name: String,    // 拡張子なしのファイル名
}

pub struct AssetBrowser {
    entries: Vec<AssetEntry>,
    selected: Option<usize>,
}

impl AssetBrowser {
    pub fn refresh(&mut self, assets_root: &Path);
    // 最大4階層まで走査。シンボリックリンクのループ防止のため深さ制限必須
}
```

### 9-C: Scene / Graph Open

`EditorSession` に `CurrentDocument` を追加:

```rust
pub enum CurrentDocument {
    None,
    Scene { scene: AuthoringScene, path: PathBuf, is_dirty: bool },
    Graph { graph: Graph, view: Option<GraphView>, graph_path: PathBuf, view_path: Option<PathBuf>, is_dirty: bool },
}
```

読み込みフロー: `fs::read_to_string` → JSON パース → Authoring バリデーション → `CurrentDocument` にセット。  
失敗はすべて `EditorSession.diagnostics` に出す。パース失敗でパニックしない。

### 9-D: Save / Dirty Flag

```rust
impl EditorSession {
    pub fn save(&mut self) -> Result<(), EditorError>;
    pub fn save_as(&mut self, path: PathBuf) -> Result<(), EditorError>;
    pub fn is_dirty(&self) -> bool;
}
```

アトミック書き込み（`engine_authoring::persist::replace_file_contents` を使う）:
```rust
engine_authoring::persist::replace_file_contents(&path, json.as_bytes())?;
```

キーボードショートカット: `Ctrl+S` → `save()`、`Ctrl+Shift+S` → `save_as()` (rfd ダイアログ)

### 9-E: Scene Hierarchy + Entity Inspector

**Scene Hierarchy パネル**（左サイド）:
- 現在の `AuthoringScene` のエンティティを一覧表示
- クリックで `selected_entity: Option<EntityId>` をセット
- `[+]` ボタン → `AuthoringCommand::CreateEntity`
- 右クリック → Delete Entity（v1: 子エンティティが存在する場合は `entity.has_children` 診断で失敗; cascade 削除は後フェーズ）

**Entity Inspector パネル**（右サイド）:
- 選択エンティティのコンポーネントを `BTreeMap` の順で表示
- `Value::F64` → `egui::DragValue`、`Value::String` → `egui::TextEdit` 等
- 編集 → `AuthoringCommand::SetComponentValue` → `Transaction::begin / apply / commit`
- `[+ Add Component]` ボタン + `ComponentSchemaRegistry` からのドロップダウン

---

## Cautions（注意点・落とし穴）

**Asset Browser の深さ制限を必ず入れる**:  
シンボリックリンクが循環していると `read_dir` が無限ループする。
深さカウンタを渡してリミット（4 階層）で打ち切る。

**egui の `DragValue` は浮動小数点のまま保持する**:  
`Value::F64` を `f32` に変換して表示すると精度が落ちる。
`DragValue` は `f64` で扱う。

**`Transaction::commit` の失敗を必ず UI に出す**:  
`commit` がエラーを返した場合、サイレントに捨てると inconsistent な状態になる。
Diagnostics パネルに必ず表示する。

**`open_scene` / `open_graph` は未保存の変更を確認するダイアログを出す**:  
`is_dirty == true` の状態で別ファイルを開くと変更が失われる。
「保存しますか？」ダイアログを出してから切り替える。

---

## Prohibited（禁止事項）

- `ProjectRoot` が GUI に依存することを禁止（`engine-authoring` に置く）
- `AssetBrowser::refresh()` を毎フレーム呼ぶことを禁止
- パスのバリデーションをスキップして `fs::read_to_string` を直接呼ぶことを禁止
- `Transaction` を bypass して `AuthoringScene` を直接変更することを禁止
- ファイル書き込みを atomic でない方法（直接 `fs::write(path)` のみ）で行うことを禁止

---

## Completion Criteria（完了基準）

- プロジェクトフォルダを editor から開ける
- Asset Browser で assets/ 以下の scene / graph / texture / mesh ファイルを確認できる
- .scene.json をダブルクリックで開き、Scene Hierarchy にエンティティが表示される
- Inspector でコンポーネントの値を編集し、Ctrl+S で保存できる
- エディタを再起動後に同じ scene を開いて変更が残っている
- 読み込み失敗が Diagnostics パネルに表示される

---

## Feeds Into（次フェーズへの依存）

- Phase 10: `CurrentDocument::Scene` の中身を Play 時に runtime world に変換する
- Phase 14: Asset Browser のエントリをクリックして entity に mesh/texture を割り当てる