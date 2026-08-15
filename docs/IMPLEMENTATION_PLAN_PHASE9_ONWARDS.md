# Implementation Plan: Phase 9 Onwards

Date: 2026-06-06  
Status: Active roadmap  
Covers: Phase 9-0, Track W, Phase 9 through Phase 40（最終更新: 2026-06-13、
Phase 15〜20 を「最低限エディタから使えるエンジン」方針で再構成。旧 15〜19 の
Scene Management / Runtime UI / Collision / Physics / Audio は Phase 18・21〜24 へ
移動。旧→新対応表は Phase 15 の直前を参照。ADR 0027〔Accepted〕追加。
Phase 26〜40 を Advanced Authoring ロードマップとして再構成。ADR 0028〔Accepted〕追加）

## 前提知識：現状の正確な把握

### 既に実装済み（思っていたより多い）

| 機能 | 場所 | 状態 |
|------|------|------|
| 深度バッファ | `app.rs` GpuState + `render.rs` pipeline | **完了** |
| `Time { delta_seconds, elapsed_seconds, frame_count }` | `engine/src/time.rs` | 完了 |
| `Input<KeyCode>`, `Input<MouseButton>`, `MouseInput` | `engine/src/input.rs` | 完了 |
| `Handle<T>`, `Assets<T>`, `AssetServer` | `engine/src/asset.rs` | 完了 |
| `spawn_from_authoring_scene()` | `engine/src/scene_bridge.rs` | 完了 |
| `Mesh::triangle()`, `Mesh::quad()` + index buffer | `engine/src/mesh.rs` | 完了 |
| `Camera3D`, `ViewportSize`, aspect更新システム | `engine/src/camera.rs` | 完了 |
| BT Graph Canvas + undo/redo | `crates/editor/` | 完了 |

### 現在の editor が持たないもの（Phase 9 で作る）

- Scene Hierarchy（エンティティ一覧パネル）
- Entity Inspector（コンポーネント編集パネル）
- Project管理（プロジェクトを開く/保存する）
- Asset Browser（assets/ 以下のファイル一覧）
- Play/Stop ボタン

### 重要な制約

**`Vertex` にノーマルがない** (`position`, `color`, `uv` のみ)。  
Phase 11 のライティングは `Vertex` に `normal: [f32; 3]` を追加する**破壊的変更**。
既存の `triangle()`, `quad()` とシェーダーも同時に変更が必要。

---

## Phase 9-0: 契約整備（Phase 9 着手の前提）

目的: ユーザーが保存するファイル形式が Phase 9 で凍結されるため、
不可逆な形式判断だけを先に ADR で確定する。期間目安: 2〜3 日。

### タスク

1. ~~ADR 0020〜0024 をレビューして Accept する（人間承認。Accept 時に
   `docs/adr/README.md` の Proposed 表から Accepted 表へ行を移動し、
   各 ADR の Status を更新する）。~~ **完了 2026-06-11**
2. ADR 0020 実装: scene 形式に `schema_version` を追加
   （`scene.rs::SceneFileRef` / `load.rs::SceneFile`、欠落時は v1 扱い、
   `SceneLoadError::UnsupportedVersion` 追加、ラウンドトリップテスト）。
3. `AuthoringCommand::DeleteEntity` を追加（v1・単一クレート変更のため
   ADR 不要、PR 説明に記録）:
   - 対象に子エンティティが存在する場合、新診断 `entity.has_children`
     （Error 級・コード固定）で失敗させる。子の一括削除（cascade）は
     v1 では行わない。
   - `Change::EntityRemoved` は components を含むエンティティ全体を
     保持し、inverse（再作成）を成立させる。
   - cascade 削除（サブツリー一括 + 逆順再作成 inverse）は、9-E の
     Hierarchy 実装中に必要性を判断する別タスクとする。
   - `SetEntityParent` は本フェーズに**含めない**。reparent UI を導入する
     時点で UI とセットで追加する（消費者なしの serialized contract を
     先行させない）。
4. ~~ドキュメント同期の一括 PR: AGENTS.md（§5/§6/G2/G4/G5）、
   phase-09 の ProjectRoot 配置矛盾を ADR 0023 に合わせて修正、
   spec §20 への Phase 8-A/B/C 完了ノート追記と `crates/graph` 表記修正。
   ※ spec §15.1 の変更は**不要**（ADR 0019 の廃止により spec とのズレ
   自体が解消されるため）。~~ **完了 2026-06-11**

### ゲート対応表（9-0 がブロックする範囲）

| 9-0 成果物 | ブロックする対象 |
|-----------|----------------|
| ADR 0020〜0024 の Accept | 9-A 着手（特に ADR 0023 が 9-A の前提） |
| scene `schema_version` 実装 | 9-D（保存形式の凍結点）。9-A〜9-C はブロックしない |
| `DeleteEntity` 実装 | 9-E のみ |
| ドキュメント同期一括 PR | ブロッカーではない（9-A〜9-B と並走可） |

### 完了基準

- ADR 0020〜0024 が Accepted。
- バージョン付き scene の保存→読込→保存が安定（golden テスト）。
- `DeleteEntity` の apply / inverse / `entity.has_children` 診断テストが通過。
- `cargo fmt --all --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` / `cargo doc --workspace --no-deps` がすべて成功。

---

## Track W: wgpu 22 → 29 移行（Phase 9 と並行する独立トラック）

**Status (2026-06-11)**: 完了。`engine` / `engine-renderer` は
`wgpu = "29"` に統一済みで、`eframe 0.34.3` / `egui-wgpu 0.34.3`
と同一 wgpu major を使っている。Phase 10-C は in-process Game View
として実装済みのため、ADR 0024 の別プロセス Player fallback は
現時点では不要（撤退案として文書上は維持）。

ADR 0024 の前提作業。editor の egui-wgpu (wgpu 29) と engine (wgpu 22) の
分裂を解消する。GPU コードが最小の今が最安値であり、Phase 10-C と
Phase 24（Runtime UI、旧 16）の両方がこの完了に依存する。**リスク台帳は ADR 0024 の
Migration Risk Inventory を参照**。

- 順序: `crates/renderer`（context.rs / surface.rs）→ `crates/engine`
  （render.rs / app.rs / material.rs / mesh.rs）→ examples 2 本。
- 参照実装: 同一ワークスペースの egui-wgpu 0.34.3 が wgpu 29 の生きた
  使用例。wgpu CHANGELOG を併読する。
- 検証ゲート: `hello_window` と `minimal_playable` の実行確認 +
  `cargo test --workspace`。
- タイムボックス: 2 週間。超過または upstream 非互換で停滞した場合、
  ADR 0024 の撤退案（別プロセス Player バイナリ）へ切り替え、10-C を
  Player 方式で再計画する。
- 分担: 機械的な API 追従はエージェント作業可。surface 構成・lifetime に
  関わる差分は人間レビュー必須。
- **10-B（editor への engine 依存追加）のマージは Track W 完了後を推奨**。
  完了前にマージすると editor が wgpu 22 と 29 を二重リンクする
  （コンパイルは通るがビルド時間・バイナリが肥大）。開発自体は
  ブランチで先行してよい。

---

## Phase 9: Project / Asset / Scene Workflow

「ゲームを作る場所」を作る。ファイルベースで保存・再読み込みできる状態を目指す。

---

### 9-A: ProjectConfig と ProjectRoot

**目的**: プロジェクトフォルダを開き、assets/ 構造を管理する。

**実装場所**: `crates/authoring/src/project.rs`（GUI依存なし・ADR 0023 で確定）

```rust
// project.json に保存される（ADR 0020 の方針で schema_version を持つ）
pub struct ProjectConfig {
    pub name: String,
    pub schema_version: u32,
}

// プロジェクトルートディレクトリの安全なラッパー
pub struct ProjectRoot {
    path: PathBuf,
    config: ProjectConfig,
}

impl ProjectRoot {
    pub fn open(path: &Path) -> Result<Self, ProjectError>;
    pub fn create(path: &Path, config: ProjectConfig) -> Result<Self, ProjectError>;
    pub fn assets_root(&self) -> PathBuf;   // <root>/assets/
    pub fn scenes_dir(&self) -> PathBuf;    // <root>/assets/scenes/
    pub fn graphs_dir(&self) -> PathBuf;    // <root>/assets/graphs/
    pub fn meshes_dir(&self) -> PathBuf;    // <root>/assets/meshes/
    pub fn textures_dir(&self) -> PathBuf;  // <root>/assets/textures/
    pub fn audio_dir(&self) -> PathBuf;     // <root>/assets/audio/
    pub fn resolve_asset(&self, relative: &str) -> Result<PathBuf, ProjectError>;
    // パストラバーサル攻撃を防ぐ: assets/ の外に出ないことを保証（既存ファイルの読み取り用）
    pub fn resolve_asset_for_write(&self, relative: &str) -> Result<PathBuf, ProjectError>;
    // 保存先用。fs::canonicalize は未存在パスに失敗するため、親ディレクトリを
    // canonicalize し末尾要素を字句検証する（ADR 0023）
}
```

**project.json の形式**:
```json
{
  "name": "MyGame",
  "schema_version": 1
}
```

**標準ディレクトリ**: `create()` 時に自動生成。存在チェックのみで失敗しない。

**エラー型**:
```rust
pub enum ProjectError {
    NotADirectory(PathBuf),
    MissingProjectFile(PathBuf),
    JsonParse(serde_json::Error),
    Io(std::io::Error),
    PathTraversal { requested: PathBuf },
}
```

**最近開いたプロジェクト**: editor の設定ファイルに保存（`dirs::config_dir()` を使用）。  
`crates/editor/src/preferences.rs` に `EditorPreferences { recent_projects: Vec<PathBuf> }` を置く。

---

### 9-B: Asset Browser

**目的**: assets/ 以下のファイルを GUI で一覧表示する。

**実装場所**: `crates/editor/src/asset_browser.rs`（editor crate のみ）

