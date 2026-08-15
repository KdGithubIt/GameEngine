# Phase 14: Asset Import / Asset Pipeline

## Goal

外部ファイル（PNG テクスチャ・OBJ メッシュ）を AssetServer でロードし、
scene の entity に割り当てて Game View に表示できるようにする。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**現在の問題**:  
scene_bridge.rs は `BUILTIN_TRIANGLE_ASSET_ID` / `BUILTIN_QUAD_ASSET_ID` 等のハードコードされた
ビルトインアセット ID のみを受け付ける。実際のゲームで使うモデルやテクスチャを読み込む手段がない。

**Phase 13 の後にこのフェーズが来る理由**:  
Phase 13 でゲームが動くことを確認した後、見た目をリッチにするためにファイルベースのアセットが必要。  
Phase 13 まではプリミティブ（cube, sphere）で十分だったが、ゲームらしい見た目にするには
外部アセットが必要。

**OBJ を GLTF より先にする理由**:  
OBJ はテキスト形式で単純・パーサーが小さい (`tobj` クレート)。  
GLTF はバイナリ・JSON 混在で複雑（マテリアル・スケルトン・アニメーション等）。  
まず OBJ でアセットパイプラインの仕組みを作り、GLTF は Phase 26+（Advanced Authoring）以降で追加する。

---

## Scope

### 作るもの

- `AssetServer::load_texture(path)` — PNG テクスチャをディスクからロード
- `AssetServer::load_mesh(path)` — OBJ メッシュをディスクからロード
- `ProjectRoot` を `AssetServer` に持たせる（assets_root の安全なパス解決）
- `asset_manifest.json` の読み込みと `AssetId → path` の解決（ADR 0021）
- `scene_bridge.rs` で `asset_ref` の `AssetId` をマニフェスト経由でファイルパスに解決
- Missing asset / unregistered file / builtin conflict 時の Diagnostics 出力

### 作らないもの

- GLTF / GLB ロード（OBJ のみ）
- アセットの hot reload（ファイル変更検知）
- アセットのバックグラウンドロード（非同期）
- アセット変換・ベイク（生ファイルを直接使う）

---

## Design Decisions

### なぜ `AssetServer` にキャッシュを持たせるか

同じ `AssetId` を複数の entity が参照したとき、ファイルを 1 回だけ読んで同じ `Handle` を返す。  
テクスチャが重複 VRAM にアップロードされることを防ぎ、読み込み時間も短縮する。
キャッシュキーは `AssetId`（Windows でのパス大文字/小文字・セパレータ問題を回避）。

### なぜ assets_root の外へのパスを禁止するか

`"../../etc/passwd"` のようなパストラバーサルを防ぐ。  
`ProjectRoot::resolve_asset()` が全パス解決を担当し、assets_root 内に収まることを保証する。

### なぜ `spawn_from_authoring_scene` のシグネチャを変えないか（World からリソース取得）

現在のシグネチャ: `fn spawn_from_authoring_scene(world: &mut World, scene: &AuthoringScene)`  
`AssetServer` と `ProjectRoot` は world の resource として挿入されているので、
world から `get_resource::<AssetServer>()` で取得できる。  
シグネチャを変えると editor / example 側の呼び出し箇所を全部変える必要がある（破壊的変更）。

### アセット参照モデル（ADR 0021）

scene.json は `asset_ref` タグのみを使う（`asset_path` タグは**導入しない**）:
```json
{
  "engine.mesh": { "$type": "asset_ref", "id": "asset_01JZ..." }
}
```
`AssetId` → ファイルパスの解決は `asset_manifest.json` を通じて行う。

```json
{
  "schema_version": 1,
  "assets": {
    "asset_01JZ...": { "path": "meshes/player.obj", "name": "player_mesh" }
  }
}
```

- ファイルリネームは manifest 1 行の変更のみ。scene.json は不変。
- builtin アセット（`BUILTIN_TRIANGLE_ASSET_ID` 等）は manifest なしで解決継続。
- path-based タグ（`"$type": "asset_path"`）は仕様 §7.4 / ADR 0004 に違反するため**永久却下**。

### ビルトインアセット限定の先行エディタ統合（2026-06-11 実装済み）

Phase 11-D のライティング検証シーンをエディタだけで組めるよう、Phase 14 を待たずに
最小のエディタ統合を先行導入した:

- `crates/editor/src/ui/mod.rs` の `addable_component_registry()` が
  `ComponentSchemaRegistry::builtin()` に `engine.mesh` / `engine.material` スキーマを
  追加登録する（authoring クレートは engine のビルトインアセット ID を知らないため、
  両クレートに依存する editor 側で登録する）
- Inspector の AssetRef 値はビルトイン 4 アセット（triangle / quad / blue / orange）の
  コンボボックスで選択できる（`builtin_asset_choices()`）
- **14-C 実装時の置き換え対象**: このハードコードされた選択肢を
  `asset_manifest.json` ベースのアセット一覧に置き換える

### なぜ OBJ ロード時に normals がなければ計算するか

OBJ ファイルによっては `vn`（法線）の記述がない。その場合は flat normals（面ごとの法線）を
自動計算する。ライティングが効かない状態でロードされるよりはよい。

---

## Implementation Plan

### 14-A: AssetServer の拡張

`crates/engine/src/asset.rs` に追加:

```rust
pub struct AssetServer {
    assets_root: Option<PathBuf>,            // 追加
    mesh_cache:    HashMap<AssetId, Handle<Mesh>>,         // AssetId キー
    texture_cache: HashMap<AssetId, Handle<Arc<Texture>>>, // AssetId キー
}

impl AssetServer {
    pub fn with_assets_root(path: PathBuf) -> Self;

    pub fn load_texture(
        &mut self,
        relative_path: &str,
        meshes: &mut Assets<Arc<Texture>>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Handle<Arc<Texture>>, AssetLoadError>;

    pub fn load_mesh(
        &mut self,
        relative_path: &str,
        meshes: &mut Assets<Mesh>,
    ) -> Result<Handle<Mesh>, AssetLoadError>;
}

pub enum AssetLoadError {
    NoAssetsRoot,
    PathTraversal { requested: String },
    Io(std::io::Error),
    ParseError(String),
}
```

キャッシュロジック:
```rust
pub fn load_mesh(
    &mut self,
    asset_id: AssetId,
    manifest: &AssetManifest,
    meshes: &mut Assets<Mesh>,
) -> Result<Handle<Mesh>, AssetLoadError> {
    if let Some(handle) = self.mesh_cache.get(&asset_id) {
        return Ok(*handle);  // キャッシュヒット（AssetId キー）
    }
    let entry = manifest.get(&asset_id).ok_or(AssetLoadError::NotInManifest(asset_id))?;
    let path = self.resolve(&entry.path)?;  // assets_root 内に制限
    let mesh = load_obj(&path)?;
    let handle = meshes.add(mesh);
    self.mesh_cache.insert(asset_id, handle);
    Ok(handle)
}
```

### 14-B: OBJ ロード（前提: Phase 11-D 完了 — `Vertex.normal` フィールドが必要）

依存クレート追加:
```toml
# crates/engine/Cargo.toml
[dependencies]
tobj = "4"
```

```rust
// crates/engine/src/asset.rs 内のヘルパー関数
fn load_obj(path: &Path) -> Result<Mesh, AssetLoadError> {
    let (models, _) = tobj::load_obj(path, &tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ..Default::default()
    }).map_err(|e| AssetLoadError::ParseError(e.to_string()))?;

    let mut vertices = Vec::new();
    let mut indices = Vec::new();

    for model in &models {
        let mesh = &model.mesh;
        for i in 0..mesh.positions.len() / 3 {
            let pos = [mesh.positions[3*i], mesh.positions[3*i+1], mesh.positions[3*i+2]];
            let norm = if mesh.normals.is_empty() {
                [0.0, 1.0, 0.0]  // フォールバック法線
            } else {
                [mesh.normals[3*i], mesh.normals[3*i+1], mesh.normals[3*i+2]]
            };
            let uv = if mesh.texcoords.is_empty() {
                [0.0, 0.0]
            } else {
                [mesh.texcoords[2*i], 1.0 - mesh.texcoords[2*i+1]]  // Y反転（OBJ座標系）
            };
            vertices.push(Vertex { position: pos, normal: norm, color: [1.0; 3], uv });
        }
        indices.extend_from_slice(&mesh.indices);
    }

    Ok(Mesh { vertices, indices: Some(indices) })
}
```

### 14-C: scene.json でのアセット参照解決（manifest 方式, ADR 0021）

`scene_bridge.rs` の `resolve_mesh_value` を manifest 経由に書き換え:

```rust
// scene.json の値: { "$type": "asset_ref", "id": "asset_01JZ..." }
fn resolve_mesh_value(
    value: &Value,
    asset_server: &mut AssetServer,
    manifest: &AssetManifest,
    meshes: &mut Assets<Mesh>,
) -> Result<Handle<Mesh>, SceneBridgeError> {
    match value {
        Value::AssetRef(id) if is_builtin_mesh(id) => resolve_builtin_mesh(id),  // 既存
        Value::AssetRef(id) => {
            asset_server.load_mesh(*id, manifest, meshes)
                .map_err(SceneBridgeError::AssetLoad)
        }
        _ => Err(SceneBridgeError::InvalidComponentValue { ... }),
    }
}
```

`asset_path` タグは受け付けない（ADR 0021: path 参照は仕様 §7.4 違反）。

### 14-D: Missing Asset Diagnostics

```rust
// AssetLoadError を Diagnostic に変換
impl From<AssetLoadError> for Diagnostic {
    fn from(e: AssetLoadError) -> Self {
        Diagnostic::error(
            "asset.load_failed",
            format!("Failed to load asset: {e}"),
        )
    }
}
```

Play 中に missing asset が発生した場合:
- Game View にはエラーマテリアル（ピンク）を表示する
- Diagnostics パネルにエラーを追加する
- ゲームを止めない（他の entity は正常に動く）

---

## Cautions（注意点・落とし穴）

**OBJ の Y 軸方向**:  
OBJ は UV の Y 軸が上から下（テクスチャ座標系）、wgpu は下から上。  
`texcoords[2*i+1]` を `1.0 - value` に変換しないとテクスチャが上下逆になる。

**`tobj` の `single_index: true`**:  
OBJ は position/normal/uv を別々にインデックスできるが、wgpu は 1 つのインデックスしか使えない。  
`single_index: true` で統一インデックスに変換する。

**テクスチャの VRAM キャッシュ**:  
`Handle<Arc<Texture>>` を返すが、`Arc<Texture>` の内部に wgpu テクスチャが入っている。  
キャッシュされた Handle を複数の entity で共有しても、GPU リソースは 1 つだけ。

**OBJ ファイルが MTL ファイルを参照する場合**:  
`tobj` は MTL も読もうとする。MTL が存在しない場合はエラーを無視して続行する。  
Material は別の `engine.material` コンポーネントで設定する。

---

## Prohibited（禁止事項）

- GLTF のロードをこのフェーズで実装することを禁止（OBJ のみ）
- `AssetServer` が assets_root 外のパスを解決することを禁止（PathTraversal エラーを返す）
- ファイルロード失敗でゲームをクラッシュさせることを禁止（エラーマテリアルで継続）
- 同じ `AssetId` のアセットを重複ロードすることを禁止（キャッシュを必ず使う）
- `"$type": "asset_path"` タグを scene.json に追加することを禁止（ADR 0021 / §7.4 違反）

---

## Completion Criteria（完了基準）

> **2026-06-13 注記**: 以下のうち 2 項目は editor 経由では未達のまま
> Phase 14 を完了扱いとし、新 Phase 16（asset / OBJ / render の editor 統合、
> `docs/phases/phase-16-asset-editor-integration.md`）へ繰り越した:
>
> 1. 「Game View にメッシュが表示される」— `RuntimePlayState::start` が
>    `AssetServer` / `AssetManifest` を world に挿入しないため、editor の
>    Play では manifest アセットが解決されない（Phase 16-D）。
>    エンジン単体（world に resource を挿入した場合）では動作する。
> 2. テクスチャの manifest 解決 — GPU の device/queue 依存のため後送り
>    （Phase 16 でも対象外。時期未定）。
>
> また「ビルトイン限定の先行エディタ統合」のハードコード picker の
> manifest 置き換えは Phase 16-C が引き取る。

- `asset_manifest.json` に登録した `AssetId` を scene.json の `asset_ref` で参照し、Game View にメッシュが表示される
- テクスチャ (`AssetId`) を manifest 経由で参照し、entity のテクスチャとして表示される
- manifest に登録されていない `AssetId` を参照したとき、`asset.unregistered_file` 警告が Diagnostics に出てゲームは続行される
- manifest エントリが存在するがファイルが見つからないとき、`asset.missing_file` エラーが出てゲームは続行される
- 同じ `AssetId` を複数 entity が参照しても、ファイルは 1 回しか読まれない

---

## Feeds Into（次フェーズへの依存）

- Phase 20: minimal sample project で実際の OBJ を使う
- Phase 25: フル sample game（旧 20）で PNG テクスチャを含む全アセットを使う