```rust
pub enum AssetKind {
    Scene,       // .scene.json
    Graph,       // .graph.json
    GraphView,   // .graph.view.json（ADR 0008 の命名に準拠）
    Texture,     // .png .jpg .jpeg .webp
    Mesh,        // .obj .gltf .glb
    Audio,       // .wav .ogg .mp3
    Unknown,
}

pub struct AssetEntry {
    pub path: PathBuf,          // assets/ からの相対パス
    pub kind: AssetKind,
    pub name: String,           // ファイル名（拡張子除く）
}

pub struct AssetBrowser {
    entries: Vec<AssetEntry>,
    selected: Option<usize>,
}

impl AssetBrowser {
    pub fn refresh(&mut self, assets_root: &Path);  // 最大4階層まで走査
    pub fn selected_entry(&self) -> Option<&AssetEntry>;
}
```

**注意**:
- 毎フレームの走査は禁止。`refresh()` はフォーカス取得時 or 明示的な操作時のみ。
- シンボリックリンクのループを防ぐため深さ制限（4階層）を設ける。
- `AssetEntry.path` は `ProjectRoot::resolve_asset()` で安全性を検証済みのパス。
- `asset_manifest.json`（ADR 0021）はプロジェクト直下（assets/ の外）に
  置かれるため、Asset Browser の走査対象外。これは意図した仕様。

**egui での表示**:
- 左下パネル（または左サイドパネル）にツリービュー
- アイコン: 種類別の絵文字か単純な色分け（第一実装は色分けで可）
- ダブルクリック → 9-C の open 処理を呼ぶ

---

### 9-C: Scene / Graph Open

**目的**: Asset Browser から .scene.json または .graph.json を開く。

**実装場所**: `crates/editor/src/session.rs` の `EditorSession` を拡張

```rust
pub enum CurrentDocument {
    None,
    Scene {
        scene: AuthoringScene,
        path: PathBuf,
        is_dirty: bool,
    },
    Graph {
        graph: Graph,
        view: Option<GraphView>,
        graph_path: PathBuf,
        view_path: Option<PathBuf>,
        is_dirty: bool,
    },
}

impl EditorSession {
    pub fn open_scene(&mut self, path: PathBuf) -> Result<(), EditorError>;
    pub fn open_graph(&mut self, path: PathBuf) -> Result<(), EditorError>;
    // .graph.view.json があれば同時に読み込む（なくても失敗しない・ADR 0008 命名）
}
```

**読み込みフロー**:
```
ファイル読み取り (fs::read_to_string)
  → JSON パース
  → Authoring バリデーション
  → CurrentDocument にセット
  → Diagnostics をクリア
```
読み込み失敗はすべて `EditorSession.diagnostics` に出す。パース失敗でパニックしない。

**レガシー結合ファイル（ADR 0022）**: ~~ADR 0019 形式（`format_version: 1` の
結合 `.json`）は読み込みのみ対応する。開いた場合は次回保存時に分離形式
（`*.graph.json` + `*.graph.view.json`）へ変換し、結合形式での新規保存は
行わない。読み込み対応は Phase 10 出荷後に撤去予定。~~ **撤去済み（2026-06-11）**:
Phase 10 完了後に ADR 0022 に従い撤去。`.scene.json` / `.graph.json` 以外の
拡張子は `editor.open_unsupported_file` 診断を出して open しない。

---

### 9-D: Save / Save As / Dirty Flag

**目的**: 編集内容をファイルに保存する。

**Dirty Flag**:
- `AuthoringCommand` が適用されるたびに `is_dirty = true`
- 保存成功後に `is_dirty = false`
- ウィンドウタイトル: `"Engine Editor — MyGame *"` (dirty 時に `*` 付加)

**保存処理**:
```rust
impl EditorSession {
    pub fn save(&mut self) -> Result<(), EditorError>;
    pub fn save_as(&mut self, new_path: PathBuf) -> Result<(), EditorError>;
}
```

**アトミック書き込み**（重要）:

自前の `fs::write` + `rename` は実装**しない**。共有ヘルパー
`engine_authoring::persist::replace_file_contents`（一時ファイル → sync →
rename、Windows は `MoveFileExW`）を必ず使う。
途中でクラッシュしても元のファイルが壊れない。

**不正シーンの保存（決定済み）**: `AuthoringScene::to_canonical_json` は
検証エラーで保存を拒否する。Phase 9 では strict save を維持し、保存失敗の
理由は Diagnostics パネルに表示する。「下書き保存」は必要になるまで
導入しない。

**キーボードショートカット**:
- `Ctrl+S` → `save()`
- `Ctrl+Shift+S` → `save_as()` (rfd クレートのファイルダイアログを使用)

**Save As ダイアログ**: `rfd` クレート（cross-platform native file dialog）を使用。
```toml
[dependencies]
rfd = "0.15"
```

---

### 9-E: Scene Hierarchy + Entity Inspector

**目的**: Scene を開き、エンティティを編集し、保存できる。

これが Phase 9 の最大の実装タスク。

**Scene Hierarchy パネル** (左サイドパネル):

```rust
// EditorApp に追加
selected_entity: Option<EntityId>,
hierarchy_filter: String,   // 検索フィルター
```

egui の表示:
```
Entities
  ├ player
  ├ enemy_01
  └ floor
[+] Add Entity
```

操作:
- クリック → `selected_entity = Some(id)`
- 右クリックコンテキストメニュー → Delete Entity（9-0 で実装済みの
  `AuthoringCommand::DeleteEntity` を使用。子を持つエンティティは
  `entity.has_children` 診断で失敗するため、UI は診断をそのまま表示する）
- `[+]` ボタン → `AuthoringCommand::CreateEntity { id: EntityId::generate(), name: "new_entity".into(), parent: None }`

**Entity Inspector パネル** (右サイドパネル):

選択エンティティのコンポーネント一覧:
```
entity_01JP...
name: player

▼ engine.transform
  x: -0.5  y: 0.0  z: 0.0
▼ engine.player_marker
  (空)
[+ Add Component ▾]
```

実装:
- `AuthoringScene` から `selected_entity` の `AuthoringEntity` を取得
- `entity.components` を `BTreeMap` でイテレート
- 各コンポーネントの値を `Value` 型に応じてウィジェット表示:
  - `Value::F64` → `egui::DragValue`
  - `Value::String` → `egui::TextEdit`
  - `Value::Bool` → `egui::Checkbox`
  - `Value::Object` → 再帰的にフィールド表示

**編集 → コマンド変換**:

フィールド単位の `SetProperty` / `PropertyPath` は**未実装であり Phase 9 では
導入しない**（保留判断）。コンポーネント値を丸ごと置換する既存の
`SetComponentValue` を使う。

```rust
// ユーザーが x を変更したら: 編集後のコンポーネント値全体を作って置換する
let mut value = current_component_value.clone(); // Value::Object
set_object_field(&mut value, "x", Value::F64(new_x));
let cmd = AuthoringCommand::SetComponentValue {
    entity: entity_id.clone(),
    component_type: ComponentTypeId::new("engine.transform"),
    value,
};
let mut tx = session.begin_transaction();
tx.apply(cmd);
match session.commit(tx) {
    Ok(_) => session.set_dirty(true),
    Err(e) => session.add_diagnostic(e.into()),
}
```

**undo/redo**: scene 文書の undo は新規実装せず、
`engine_authoring::AuthoringSession`（scene クローン方式・ADR 0005）を
`EditorSession` に内蔵して使う。`AuthoringScene` は `Serialize` を持たない
ため、ADR 0018 の JSON スナップショット方式は scene 文書には適用しない。

**Add Component ドロップダウン**:
- `ComponentSchemaRegistry` を `authoring::schema` に**新規実装**して型一覧を
  取得（手書き登録で開始。リフレクション方式は Open Decision #2 のまま
  先取りしない）
- 選択 → `AuthoringCommand::AddComponent`

**Phase 9 の完了基準**:
- プロジェクトフォルダを開ける
- Asset Browser で assets/ 以下のファイルを確認できる
- .scene.json を開き、エンティティを選択・編集・保存できる
- 再起動後に変更が残っている

---

## Phase 10: Editor Runtime Preview / Game View

**Status (2026-06-11)**: Phase 10-A/B/C/D 完了。Track W 完了後の
in-process Game View と `editor.runtime.*` diagnostics で実装済み。
10-E（AI Observation）と 12-F（Virtual Input）も同日実装済み
（ADR 0026 Accepted）。

「エディタから実行」ができる状態を作る。

---

### 10-A: Play / Stop 状態管理

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum EditorMode {
    Edit,
    Playing,
}

// EditorApp に追加
editor_mode: EditorMode,
runtime_state: Option<RuntimePlayState>,
```

```rust
struct RuntimePlayState {
    world: engine_ecs::World,
    // Game View へのレンダリングターゲット情報（10-C で詳細）
}
```

**Play ボタン押下フロー**:
1. 現在の `AuthoringScene` をバリデーション
2. バリデーションに失敗したら Diagnostics に出して Play キャンセル
3. `engine_ecs::World::new()` を作成
4. `spawn_from_authoring_scene(&mut world, &scene)` を呼ぶ
5. 基本システムをセットアップ（transform propagation 等）
6. `RuntimePlayState` を作って `runtime_state` にセット
7. `editor_mode = Playing`

**Stop ボタン押下フロー**:
1. `runtime_state = None`（Drop でリソース解放）
2. `editor_mode = Edit`

**重要な不変条件**: Play 中も `AuthoringScene` は変更されない。  
runtime_world と authoring document は完全に分離する。

**段階導入（ADR 0024 §5）**: 10-A/B は Game View なしの「ロジック Play」
として Track W 完了前に完成させてよい（`engine_ecs::App::update()` は GPU
不要）。Hierarchy / Inspector で値の変化を観察できれば完了とする。

**panic 方針（ADR 0024 §6）**: スケジュール tick は `catch_unwind` で包み、
panic 時は `editor.play_panicked` 診断を出して runtime world を破棄し、
Edit モードへ復帰する。

---

### 10-B: Authoring → Runtime World

`engine::scene_bridge::spawn_from_authoring_scene()` を使う（既存）。

**crates/editor の依存関係変更**:

現在: `engine-authoring` のみ  
変更後: `engine` を追加（Play 機能に必要）

```toml
# crates/editor/Cargo.toml
[dependencies]
engine = { path = "../engine" }
engine-authoring = { path = "../authoring" }
```

この変更で `engine` crate の全機能がエディタから使える。

**マージ時期**: この依存追加のマージは **Track W（wgpu 29 統一）完了後を
推奨**（ADR 0024 §5）。完了前にマージすると editor が wgpu 22 と 29 を
二重リンクする（コンパイルは通るがビルド時間・バイナリが肥大）。
開発はブランチで先行してよい。

**セットアップコード**:
```rust
fn start_play(session: &EditorSession) -> Result<RuntimePlayState, PlayError> {
    let scene = session.current_scene().ok_or(PlayError::NoScene)?;
    let diag = scene.validate();
    if diag.iter().any(Diagnostic::is_blocking) {
        return Err(PlayError::InvalidScene(diag));
    }

    let mut world = engine_ecs::World::new();
    // 基本リソースを挿入
    world.insert_resource(Time::default());
    world.insert_resource(Input::<KeyCode>::default());
    world.insert_resource(Input::<MouseButton>::default());
    world.insert_resource(MouseInput::default());

    spawn_from_authoring_scene(&mut world, scene)?;

    Ok(RuntimePlayState { world })
}
```

---

### 10-C: Game View の実装方針

**前提（ADR 0024）**: 本節は **Track W（wgpu 29 統一）の完了がゲート**。
現状は engine が wgpu 22、eframe 0.34 が wgpu 29 で、異バージョン間の
GPU オブジェクト共有は不可能なため、統一前にこの方式は実装できない。

**eframe + egui_wgpu を使ったレンダリング**（統一後の推奨アプローチ）:

eframe の wgpu バックエンドでは、`egui_wgpu::Painter` が内部の wgpu Device/Queue を保持している。  
egui には「wgpu テクスチャを egui 画像として表示する」機能がある。

```rust
// ゲームをレンダーターゲットテクスチャに描画し、その ID を持つ
struct RuntimePlayState {
    world: engine_ecs::World,
    game_texture_id: egui::TextureId,
    game_render_target: wgpu::Texture,
    game_render_view: wgpu::TextureView,
}
```

**egui での表示**:
```rust
// CentralPanel の中で
ui.image(egui::load::SizedTexture::new(
    runtime_state.game_texture_id,
    egui::vec2(width, height),
));
```

**実装手順**:
1. eframe の `CreationContext` から `egui_wgpu::RenderState` を取得
2. wgpu `Texture` を作成（ゲームレンダーターゲット用、サイズは Game View パネルサイズに合わせる）
3. `painter.register_native_texture(device, &view, FilterMode::Linear)` で TextureId を取得
4. 毎フレーム: ゲームの ECS + RenderState でそのテクスチャに描画
5. egui でテクスチャを画像として表示

**注意**: Game View パネルのサイズが変わったらテクスチャを再作成。

**必要なエンジン側リファクタ（ADR 0024 §2-3）**:
- `GpuState::render` は現在ウィンドウサーフェス直結
  （`surface.get_current_texture()`）。任意の `TextureView` へ描画できる
  `render_world(world, color_target, depth_target, size)` 相当を抽出する
  （`render` モジュールは crate 内私有のため公開 API 破壊なし）。
- `ViewportSize` はウィンドウサイズ直結をやめ、レンダーターゲットサイズ
  から導出する（`camera_aspect_system` の入力源を変更）。

**撤退案**: ADR 0024 の別プロセス Player バイナリに一本化。旧記載の
「winit + 手動 egui セットアップ」案は廃止。

---

### 10-D: Runtime Diagnostics

Play 中に発生しうるエラーを構造化して表示:

```rust
pub enum RuntimeDiagnosticKind {
    SceneConversionFailed(Vec<Diagnostic>),
    NoCamera,
    MissingAsset { path: String },
    RenderError(String),
}
```

これらはすべて既存の `Diagnostics` パネルに出す。Play ボタンを押した時点でクリアする。

---

### 10-E: AI Observation — Game View Frame Capture（2026-06-11 追加・同日実装済み）

Game View の現在フレームを `FrameCapture { width, height, rgba8 }` として
読み戻す。AI エージェントへの画面観測・golden image テストの土台
（詳細は `docs/phases/phase-10-game-view.md` §10-E、境界は ADR 0026）。

- capture は生 RGBA8 の読み戻しと editor 側の PNG 保存まで対応。readback はレンダーターゲット所有者
  （現状は editor の `RuntimePlayState::capture_game_view`）に置き、
  第二の消費者が現れたら engine へ抽出（ADR 0026 改訂）。
  AI API 通信・プロンプト構築・AI bridge 向けの画像変換は CLI / MCP /
  agent レイヤー（AI Agent Bridge、planned Phase 40・ADR 0026）が担当
- キャプチャ座標系 = `MouseInput.position` の物理ピクセル座標
  （12-F の仮想マウス入力と 1:1 対応）
- タスク: 10-E-1 読み戻し / 10-E-2 `FrameCapture` API /
  10-E-3 `capture_game_view()`（テクスチャに `COPY_SRC` 追加）は完了。
  10-E-4 Capture Frame ボタンの PNG 保存も完了。

**Phase 10 の完了基準**:
- Play ボタンで runtime world が生成される
- Game View パネル（またはサブウィンドウ）に runtime scene が描画される
- Stop で edit モードに戻れる
- Play 失敗理由が Diagnostics パネルに表示される

---

## Phase 11: Rendering + Camera Basics

---

### 11-A: 深度バッファ — **実装済み・作業不要**

`render.rs` の pipeline に `depth_stencil: Some(DepthStencilState { depth_write_enabled: true, depth_compare: Less, ... })` が設定済み。  
`app.rs` の `GpuState` が `depth_view` を作成し、リサイズ時に再作成している。  
**Phase 11-A は完了している。**

---

### 11-B: Mesh プリミティブの追加

`crates/engine/src/mesh.rs` に追加:

```rust
impl Mesh {
    pub fn cube() -> Self;                              // 1x1x1 の立方体
    pub fn plane(width: f32, depth: f32) -> Self;       // XZ 平面
    pub fn sphere(rings: u32, sectors: u32) -> Self;    // UV球
}
```

**cube の実装方針**:
- 24頂点 + 36インデックス（面ごとに4頂点、法線が面に対して垂直になるように）
- 6面 × 4頂点 = 24頂点。共有頂点は使わない（法線の向きが違うため）
- インデックス: 各面を2三角形で構成（`[0,1,2, 0,2,3]` × 6面）
- UV: 各面 0..1 のフルUV

**sphere の実装方針**:
- UV球（緯線・経線の交点が頂点）
- `(rings+1) × (sectors+1)` 頂点
- 頂点計算: `theta = PI * ring / rings`, `phi = 2*PI * sector / sectors`
  - `x = sin(theta) * cos(phi)`, `y = cos(theta)`, `z = sin(theta) * sin(phi)`

**注意**: 11-D（ライティング）で `normal` フィールドが追加される。  
プリミティブ追加と normals 追加は**同じ PR** でまとめる方が安全。

---

### 11-C: カメラコンポーネント — **完了 2026-06-11**

`Camera3D` と `camera_aspect_system` は実装済み。`OrbitCamera`、`FollowCamera`、
`orbit_camera_system`、`follow_camera_system` を `camera.rs` に追加済み。

**追加するもの** (`crates/engine/src/camera.rs`):

```rust
// ゲーム用: エンティティを追従
pub struct FollowCamera {
    pub target: engine_ecs::Entity,
    pub offset: Vec3,
    pub spring_strength: f32,  // 0.0 = 即座に追従, 1.0 = 追従しない
}

// ゲーム用: マウスで軌道を描く
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    pub yaw: f32,    // ラジアン
    pub pitch: f32,  // ラジアン
    pub pitch_min: f32,
    pub pitch_max: f32,
    pub orbit_speed: f32,
    pub zoom_speed: f32,
}
```

システム:
- `follow_camera_system`: FollowCamera を持つエンティティの Transform を更新
- `orbit_camera_system`: マウスドラッグで yaw/pitch 更新、スクロールで zoom

---

### 11-D: Basic Lighting — **完了 2026-06-11**（破壊的変更を含む・適用済み）

`Vertex.normal` 追加、`AmbientLight`/`DirectionalLight` リソース、WGSL ライティング、
`DebugRenderState` 実装済み。

**順序制約**: 11-D（`Vertex` への normal 追加）は **14-B（OBJ インポート）
より必ず前**に行う。OBJ ローダが normal 前提で書かれた後では二度手間になる。
Phase 13 を先に出したい場合に 11-D を 13 の後ろへ遅らせるのは可（制約内）。

**ステップ1: `Vertex` に `normal` フィールドを追加**

```rust
// crates/engine/src/mesh.rs
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],   // ← 新規追加
    pub color: [f32; 3],
    pub uv: [f32; 2],
}

impl Vertex {
    pub const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3,  // position
            1 => Float32x3,  // normal     ← 追加
            2 => Float32x3,  // color
            3 => Float32x2,  // uv
        ],
    };
}
```

既存の `Mesh::triangle()` と `Mesh::quad()` の全頂点に `normal` を追加。  
`triangle()` → `normal: [0.0, 0.0, 1.0]` (+Z向き)  
`quad()` → `normal: [0.0, 0.0, 1.0]` (+Z向き)

**ステップ2: ECS リソース追加**

```rust
// crates/engine/src/light.rs (新規)
pub struct AmbientLight {
    pub color: Vec3,
    pub intensity: f32,
}

pub struct DirectionalLight {
    pub direction: Vec3,   // 正規化済み、光の向き（光源から見て）
    pub color: Vec3,
    pub intensity: f32,
}

impl Default for AmbientLight {
    fn default() -> Self { Self { color: Vec3::ONE, intensity: 0.1 } }
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            direction: Vec3::new(-0.5, -1.0, -0.5).normalize(),
            color: Vec3::ONE,
            intensity: 1.0,
        }
    }
}
```

**ステップ3: WGSL シェーダー更新**

```wgsl
// crates/engine/src/shaders/mesh.wgsl
struct LightUniform {
    ambient_color: vec3<f32>,
    ambient_intensity: f32,
    dir_direction: vec3<f32>,
    dir_intensity: f32,
    dir_color: vec3<f32>,
}

@group(3) @binding(0) var<uniform> light: LightUniform;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,   // 追加
    @location(2) color: vec3<f32>,
    @location(3) uv: vec2<f32>,
}

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_normal: vec3<f32>,   // 追加
    @location(1) color: vec3<f32>,
    @location(2) uv: vec2<f32>,
}

@vertex
fn vs_main(v: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let world_pos = object.model * vec4<f32>(v.position, 1.0);
    out.clip_position = camera.view_proj * world_pos;
    // 法線変換: 非一様スケール対応のため逆転置行列が理想だが
    // 最初は model の 3x3 部分でも可
    out.world_normal = normalize((object.model * vec4<f32>(v.normal, 0.0)).xyz);
    out.color = v.color;
    out.uv = v.uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let tex_color = textureSample(t_diffuse, s_diffuse, in.uv) * object.color * vec4<f32>(in.color, 1.0);

    // Phong diffuse
    let n = normalize(in.world_normal);
    let l = normalize(-light.dir_direction);
    let diffuse = max(dot(n, l), 0.0) * light.dir_color * light.dir_intensity;
    let ambient = light.ambient_color * light.ambient_intensity;

    let lit = (ambient + diffuse) * tex_color.rgb;
    return vec4<f32>(lit, tex_color.a);
}
```

**ステップ4: RenderState に light bind group を追加**

`render.rs` の `RenderState::new()` に `LightUniform` の buffer + BGL + BG を追加。  
`render.rs` の `pipeline_layout` に BGL 追加 (group 3)。  
`GpuState::render()` で light リソースを読み取り、`render.update_light()` を呼ぶ。

**まとめ: この変更の波及範囲**:
- `Vertex` 構造体
- `Vertex::LAYOUT`
- `mesh.wgsl`（VertexInput, VertexOutput, vs_main, fs_main）
- `RenderState::new()` パイプラインレイアウト
- `GpuState::render()` ライトデータ更新
- `Mesh::triangle()`, `Mesh::quad()` の全頂点
- テスト内の `Vertex` 直接構築箇所

---

### 11-E: Debug Draw — **完了 2026-06-11**

`DebugLines` リソース（`line`/`aabb`/`axes`/`sphere_wire`）、`DebugRenderState`（LineList
パイプライン）実装済み。毎フレーム後に自動クリアされる。

**目的**: コライダー・トランスフォームの可視化。物理実装（Phase 21-22、旧 17-18）で必須。

```rust
// crates/engine/src/debug_draw.rs (新規)
pub struct DebugLine {
    pub from: Vec3,
    pub to: Vec3,
    pub color: [f32; 4],
}

// ECS リソース: 毎フレーム描画後にクリアされる
pub struct DebugLines {
    pub lines: Vec<DebugLine>,
}

impl DebugLines {
    pub fn line(&mut self, from: Vec3, to: Vec3, color: Vec3);
    pub fn aabb(&mut self, center: Vec3, half_extents: Vec3, color: Vec3);
    pub fn axes(&mut self, transform: &Transform, length: f32);
    pub fn sphere_wire(&mut self, center: Vec3, radius: f32, color: Vec3);
}
```

**GPU 実装**:
- 専用 `wgpu::RenderPipeline`、`PrimitiveTopology::LineList`
- `vertex_buffer`: 動的（毎フレーム `write_buffer`）
- `DebugLines` の内容を毎フレーム頂点バッファに書き込む
- メッシュのレンダーパスの後で描画（深度テストあり or なしは切り替え可能）

**頂点形式（デバッグ専用、軽量）**:
```rust
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DebugVertex {
    position: [f32; 3],
    color: [f32; 4],
}
```

---

## Phase 12: Runtime Foundation — Time / Input / Fixed Update

---

### 12-A: Time — **完了 2026-06-11**

`Time { delta_seconds, elapsed_seconds, frame_count }` は完了。

```rust
pub struct Time {
    pub delta_seconds: f32,
    pub elapsed_seconds: f32,
    pub frame_count: u64,
}
```

---

### 12-B: Fixed Timestep

**実装時期**: Phase 21（collision、旧 17）の先頭タスクとして実装する。Phase 13 の垂直
スライスは可変 dt で成立し、fixed timestep の本来の顧客は物理（21/22、旧 17/18）。
`ecs::App` への公開 API 追加なので、必要になる直前に設計する。

**Fixed Update の問題**: フレームレートが不安定でも物理・ゲームロジックを一定間隔で実行する。

```rust
// crates/engine/src/time.rs
pub struct FixedTime {
    pub fixed_delta: f32,        // デフォルト: 1.0 / 60.0
    pub(crate) accumulator: f32,
}
```

**EcsApp への追加** (`crates/ecs/src/app.rs`):

```rust
impl App {
    pub fn add_fixed_system<P, M>(&mut self, system: impl IntoSystem<P, M>) -> &mut Self;
    pub(crate) fn run_fixed_update(&mut self) -> Result<usize, ...>;
    // 戻り値: 実行したステップ数（デバッグ用）
}
```

**EngineRunner での実行** (`crates/engine/src/app.rs`):

```rust
// RedrawRequested の中で ECS update の前に実行
let mut fixed_time = world.get_resource_mut::<FixedTime>().unwrap();
fixed_time.accumulator += delta;
let fixed_delta = fixed_time.fixed_delta;
while fixed_time.accumulator >= fixed_delta {
    fixed_time.accumulator -= fixed_delta;
    drop(fixed_time);
    self.app.ecs.run_fixed_update()?;
    fixed_time = world.get_resource_mut::<FixedTime>().unwrap();
}
```

**注意**: accumulator が異常に大きくなった場合（長いヒッチ後など）に上限を設ける:
```rust
fixed_time.accumulator = fixed_time.accumulator.min(fixed_delta * 5.0);
```

---

### 12-C: Input — **実装済み**

`Input<KeyCode>`, `Input<MouseButton>`, `MouseInput` はすべて完了。

---

### 12-D: Action Mapping

**Phase 12 では実装不要**。KeyCode 直接参照で十分。  
Phase 34（Project Settings / Input Actions）まで延期（ADR 0028 §Decision 4）。
Phase 34 で binding data model・persistence・editor UX・runtime lookup contract を
ADR で確定してから実装する。

---

### 12-E: Runtime Debug Overlay

```rust
// App に組み込みのデバッグオーバーレイ（egui を使わない軽量版）
pub struct DebugOverlay {
    pub enabled: bool,
    pub show_fps: bool,
    pub show_entity_count: bool,
}
```

egui を runtime に組み込む際（Phase 24 以降、旧 16）に本格実装。  
それまでは `log::info!` で毎秒 FPS をログ出力する程度でよい。

---

### 12-F: Virtual Input Layer（2026-06-11 追加・同日実装済み・ADR 0026 Accepted）

AI / Replay / Test が engine 内の仮想入力として keyboard / mouse /
（将来）gamepad を注入できるようにする。OS レベルの入力合成は恒久禁止。
詳細は `docs/phases/phase-12-runtime-foundation.md` §12-F。

```rust
#[non_exhaustive]
pub enum InputSource { Human, AiAgent, Replay, Test }

#[non_exhaustive]
pub enum InputCommand {
    Key { key: KeyCode, pressed: bool },
    MouseButton { button: MouseButton, pressed: bool },
    MouseMove { position: (f32, f32) },
    MouseDelta { delta: (f64, f64) },
    MouseScroll { amount: f32 },
    GamepadButton { gamepad: GamepadId, button: GamepadButton, pressed: bool },
    GamepadAxis { gamepad: GamepadId, axis: GamepadAxis, value: f32 },
}
```

- `VirtualInputQueue` リソースに push し、「`clear_transitions()` 後・
  schedule 前」の固定タイミングで既存の `Input<KeyCode>` /
  `Input<MouseButton>` / `MouseInput` へ drain（winit イベントと同じ
  `just_pressed` セマンティクス）
- gamepad は `GamepadId` / `GamepadButton` / `GamepadAxis` の**型定義のみ**。
  `gilrs` 等の実デバイス対応・`Input<GamepadButton>` リソースは将来
- winit の人間入力も将来 `InputSource::Human` の `InputCommand` に一本化する
  （`EngineRunner` 内部リファクタ・公開 API 影響なし）
- タスク: 12-F-1〜12-F-7（各 30 分〜1 時間、phase doc 参照）。
  GPU 不要のため Phase 11 と並行着手可。**Phase 13 の前に終えると
  PlayerController を入力注入で自動テストできる**
- Replay 記録・再生のファイル形式は別 ADR を書くまで凍結しない

---

## Phase 13: Minimal Playable Vertical Slice

---

### 13-A: Player Controller — **完了 2026-06-11**

`PlayerController`、`MovePlane`、`PlayerMarker`、`player_controller_system` 実装済み。
`minimal_playable.rs` も更新済み。

既存の `minimal_playable.rs` の `player_move_system` をエンジンの組み込みシステムに昇格。

```rust
// crates/engine/src/player.rs
pub struct PlayerController {
    pub move_speed: f32,
    pub move_plane: MovePlane,   // XZ (3D), XY (2D)
}

pub enum MovePlane { XZ, XY }

pub fn player_controller_system(
    keyboard: Res<Input<KeyCode>>,
    time: Res<Time>,
    mut query: Query<(&PlayerController, &mut Transform), With<PlayerMarker>>,
) { ... }
```

`App::new()` では自動登録しない（ユーザーが `app.add_system(player_controller_system)` する）。

---

### 13-B: Camera Controllers — **完了 2026-06-11**（11-C と統合）

`OrbitCamera`/`FollowCamera` と各システムは 11-C と統合して実装済み。

```rust
// crates/engine/src/camera.rs への追加

pub fn orbit_camera_system(
    mouse: Res<MouseInput>,
    mouse_buttons: Res<Input<MouseButton>>,
    mut query: Query<(&mut OrbitCamera, &mut Transform)>,
) { ... }

pub fn follow_camera_system(
    time: Res<Time>,
    targets: Query<&GlobalTransform>,
    mut cameras: Query<(&FollowCamera, &mut Transform)>,
) { ... }
```

---

### 13-C: Editor-authored Playable Scene — **完了 2026-06-11**

Phase 9-10-13-A/B が完了していれば、追加実装なし。  
検証: GUI でシーンを作り、Play → WASD で操作 → Stop の一連の流れが動くことを確認。

---

### 13-D: Behavior Tree を Play モードに接続 — **完了 2026-06-11**

`RuntimePlayState::start()` で `register_behavior_tree_system` を呼び出す実装済み。
失敗時は `PlayError::SystemRegistration` で診断を出す。

Play 開始時のセットアップに追加:

```rust
// BehaviorTreeRunner コンポーネントを持つエンティティが存在する場合
register_behavior_tree_system(&mut world);
```

または `spawn_from_authoring_scene` を拡張して、シーンに紐付けられたグラフから  
`BehaviorTreeRunner` を自動的にスポーンする（Phase 14 以降の課題）。

---

## Phase 14: Asset Import / Asset Pipeline

---

### 14-A: AssetServer 拡張 — **完了 2026-06-11**

`AssetServer` に `mesh_cache`/`texture_cache`（`AssetId` キー）と
`with_assets_root()` ビルダーを追加済み。キャッシュアクセサ
（`cached_mesh`/`cached_texture`/`cache_mesh`/`cache_texture`）も追加。

**旧案のテクスチャ非同期ロードの状況**: `material.rs` に `Texture::from_png_bytes()` があるが、ディスクからのロードはない。

```rust
// crates/engine/src/asset.rs への追加
impl AssetServer {
    pub fn load_texture(
        &mut self,
        path: &str,           // "textures/player.png"
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Handle<Arc<Texture>>, AssetLoadError>;
}
```

- パスは `ProjectRoot::resolve_asset()` で assets_root 内に限定
- キャッシュ: `HashMap<AssetId, Handle<Arc<Texture>>>` を `AssetServer` に追加
  （ADR 0021: キーはパス文字列ではなく `AssetId`。Windows の大文字小文字・
  区切り文字の罠を回避）
- 同じ `AssetId` を2回ロードしてもテクスチャを共有（参照カウント）

**AssetServer に assets_root を持たせる**:

```rust
pub struct AssetServer {
    assets_root: Option<PathBuf>,    // 追加
    // ...
}

impl AssetServer {
    pub fn with_assets_root(path: PathBuf) -> Self;
}
```

---

### 14-B: OBJ メッシュロード — **完了 2026-06-11**

`tobj = "4"` 追加、`load_obj` ヘルパー実装済み。`AssetServer::load_mesh` が `.obj`
を解析する。ノーマルなし→ +Y フォールバック、UV Y 軸反転（wgpu 座標系）。
`AssetLoadError::MeshParse` 追加。

依存クレートの追加:
```toml
# crates/engine/Cargo.toml
[dependencies]
tobj = { version = "4", features = ["async"] }
```

```rust
impl AssetServer {
    pub fn load_mesh(
        &mut self,
        path: &str,    // "meshes/cube.obj"
    ) -> Result<Handle<Mesh>, AssetLoadError>;
}
```

OBJ ロード処理:
1. `tobj::load_obj()` でモデルと材質を読む
2. 各三角形の頂点に position/normal/uv を設定
3. OBJ の法線がなければ平均法線を計算（face normal で代用）
4. `Mesh { vertices, indices: Some(indices) }` を返す

**glTF の場合** (将来): `gltf` クレートを追加。OBJ より情報量が多くメッシュ・マテリアル・スケルトンも取れる。Phase 14 では OBJ のみで十分。

---

### 14-C: Asset Manifest Resolution（ADR 0021）— **完了 2026-06-11**

`AssetManifest`/`ManifestEntry`/`AssetManifestError` 実装済み。`spawn_from_authoring_scene`
が world から `AssetManifest` と `AssetServer` を読み取り、非ビルトイン `asset_ref` を
manifest 経由で解決する。ビルトイン ID は manifest なしでも解決継続。

**旧案（`"$type": "asset_path"` のパス参照）は破棄**。パスは安定識別子では
なく、ファイルのリネームで全シーンの参照が壊れるため（spec §7.4 /
ADR 0004 違反）。シーンは従来どおり `asset_ref`（`AssetId`）のみで参照する。

プロジェクト直下の `asset_manifest.json` が `AssetId` → 相対パスを持つ:

```json
{
  "schema_version": 1,
  "assets": {
    "asset_01JZ...": { "path": "meshes/player.obj", "name": "player_mesh" },
    "asset_01K0...": { "path": "textures/player.png", "name": "player_texture" }
  }
}
```

- `assets` は `BTreeMap<AssetId, ManifestEntry>`（決定的シリアライズ）。
- `path` は assets root 相対。`ProjectRoot::resolve_asset()` で検証する。
- ファイルのリネーム/移動はマニフェスト 1 行の修正で完結し、シーンは無傷。

解決フロー: `asset_ref` → マニフェスト → パス → `AssetServer` ロード →
`RuntimeAssetId`。セッション中の `AssetId → RuntimeAssetId` 対応
（spec §5.2）は従来どおり Play 終了時に破棄する。

`scene_bridge.rs` の拡張:
- マニフェスト経由の解決を追加。ビルトイン ID（`BUILTIN_*`）は
  マニフェストなしで解決できるフォールバックとして維持（後方互換）。
- マニフェストがビルトイン ID を再定義した場合は `asset.builtin_conflict`
  診断。
- 整合性診断: `asset.missing_file`（エントリあり・ファイルなし、Error）、
  `asset.unregistered_file`（ファイルあり・エントリなし、Warning）。

シグネチャ変更案:
```rust
pub fn spawn_from_authoring_scene(
    world: &mut World,
    scene: &AuthoringScene,
) -> Result<AuthoringToRuntimeMap, SceneBridgeError>
// AssetServer とマニフェストは world のリソースから取得。
// なければビルトインのみ対応（後方互換）。
```

---

### 14-D: Missing Asset Diagnostics — **完了 2026-06-11**

診断コード:
- `asset.unregistered_file` (Warning): manifest に未登録の AssetId → フォールバックメッシュ（triangle）でゲーム続行
- `asset.missing_file` (Error): manifest エントリはあるがファイル読み込み失敗 → フォールバックでゲーム続行
- `asset.builtin_conflict` (Warning): manifest がビルトイン ID を再定義 → ビルトインが優先

`AuthoringToRuntimeMap.asset_diagnostics` に格納され `PlayStart.diagnostics` に転送される。
テクスチャの manifest 解決は GPU 依存のため後送り（新 Phase 16 でも対象外・時期未定。docs に記録済み）。
チェッカーボードのピンクエラーメッシュは未実装（フォールバックは `Mesh::triangle()` で代替）。

---

## 旧→新 Phase 対応表（2026-06-13 再構成）

Phase 14 完了時点の見直しで、Phase 15〜20 を「最低限エディタから使える
エンジンにする」方針で再構成した。旧 15〜19 の runtime 機能は Phase 21
以降へ後ろ倒しする（理由: 12-B Fixed Timestep 未実装、エディタに collider
編集手段がない状態で collision に進むと検証がコード直書きに逆戻りする）。

| 旧 Phase | 新 Phase | 備考 |
|---------|---------|------|
| 15 Scene Management | **18**（縮小） | SceneLoader / Reload のみ。DontDestroyOnLoad / transition サンプルは Phase 21 以降 |
| 16 Runtime UI / HUD | **24** | |
| 17 Collision | **21** | 12-B Fixed Timestep を 21 の先頭タスクとして帯同 |
| 18 Simple Physics | **22** | |
| 19 Audio | **23** | |
| 20 Sample Game（フル版） | **25** | 新 20 は minimal sample project。コイン・敵 AI・HUD・音つきのフル版は 25 |
| 21〜26 Advanced Authoring | **26 以降** | ADR 0026 の AI Agent Bridge は番号でなく名前で参照する |

新 15〜20 のゴール: エディタで project を開き、scene を編集し、外部 OBJ を
Mesh component に割り当て、Play で Game View に表示し、保存して開き直せる。
後ろ倒しした機能の詳細は各 phase ドキュメント（rename 済み）が正規ソース。

---

## Phase 15: Component Definition / Registry / Inspector 基盤

**前提**: ADR 0027（ComponentDefinition / ComponentRegistry の境界）の Accept。

**現状の問題**: component を 1 つ足すと、engine の struct / scene_bridge の
spawn 分岐 / schema 登録（authoring または editor 側）/ inspector 特殊扱い
（`builtin_asset_choices` 等）の最低 4 箇所に手が要る。実際 13-A の
`PlayerController` / `OrbitCamera` / `FollowCamera` は registry 未登録で
エディタから追加できない。Camera / Light は authorable な component type
自体が存在せず、Play 時は default camera を仮挿入しているだけ。

### 15-A: ADR 0027 の Accept

ComponentRegistry は engine 所有（spawn 関数が engine 型を要するため）。
authoring の `ComponentSchemaRegistry` は schema 専用契約として温存し、
CLI / MCP は変更なし。詳細は
`docs/adr/0027-component-definition-registry.md`。

### 15-B: ComponentRegistry 実装と既存 4 component の移行

- `engine::components::builtin_registry()`（モジュールパスは実装時決定）に
  `ComponentDefinition { schema, spawn, inspector }` を登録する
- 既存 4 component（transform / player_marker / mesh / material）を
  **挙動を変えずに**移行する
- `scene_bridge` の spawn 分岐と editor の `addable_component_registry()` /
  `builtin_asset_choices()` を registry 由来に置換する
- ゲート: 既存の bridge / editor テストが全部通ること（機能追加ゼロ）

### 15-C: Camera / Light / PlayerController の authorable 化

- `engine.camera`（Camera3D 相当）、`engine.directional_light` + ambient、
  `engine.player_controller` を registry に追加
- Light は現在 world resource。entity component として author し、system で
  resource へミラーする。複数 directional light は最初の 1 個を採用 + 診断
- scene に camera があるときは Play の default camera 挿入を抑止する
- 新 component type の追加は scene format に対して additive。旧エディタで
  開いたときの未知 component の扱い（保持されること）を事前に確認する

### 15-D: PropertyPath / SetProperty（ADR 0025 の割当どおり）

- spec 既定義の `PropertyPath` 仕様に従う（独自構文を発明しない）
- `AuthoringCommand::SetProperty` の apply / inverse / 診断
  （存在しない path・型不一致）をテストで固める
- Inspector を field 単位編集に切り替える。`SetComponentValue` は
  丸ごと置換用として共存する

### 15-E: undo coalescing（ドラッグ 1 回 = undo 1 ステップ）

- ドラッグ中は preview 値を UI ローカルに保持し、release
  （`drag_stopped()`）で 1 transaction をコミットする
- AuthoringSession のクローン式 undo（ADR 0005）が 1 コミット 1 クローン
  であるため、ドラッグ毎フレームコミットのコスト問題も同時に解決する

**Phase 15 の完了基準**:
- 新規 component の追加 = registry 登録 1 箇所 + テストで、Add Component /
  Inspector / Play に反映される
- Camera / Light / PlayerController をエディタから追加・編集できる
- 数値ドラッグ 1 回が undo 1 ステップになる
- `SetProperty` の apply / inverse / 診断テストが通過する

---

## Phase 16: Asset / OBJ / Render の editor 統合

**現状の問題**: `RuntimePlayState::start(scene)` は scene しか受け取らず、
`AssetServer` / `AssetManifest` を world に挿入しないため、14-C の manifest
解決は editor Play から到達不能。Phase 14 の完了基準のうち
「manifest 登録した AssetId が Game View に表示される」は editor 経由では
未達のまま。Inspector の asset picker もビルトイン 4 種のハードコード
（phase-14 doc が「14-C 実装時の置き換え対象」と予告済み）。

### 16-A: editor での manifest 読み込み

- Project open 時に `asset_manifest.json` を読み込み・保持（なければ空
  manifest として扱う）
- 保存は `engine_authoring::persist::replace_file_contents` で atomic に
  行う（自前の `fs::write` + rename は実装しない）

### 16-B: Register Asset 操作

- Asset Browser で `.obj` を選んで `AssetId`（ULID）を発行し manifest に追記
- 同一ファイルの二重登録防止、`name` slug の重複規則を決める
- 未登録ファイル（`asset.unregistered_file`）を editor 側でも可視化する

### 16-C: Inspector の Mesh picker を manifest ベースに

- 選択肢 = built-in（triangle / quad）+ manifest 登録済み mesh
- built-in は後方互換として残す（ADR 0021 のビルトインフォールバック維持）

### 16-D: RuntimePlayState への ProjectRoot / AssetServer / AssetManifest 接続

- `RuntimePlayState::start` に `ProjectRoot` を渡し、
  `AssetServer::with_assets_root` + `AssetManifest` を world resource として
  挿入してから `spawn_from_authoring_scene` を呼ぶ
- **破壊的変更プロトコル対象**（engine + editor、同一 PR）
- project 未オープンで scene だけ開いている場合は built-in のみで
  Play 続行 + 診断

### 16-E: 統合テスト

- temp project + 実 OBJ ファイルで「manifest 登録 → scene 参照 → Play →
  Game View に表示」を検証する
- ファイル欠損時は fallback + `asset.missing_file` 診断でゲーム続行

**スコープ外**: テクスチャの manifest 解決（GPU の device/queue 依存のため
引き続き後送り。Mesh のみで「外部アセットが見える」を成立させる）。

**Phase 16 の完了基準**:
- manifest 登録済み OBJ を Inspector で選び、Play で Game View に表示される
- built-in triangle / quad は従来どおり動く
- 欠損時は fallback + 診断で続行する。統合テストあり

---

## Phase 17: Scene Editing / Preview の実用化

### 17-A: Camera / Light / Material 編集の安定化

scene 上の `engine.camera` / `engine.directional_light` が Play に実際に
効くことの検証と齟齬解消（camera があるのに default camera が挿入される等）。

### 17-B: Hierarchy 実用化

- entity rename
- 子持ち entity の削除方針確定（9-0 で保留した cascade 削除の決着）
- 選択状態と Inspector / Game View の同期

### 17-C: Game View 安定化

- リサイズ・aspect・再生中 panel 操作で破綻しない
- 選択 entity の確認導線（最低限: debug axes 表示 or 選択 entity 情報表示）

### 17-D: フルループの回帰チェックリスト化

「編集 → Play → Stop → 編集 → 保存 → 再起動 → 開き直し」を手順書化し、
リリース判定に使う回帰チェックリストにする。

**Phase 17 の完了基準**:
- フル編集ループのチェックリスト全項目グリーン
- camera / light の編集結果が Play に正しく反映される

---

## Phase 18: ProjectRoot ベースの Runtime Scene Loading / Reload

旧 Phase 15 の縮小移設。複雑な scene transition より「project 内 scene を
確実に load / reload / Play できる」ことを優先する。
詳細は `docs/phases/phase-18-runtime-scene-loading.md`。

### 18-A: SceneLoader（旧 15-A）

```rust
// crates/engine/src/scene_loader.rs (新規)
pub struct SceneLoader { /* ProjectRoot 経由のパス解決 */ }

impl SceneLoader {
    pub fn load(&self, relative_path: &str) -> Result<AuthoringScene, SceneLoadError>;
}

pub enum SceneLoadError {
    Io(std::io::Error),
    JsonParse(String),
    Validation(Vec<Diagnostic>),
}
```

`engine-authoring` の `load_scene_from_json()` + `fs::read_to_string()` の
ラッパー。パスは `ProjectRoot` 経由で解決する。

### 18-B: editor からの Reload

ファイル上の scene を Play 中に確実に再ロードできる（まず「同一 scene の
reload」のみ）。失敗は診断に出してクラッシュしない。

### 18-C:（任意・最小）単純な scene 切替

SceneManager の pending_load 方式（ECS システムは pending を立てるだけ、
実切替は `App` のメインループで行う）。**`DontDestroyOnLoad`（旧 15-C）と
transition サンプル（旧 15-D）は実装しない**（Phase 21 以降、実需が出た
時点で導入する）。

**Phase 18 の完了基準**:
- editor で開いている project の scene を Play 中に reload できる
- 失敗が診断に出る（クラッシュしない）

---

## Phase 19: Minimal Runtime Features

新規 runtime 機能は原則追加しない「安定化 Phase」。
**Collision / Physics / Audio は着手禁止**（Phase 21〜23）。

### 19-A: PlayerController の editor 導線

editor で追加・編集し、Play の Game View 内で WASD 移動が動く
（Game View focus 時の入力ルーティング確認）。

### 19-B: Camera controllers

`OrbitCamera` / `FollowCamera` を authorable にし、sample project で
使える程度に安定化する。

### 19-C: BehaviorTree 接続の editor 検証

13-D の実装（`register_behavior_tree_system`）を editor scene からの
導線で検証する。

### 19-D: debug draw / time / input の確認

debug draw の Play 中トグル、`Time` / input の安定確認。

**Phase 19 の完了基準**:
- sample project 規模で PlayerController / カメラ / BT が editor 経由で動く
- 新規 runtime 機能ゼロでも完了と認める

---

## Phase 20: Minimal Sample Project

**目的**: 「エディタから普通に使えるエンジン」を 1 つの最小プロジェクトで
実証する。コイン・敵 AI・HUD・音つきのフル版 sample game は Phase 25。

### 20-A: sample project データ

```
examples/sample_project/
  project.json
  asset_manifest.json
  assets/
    scenes/main.scene.json
    meshes/        # OBJ 1〜2 個
```

コードではなく**プロジェクトデータ**。editor で開いて使う。

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

### 20-C: 自動スモークテスト

12-F の `InputCommand` 注入 + 10-E の `FrameCapture` で「Play → 仮想入力 →
キャプチャが背景クリア色以外を含む」程度の統合テストを
`cargo test --workspace` に乗せる（golden image までは要求しない）。

**Phase 20 の完了基準**:
- 20-B のチェックリストが全項目通る
- 20-C のスモークテストが CI で通る

---

## Phase 21+: 後ろ倒しした Runtime 機能と Advanced Authoring

2026-06-13 の再構成で旧 15〜19 の runtime 機能をここへ移した。
各 phase の実装詳細は rename 済みの phase ドキュメントが正規ソース。

| Phase | 内容 | 出典 / 備考 |
|-------|------|------------|
| 21 | Collision Detection（先頭タスクとして 12-B Fixed Timestep を実装） | 旧 17。`docs/phases/phase-21-collision.md`。`DontDestroyOnLoad` / scene transition サンプル（旧 15-C/D）の要否もここで再判断 |
| 22 | Simple Physics | 旧 18。`docs/phases/phase-22-physics.md` |
| 23 | Audio | 旧 19。`docs/phases/phase-23-audio.md`。rodio 依存はここで追加 |
| 24 | Runtime UI / HUD | 旧 16。`docs/phases/phase-24-runtime-ui.md`。egui / egui-winit / egui-wgpu 依存はここで追加 |
| 25 | Full Sample Game（コイン・敵 AI・HUD・SE/BGM つき） | 旧 20 の内容。新 20 の minimal sample project を拡張する |

### Phase 26–40: Advanced Authoring（2026-06-13 再構成・ADR 0028 Accepted）

Phase 25 完了後のロードマップを具体番号で整理した（ADR 0028）。
Phase 17 は廃止せず Phase 27〜29 の前提として維持する。
shared contract・serialized format・CLI/MCP 境界に触れる Phase は着手前に個別 ADR を作成する。

| Phase | 内容 | 備考 |
|-------|------|------|
| 26 | Project Hub / Scene-first Startup | ProjectRoot は ADR 0023 の既存モデル。New / Open / Recent の UX を追加 |
| 27 | Edit Mode Scene View / Editor Camera | Edit Mode で authoring scene を描画。Scene View / Game View を UI 上で分離 |
| 28 | Scene Picking / Selection / Multi-select | Scene View クリックで EntityId 選択。Hierarchy / Inspector と同期 |
| 29 | Transform Gizmo / Duplicate / Copy Paste | Move / Rotate / Scale gizmo。操作 1 回が undo 1 step |
| 30 | Console / Problems / Validation | structured diagnostics を severity / code / target で表示 |
| 31 | Asset Database v2 / Import Settings | manifest 拡張または `.meta` 導入。着手前に ADR 必須 |
| 32 | Drag & Drop Authoring UX | Asset Browser から Scene / Hierarchy / Inspector へ drag & drop |
| 33 | Prefab / Reusable Entity Template | `.prefab.json` v1。instantiate + EntityId remap。着手前に ADR 必須 |
| 34 | Project Settings / Input Actions | Phase 12-D の延期分（ADR 0028 §4）。Tags / Layers / Input Actions / Start Scene。着手前に ADR 必須 |
| 35 | Material / Lighting / Environment Editor | material asset、texture / environment settings の editor 対応 |
| 36 | glTF / GLB Asset Pipeline | static mesh / material / texture import。animation は Phase 37 |
| 37 | Animation Runtime / Clip Import | clip asset、sampler、Animator component、glTF animation import |
| 38 | Animation Authoring / Animation Graph | graph foundation を使う Animation Graph domain（BT と共有モデル重複禁止） |
| 39 | Build / Packaging / Distribution | Start Scene と Asset Database から runnable desktop package を生成。着手前に ADR 必須 |
| 40 | AI Agent Bridge | 10-E FrameCapture と 12-F VirtualInput を CLI / MCP tool として外部へ公開（ADR 0026）。PNG encode / prompt / AI API は engine 外。着手前に ADR 必須 |

各 Phase の実装詳細は `docs/phases/phase-N-*.md` が正規ソース。

### Roadmap consolidation prerequisite（番号なし）

旧 Phase 41: Consolidation の内容は実施対象として残すが、正式な番号付き Phase としては
扱わない。Phase 40 完了後、新 Phase 41 着手前に必ず実施する prerequisite とする。

実施内容:

- Phase 15-20 のギャップ監査
- 未到達事項の小さな実装（必要な場合のみ）
- docs 整合
- `docs/AGENTS.md` と `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` 系の整合
- 旧 ADR の status 更新
- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo doc --workspace --no-deps`

この prerequisite では Shadow / Scripting / Gamepad / NavMesh など新 Phase 41 以降の
本実装に入らない。

### Phase 41-47: Runtime / Platform roadmap（2026-06-14 renumber）

旧 Phase 41: Consolidation を番号なし prerequisite に移したため、以後の正式 Phase 番号を
1 つ前に詰める。

| 新 Phase | 旧 Phase | 内容 | ADR / 備考 |
|----------|----------|------|------------|
| Phase 41 | Phase 42 | Shadow Mapping & Environment Lighting | ADR 0036 必須。`docs/phases/phase-41-shadow-ibl.md` |
| Phase 42 | Phase 43 | Scripting System | ADR 0037 Accepted。Rhai via `rhai`。MonoBehaviour 的 `ScriptComponent`、diagnostics / profiler 方針。`docs/phases/phase-42-scripting.md` |
| Phase 43 | Phase 44 | Gamepad / Controller Input | ADR 0038 必須。`docs/phases/phase-43-gamepad.md` |
| Phase 44 | Phase 45 | Navigation Mesh & Pathfinding | ADR 0039 必須。`docs/phases/phase-44-navmesh.md` |
| Phase 45 | Phase 46 | Post-Processing Pipeline | ADR 0040 必須。`docs/phases/phase-45-postprocess.md` |
| Phase 46 | Phase 47 | WASM / Web Build Target | ADR 0041 必須。`docs/phases/phase-46-wasm.md` |
| Phase 47 | Phase 48 | GPU Instancing & LOD | `docs/phases/phase-47-instancing-lod.md` |

### Phase 48+: 機能ロードマップ（2026-07-04 追加）

Phase 41〜47 完了後の新規 Phase。旧番号は存在しない。

| Phase | 内容 | ADR / 備考 |
|-------|------|------------|
| Phase 48 | Skinned Mesh & Skeletal Animation | ADR 0043 Accepted・実装完了（2026-07-04）。`docs/phases/phase-48-skinned-mesh.md` |
| Phase 49 | Particle System | ADR 0044 Accepted・実装完了（2026-07-05）。`docs/phases/phase-49-particles.md`。描画は Phase 47 instancing を再利用 |
| Phase 50 | Directional Shadow Pass | ADR 0036 の GPU 実装（新 ADR 不要）・実装完了（2026-07-05）。`docs/phases/phase-50-shadow-pass.md` |
| Phase 51 | Packaging End-to-End | ADR 0045 Accepted・実装完了（2026-07-05）。`docs/phases/phase-51-packaging.md`。player バイナリ + package_project |
| Phase 52 | Particle Emitter Authoring Integration | 新 ADR 不要（ADR 0027 + 0044 に従う）。`docs/phases/phase-52-particle-authoring.md`。`engine.particle_emitter` を component registry に追加 |

### M1 マイルストーン: アクション RPG 制作可能ライン（2026-07-11 策定）

**目標**: 「妖怪ウォッチバスターズ」級の 3D アクション RPG
（小規模アリーナ・リアルタイム近接戦闘・ロックオン・AI 僚機・
ミッションループ・HUD/メニュー UI・セーブ）を、**エンジン本体を改造せずに**
データ + Rhai スクリプト + examples レベルの Rust で作れる状態。

**受け入れ基準**: Phase 62 の vertical slice `busters_lite` が成立すること。

**ギャップ分析（2026-07-11 実測）**:

| 領域 | 現状 | M1 に足りないもの |
|------|------|------------------|
| 描画・アニメ・パーティクル・影 | Phase 35〜52 で実装済み | アニメのクロスフェード・アニメイベント・Animation Graph の実行時評価 |
| 衝突・物理 | AABB collider + push-out + CollisionEvents（Phase 21/22） | カプセル形状・kinematic character controller・collision layers の runtime 適用・trigger volume |
| UI | egui 即時モード（Rust コード専用・Phase 24） | 宣言的 UI ドキュメント（データ駆動・エディタ/スクリプトから利用可能）・CJK フォント |
| ゲームフロー | SceneLoader/reload（Phase 18。切替 = 18-C 未実装） | 実行時シーン切替・シーン横断の永続状態・セーブ/ロード |
| スクリプト | Rhai + component get/set + input snapshot（Phase 42） | spawn/despawn・アニメ/オーディオ/UI 制御・タイマー・イベント購読の拡張 |
| オーディオ | SE + 単一 BGM + master volume（Phase 23） | バス音量・BGM クロスフェード・（任意）距離減衰 |
| カメラ | Orbit/Follow（Phase 13/19） | ロックオンカメラ・壁衝突回避 |
| AI | BT + grid A* NavMesh（Phase 44） | （M1 では既存で足りる想定） |

**M1 非目標（明示的にやらない）**: ネットワーク／ローカルマルチプレイ
（バスターズの 4 人マルチはシングル + AI 僚機で代替）・コンソール移植・
IK・カットシーンエディタ・ローカライズ基盤・外部物理エンジン統合
（rapier 等）・WASM ランタイム完成。

### Phase 53-63: M1 ロードマップ

| Phase | 内容 | ADR / 備考 |
|-------|------|------------|
| Phase 53 | Declarative UI Foundation | **ADR 0046 必須**（authoring 新ドメイン + engine 依存方向）。UI document データモデル（`*.ui.json`・Panel/Text/Image/Button のツリー + アンカー/レイアウト）+ engine 側 egui インタープリタ + `$bind` 読み取りバインディング + CJK フォント読み込み。VDOM/reconciler は作らない（egui 即時モードの上にデータ解釈層のみ） |
| Phase 54 | UI Interactivity & Authoring Integration | UI イベント（`on_click` 等）を**データ**として Rhai / BT へ配線。`engine.ui_document` を component registry へ追加（Phase 52 パターン）。UI アセットのホットリロード |
| Phase 55 | Scene Management & Game Flow | **ADR 必須**。SceneManager（実行時シーン切替 = 18-C の決着）・シーン横断の永続リソース・切替時のアセット解放/保持規則・ロード画面フック |
| Phase 56 | Save / Load | **ADR 必須**（セーブフォーマットは安定フォーマット扱い・バージョン付き）。セーブスロット・Rhai からの読み書き API・パッケージ実行時の保存先解決 |
| Phase 57 | Action Collision Toolkit | カプセル/スフィアコライダー・kinematic character controller（斜面/段差/押し出し）・collision layers/masks の runtime 適用（Phase 34 settings 連携）・trigger volume・CollisionEvent の script/BT 配線 |
| Phase 58 | Targeting & Action Camera | ロックオン対象レジストリ（faction/範囲/遮蔽）・ロックオンカメラ・カメラの壁衝突回避（spring arm） |
| Phase 59 | Animation Blending & Events | クリップ間クロスフェード・アニメーションイベント（フレーム発火 → script/BT。攻撃判定フレーム用）・Phase 38 Animation Graph の実行時評価接続 |
| Phase 60 | Script API v2 | ScriptContext 拡張: prefab spawn/despawn・エンティティ検索・アニメ/オーディオ/UI/カメラ制御・タイマー・イベント購読。max operation 安全規則は ADR 0037 を維持 |
| Phase 61 | Audio v2 | 実装完了（2026-07-12）。BGM/SE バス別音量・BGM ループ/クロスフェード。任意項目の距離減衰 SE は未実装。`docs/phases/phase-61-audio-v2.md` |
| Phase 62 | Vertical Slice: busters_lite | **実装完了（2026-07-12）**。`examples/busters_lite` + `crates/engine/examples/busters_lite.rs`。アリーナ 1 面のミッションループ（タイトル → 出撃 → 戦闘 → リザルト → セーブ）、プレイヤー + AI 僚機 2 + 敵グループ、ロックオン近接コンボ、HUD/ポーズ。エンジン本体非改造。`docs/phases/phase-62-busters-lite.md` |
| Phase 63 | M1 Consolidation | **実装完了（2026-07-12）**。vertical slice 更新時間メトリクス、エディタ Package ボタン配線、統合手動チェックリスト `docs/M1_ACCEPTANCE_CHECKLIST.md`、docs/ADR 整合、4 ゲート。`docs/phases/phase-63-m1-consolidation.md` |

実装順は表の順を基本とするが、UI トラック（53-54）とアクション基盤トラック
（57-59）は独立しており並行・入替可。Phase 60 は 54/57/59 のイベント供給元に
依存するためそれらの後。Phase 62 が M1 の受け入れゲート。

---

## クレート依存関係の変更サマリー

```
現在の依存グラフ（Cargo.toml 実測）:
  renderer → wgpu/winit のみ（ecs に依存しない・ADR 0003）
  engine   → ecs + renderer + authoring
  cli / mcp / editor → authoring のみ（editor は eframe / rfd / serde_json も保持）

Phase 10-B 以降:
  editor → authoring + engine（マージは Track W 完了後を推奨・ADR 0024）

追加クレート:
  editor/Cargo.toml: + engine（rfd は Phase 8-C で導入済み）
  engine/Cargo.toml: + tobj (Phase 14 で導入済み), + rodio (Phase 23),
                     + egui/egui-winit/egui-wgpu (Phase 24)
  新 Phase 15〜20 は新規クレート・新規第三者依存を追加しない
  Phase 42: + rhai（ADR 0037 に従い Rhai runtime wrapper として導入）
  Phase 43: + gilrs（ADR 0038 に従い desktop target のみに導入）
```

---

### Phase 42 subphase breakdown

- 42-A: ADR 0037 / Rhai scripting runtime design
- 42-B: ScriptAsset / ScriptEngine / AST cache
- 42-C: ScriptComponent lifecycle hooks
- 42-D: ScriptContext ECS facade
- 42-E: Script diagnostics / profiler / max operation safety
- 42-F: Editor integration / Console errors / hot reload
- 42-G: Rust promotion policy documentation

---

## 実装順序と依存関係

```
9-0 (契約整備: ADR 0020-0024 / scene version / DeleteEntity)
   │                              ┌─ Track W: wgpu 29 統一（並行・TB 2週間）
   ▼                              │   撤退案 = 別プロセス Player (ADR 0024)
9-A (ProjectRoot) → 9-B (AssetBrowser) → 9-C (Open) → 9-D (Save) → 9-E (Hierarchy+Inspector)
   ▼                              │
10-A/B (ロジック Play; Track W 不要・catch_unwind。10-B のマージは Track W 後)
   ▼                              │
10-C (GameView) ◄─────────────────┘（Track W 完了がゲート）→ 10-D (Diagnostics)
   ▼
11-B+11-D (Primitives + Vertex normal + Lighting; 同一 PR) → 11-C → 11-E (DebugDraw)
   ▼
13-A (PlayerController) → 13-B (CameraController) → 13-C (Playable Scene) → 13-D (BT 接続)
   ▼
14-A (Texture Load) → 14-B (Mesh Load; 11-D 完了後) → 14-C (Manifest 解決) → 14-D
   ▼
15-A (ADR 0027 Accept) → 15-B (Registry 移行; 挙動不変) → 15-C (Camera/Light/Controller)
   │                                                          │
   └→ 15-D (SetProperty) → 15-E (undo coalescing)             │ ※ 15-D/E と 15-C は並行可
   ▼
16-A (manifest 読込) → 16-B (Register Asset) → 16-C (Mesh picker)
   │
   └→ 16-D (RuntimePlayState 接続; 破壊的変更) → 16-E (統合テスト)
   ▼
17-A (Camera/Light 安定化; 15-C 依存) → 17-B (Hierarchy) → 17-C (GameView) → 17-D (回帰チェックリスト)
   ▼
18-A (SceneLoader) → 18-B (Reload) → 18-C (任意: 単純切替)
   ▼
19-A (PlayerController 導線) → 19-B (Camera controllers) → 19-C (BT 検証) → 19-D (debug draw/time/input)
   ▼
20-A (sample project データ) → 20-B (受け入れチェックリスト) → 20-C (自動スモークテスト)
   ▼
21 (12-B FixedUpdate + Collision) → 22 (Physics) → 23 (Audio) → 24 (Runtime UI) → 25 (Full Sample Game)
   ▼
26 → 27 → 28 → 29 → 30 → 31 → 32 → 33 → 34 → 35 → 36 → 37 → 38 → 39 → 40
   ▼
Roadmap consolidation prerequisite（番号なし）
   ▼
41 (Shadow/IBL) → 42 (Rhai Scripting) → 43 (Gamepad) → 44 (NavMesh) → 45 (Post-Processing)
   → 46 (WASM/Web) → 47 (GPU Instancing & LOD)
   ▼
48 (Skinned Mesh & Skeletal Animation; ADR 0043 + 親子 Transform 伝播が前提)
   ▼
49 (Particle System; ADR 0044 + Phase 47 instancing が前提)
   ▼
50 (Directional Shadow Pass; ADR 0036 の実装。Phase 41 contracts + Phase 47 batches が前提)
   ▼
51 (Packaging End-to-End; ADR 0045。Phase 34 settings + Phase 39 analysis が前提)
   ▼
52 (Particle Emitter Authoring; ADR 0027 registry パターン + Phase 49 particles が前提)
   ▼
┌─ UI トラック ──────────────┐   ┌─ アクション基盤トラック ─────────────┐
│ 53 (UI Foundation; ADR 0046) │   │ 57 (Action Collision Toolkit)         │
│    ▼                         │   │    ▼                                  │
│ 54 (UI Interactivity)        │   │ 58 (Targeting & Action Camera)        │
└──────────────┬───────────────┘   │    ▼                                  │
               │                   │ 59 (Animation Blending & Events)      │
55 (Scene Management) → 56 (Save/Load)  └────────────┬───────────────────────┘
               │                                     │
               └──────────► 60 (Script API v2) ◄─────┘
                                ▼
                            61 (Audio v2)
                                ▼
                            62 (Vertical Slice: busters_lite = M1 受け入れゲート)
                                ▼
                            63 (M1 Consolidation)
```

---

## テスト方針

各 Phase の完了基準を満たすテストを以下の種別で書く:

| Phase | テスト種別 |
|-------|-----------|
| 9-0 | scene `schema_version` ラウンドトリップ + version 欠落（v1 扱い）+ `UnsupportedVersion`。`DeleteEntity` の apply / inverse / `entity.has_children` 診断 |
| 9-A | `ProjectRoot::open()` のユニットテスト（temp dir 使用）。`resolve_asset` / `resolve_asset_for_write` のトラバーサル拒否 |
| 9-D | save → reload → 内容一致のインテグレーションテスト |
| 11-B | `Mesh::cube().validate()` 等 |
| 11-D | shader コンパイルテスト（wgpu offline validation） |
| 10-E | `FrameCapture` のサイズ・RGBA8 長の検証（既知色クリアの readback で色一致を assert） |
| 12-B | Fixed Update が正確な間隔で実行されるかのユニットテスト |
| 12-F | `InputCommand` 注入 → tick → `just_pressed` 観測 → 次 tick で消えることの状態遷移テスト。gamepad コマンド受理（no-op）テスト |
| 14-A/B | ファイルロードは実ファイルを使ったインテグレーションテスト |
| 15-B | registry 移行後に既存 bridge / editor テストが無変更で通過（挙動不変ゲート） |
| 15-C | 新 component の spawn / default / schema ラウンドトリップ。複数 directional light 時の採用規則 + 診断 |
| 15-D | `SetProperty` の apply / inverse / 診断（存在しない path・型不一致）。`SetComponentValue` との共存 |
| 15-E | ドラッグ操作 1 回で undo スタックが 1 エントリしか増えないこと |
| 16-B | manifest 追記の atomic write / 二重登録拒否 / ULID 発行のユニットテスト |
| 16-E | temp project + 実 OBJ の「登録 → 参照 → Play → 表示」統合テスト。欠損時 fallback + 診断 |
| 18-A/B | scene load / reload の成功・失敗（診断化）テスト |
| 20-C | 仮想入力 + `FrameCapture` のスモークテスト（背景クリア色以外の画素を含む） |
| 21 (旧 17-C) | AABB overlap のユニットテスト、entered/exited の状態遷移テスト |
| 22 (旧 18-C) | push-out の方向と量のユニットテスト |
