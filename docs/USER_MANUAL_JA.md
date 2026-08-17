# GameEngine 取扱説明書

対象: GameEngine ワークスペース 0.1.0（2026-07-22 時点の作業ツリー）

対象環境: Windows デスクトップ版エディター／デスクトップ版 Player

文書の目的: 現在のエンジンでゲームを作成、編集、実行、デバッグ、配布するための仕様と操作手順を一か所にまとめる。

> 本書は「将来の計画」ではなく、現行コード、Accepted ADR、機能到達性表、同梱サンプルを基準にしています。
> 画面に存在しない機能は、ランタイムやライブラリに実装があっても「エディター操作では未到達」と明記します。

## 1. エンジンの概要

GameEngine は Rust 製のデータ指向ゲームエンジンです。ゲームの編集データと実行時 ECS を分離し、同じプロジェクトを次の経路から扱えることを基本方針としています。

- 人間向けの `engine-editor`（egui／eframe 製ビジュアルエディター）
- JSON ベースのシーン、グラフ、UI、マテリアル、Prefab、プロジェクト設定
- プロジェクト固有の Rust ゲームコードを格納する `game/` クレート
- Behavior Tree 操作用の `engine-cli` と MCP アダプター
- エディター内 Play と配布用 `player` が共有するランタイム構成

編集時の Entity ID や Asset ID は永続 ID です。実行時 ECS の Entity や GPU ハンドルとは別物で、シーンや Prefab に実行時 ID は保存されません。

### 1.1 現在の主要機能

| 分野 | 現在の状態 |
| --- | --- |
| プロジェクト作成・オープン | 通常の Project Hub から利用可能 |
| シーン編集 | Hierarchy、Scene View、Inspector、Undo/Redo、保存に対応 |
| Rust ゲームコード | プロジェクトごとの Component／Resource／System を生成・ビルド可能 |
| 入力 | キーボード、マウス、ゲームパッド、仮想入力、Input Action に対応 |
| 描画 | OBJ、組み込み Mesh、Material v2、ライト、影、LOD、スキニング、パーティクル、HDR 後処理 |
| アニメーション | glTF／FBX クリップ、Animation Controller／Graph／Set、イベント、クロスフェード、ルートモーション |
| 物理・戦闘 | Primitive Collider、Character Controller、衝突イベント、Damage Receiver、Hitbox、Knockback |
| AI | グリッド NavMesh、A*、NavMesh Agent、Behavior Tree |
| UI | `*.ui.json` の宣言 UI、バインディング、ボタンイベント、ホットリロード |
| オーディオ | SE、BGM、バス音量、BGM クロスフェード、2D／3D authored playback |
| ゲームフロー | シーン切替、セーブスロット、実行時 Prefab spawn |
| デバッグ | Console、Problems、Input Debugger、Pause／Step、描画オーバーレイ、フレームキャプチャ |
| 配布 | Windows デスクトップ向けフォルダー形式パッケージ |

### 1.2 現時点で対象外または未完成のもの

- ネットワーク／マルチプレイヤー
- ローカル複数プレイヤー向けのコントローラー割り当て
- Web ブラウザーで実際に遊べる完成した WASM 配布
- Mesh Collider、任意形状の物理、完全な剛体物理エンジン
- 多層／任意傾斜地形を対象にした本格的な Recast 系 NavMesh
- ブレンドツリー、アニメーションレイヤー、IK
- UI Builder の Canvas 直接リサイズ／Anchor ハンドル、複数選択、型付き Binding／Event 候補、Focus 状態プレビュー
- アセットの未使用除外、実行ファイルへのアセット埋め込み、クロスコンパイル
- Rhai の新規機能拡張（既存機能は残っていますが、新規ゲームではプロジェクト Rust が主経路です）

## 2. 起動前の準備

### 2.1 必要なもの

1. Rust の stable toolchain と Cargo をインストールします。
2. `cargo` コマンドへ PATH が通っていることを確認します。
3. GPU ドライバーを更新し、wgpu が初期化できる状態にします。
4. プロジェクト固有 Rust コードを使う場合も同じ Rust toolchain を使用します。

確認コマンド:

```powershell
rustc --version
cargo --version
```

本リポジトリにはインストーラー形式のエディター配布物はありません。通常はソースからビルドして起動します。初回は依存関係のコンパイルに時間がかかります。

### 2.2 エディターの起動

最も簡単な方法は、リポジトリ直下の次のファイルをダブルクリックすることです。

```text
エディタをビルドして起動.bat
```

PowerShell から起動する場合:

```powershell
cd C:\RustProject\RustProject\GameEngine
cargo run -p engine-editor
```

起動に失敗した場合は、コンソールに表示された Cargo／Rust／GPU のエラーを確認してください。

## 3. まず動かす最短手順

### 3.1 新規プロジェクトを作る

1. エディターを起動します。
2. Project Hub の **New Project…** を押します。
3. 新しいプロジェクトとして使用するフォルダーを選択します。
4. 選択したフォルダー名がプロジェクト名になります。
5. エディターが次を自動生成します。
   - `project.json`
   - `project_settings.json`
   - `assets/` と標準サブフォルダー
   - `assets/scenes/main.scene.json`
   - プロジェクト固有 Rust コード用の `game/` クレート
6. `main.scene.json` が自動的に開きます。
7. ツールバーの **Play** を押します。
8. Game View をクリックして入力フォーカスを与えます。
9. **Stop** または `Esc` で Play を終了します。

新規シーンには、組み込み Quad、カメラ、Directional Light、Ambient Light が入っています。Start Scene は `scenes/main.scene.json` に設定されます。

### 3.2 同梱サンプルを動かす

機能確認には `examples/coin_collision_loop` を使います。同梱プロジェクトはこれ 1 本です。

1. Project Hub の **Open Project…** を押します。
2. `C:\RustProject\RustProject\GameEngine\examples\coin_collision_loop` を選択します。
3. **Build → Build Rust Game** を実行します。
4. ビルドが成功したら **Play** を押します。
5. Game View をクリックしてから操作します。

サンプルの想定フローは Title → Briefing → Arena → Result → Title です。設定上の操作は次のとおりです。

| 操作 | キーボード | ゲームパッド |
| --- | --- | --- |
| 移動 | `W` `A` `S` `D` | 左スティック |
| 攻撃 | `F` | West ボタン（index 2） |
| 回避 | `Space` | East ボタン（index 1） |
| ターゲット切替 | `Q` | North ボタン（index 3） |
| 決定／進行 | `Enter` | South ボタン（index 0） |
| ポーズ | `Esc` | Start ボタン（index 7） |

> 現在の組み込み Game View のキーボード転送コードは `W/A/S/D`、Space、矢印キーを明示転送しています。`F`、`Q`、Enter などの Action は配布 Player またはゲームパッドでは使用できますが、エディター内 Game View で反応しない場合があります。この場合はゲームパッドか配布 Player を使って確認してください。

## 4. エディター画面の構成

### 4.1 上部メニュー

| メニュー | 主な機能 |
| --- | --- |
| File | New Project、Open Project、Open Document、Save、Save As |
| Edit | Undo、Redo、Duplicate、Copy、Paste、Delete |
| View | Assets、Console、Problems、Input Debugger、Systems、Hierarchy の表示 |
| Project | Rust Game 初期化、Rust Script 作成、Rust ファイルを開く、NavMesh Bake |
| Build | Rust Game の Check／Build、Package Project、ビルド中止 |
| Help | エディター情報 |

### 4.2 ツールバー

- **Save**: 現在の Scene または Graph を保存します。
- **Undo / Redo**: 編集内容を戻す／やり直します。履歴上限は 100 件です。
- **Play**: 開いているシーンを実行します。
- **Build**: Play と Game Component が使用する Rust Game Code を build して読み込みます。`*` は未ビルドまたは変更後であることを表します。
- **Stop**: Play を終了します。
- **Pause / Resume**: Play の進行を一時停止／再開します。
- **Step**: Pause 中に FixedUpdate 1 回と Update 1 回だけ進めます。
- **Reload**: Play を停止せず、ディスク上の現在のシーンを再読込します。

Play 中はシーン編集と保存が無効です。これは実行中の ECS 状態を編集データへ誤って保存しないための仕様です。

### 4.3 左ドック

- **Hierarchy**: シーン Entity の一覧、検索、選択、複製、コピー、削除、作成。
- **Systems**: Update／FixedUpdate の実行順、Engine／Game フィルター、有効・無効、順序変更、制約確認。

### 4.4 中央ワークスペース

- Edit Mode の Scene を開いているとき: **Scene View**
- Play 中: **Game View**
- `*.graph.json` を開いているとき: **Graph Canvas**
- 何も開いていないとき: **Project Hub**

### 4.5 右 Inspector

- Scene では Entity 名、表示名、説明、Component を編集します。
- Graph では選択 Node の ID、型、名前、Properties JSON を編集します。
- Component の AssetRef、EntityRef、列挙値、数値範囲、Layer Mask などは Schema に応じた専用 UI になります。
- Play 中は編集不可です。選択 Entity の一部ランタイム状態は読み取り専用デバッグ表示になります。

### 4.6 下ドック

- **Assets**: `assets/` と `game/src/` のファイル一覧。
- **Console**: 時刻付きの実行ログと診断。
- **Problems**: 継続的に確認すべきエラー／警告。対象 Entity や Rust ソースへ移動できます。
- **Input**: Play 中の物理入力、ゲームパッド、解決済み Input Action を表示します。

### 4.7 ステータスバー

プロジェクト名、保存状態、Error／Warning 件数、Rust build 状態、アセット import の進捗を表示します。

## 5. プロジェクトの扱い

### 5.1 プロジェクトを開く

1. Project Hub または **File → Open Project…** を選択します。
2. `project.json` があるプロジェクトルートを選択します。
3. `project_settings.json` の `start_scene` があればその Scene を開きます。
4. Start Scene が未設定なら、Assets 内で最初に見つかった Scene をエディター上だけのフォールバックとして開きます。

未保存のドキュメントがある状態で別プロジェクト／ドキュメントを開くと、保存、破棄、キャンセルの確認が表示されます。

### 5.2 標準および推奨フォルダー構成

次は機能別の推奨配置を含む全体像です。新規作成時に存在しないサブフォルダーは、アセットを追加するときに必要に応じて作成してください。

```text
<project>/
  project.json
  project_settings.json
  asset_manifest.json
  assets/
    scenes/
    graphs/
    meshes/
    textures/
    audio/
    materials/
    animations/
    navigation/
    prefabs/
    ui/
    scripts/
      rhai/
      rust/
        components/
        resources/
        systems/
        shared/
  game/
    Cargo.toml
    src/
      lib.rs
      project_modules.rs
```

`assets/scripts/rust/` 以下は自由にフォルダを作れます。上の 4 フォルダは新規作成時の既定保存先という推奨であり、種類ごとの強制カテゴリではありません。`game/src/project_modules.rs` はフォルダ構造から自動生成されるモジュール索引で、手で編集しません。

`asset_manifest.json` はプロジェクトデータであり、Assets 一覧には表示されません。シーンはファイルパスで管理され、それ以外の多くのアセットは永続 `AssetId` で参照されます。

### 5.3 プロジェクト設定

`project_settings.json` には次が保存されます。

- Tags
- 最大 32 スロットの Layers
- Input Actions
- Start Scene
- Update／FixedUpdate の System 順序と無効化設定

現在、`ProjectSettingsPanel` の実装はありますが、メインエディターのメニュー／ドキュメント経路へ接続されていません。Tags、Layers、Input Actions、Start Scene を変更する通常の画面操作は現時点では提供されていないため、Play を停止して外部テキストエディターで `project_settings.json` を編集してください。System 順序と有効／無効は **View → Systems** から編集でき、自動保存されます。

最小例:

```json
{
  "schema_version": 1,
  "tags": ["player", "enemy"],
  "layers": [
    { "index": 0, "name": "World" },
    { "index": 1, "name": "Attack" }
  ],
  "input_actions": [
    { "name": "move_forward", "keys": ["KeyW"] },
    { "name": "attack", "keys": ["KeyF"], "gamepad_buttons": [2] }
  ],
  "start_scene": "scenes/main.scene.json"
}
```

手動編集後はプロジェクトを開き直すか、Play を停止して再度開始してください。System 設定を含む変更は既に動いている Play world には適用されません。

## 6. シーン編集

### 6.1 Entity を作成する

1. Hierarchy の空白部分を右クリックします。
2. **Create** を開きます。
3. プリセットを選びます。

| プリセット | 初期 Component |
| --- | --- |
| Empty Entity | なし |
| Player | Transform、Player Marker、Player Controller、Character Controller、Collider、Kinematic Physics Body、Damage Receiver、Lock-On Camera |
| Enemy | Transform、NavMesh Agent、Collider、Kinematic Physics Body、Damage Receiver、Lock-On Target、Runtime Metadata |
| Camera | Transform、Camera |
| Directional Light | Transform、Directional Light |
| Primitive / Triangle | Transform、組み込み Triangle Mesh、Material |
| Primitive / Quad | Transform、組み込み Quad Mesh、Material |

作成後は Entity が選択され、Inspector から編集できます。

### 6.2 Entity の選択と編集

- Hierarchy の行をクリックすると選択します。
- Scene View 上の Entity をクリックしても選択できます。
- Hierarchy 検索欄では名前、表示名、Entity ID を絞り込めます。
- Inspector の Name／Display Name／Description は編集後に反映されます。
- Component は折りたたみ見出しごとに編集できます。
- **Remove Component** で削除します。
- **Add Component** の検索欄と一覧から Engine／Game Component を追加します。

Project Rust の Component は、Rust Game の Build が成功して Schema をロードした後に Add Component 一覧へ表示されます。

### 6.3 Scene View のカメラ操作

| 操作 | 入力 |
| --- | --- |
| Orbit | 右マウスボタンをドラッグ |
| Pan | 中マウスボタンをドラッグ |
| Zoom | マウスホイール |
| Entity 選択 | 左クリック |

Scene View 用カメラはエディターだけの状態で、`*.scene.json` には保存されません。

### 6.4 Transform Gizmo

| モード | ボタン | ショートカット |
| --- | --- | --- |
| Move | Move | `T` |
| Rotate | Rotate | `R` |
| Scale | Scale | `S` |

現在の Scene View の直接ドラッグ処理は Move の軸ドラッグが中心です。Rotate／Scale の Component 値は Inspector でも編集できます。Gizmo の変更は Authoring Command を通り、Undo 対象になります。

### 6.5 複製、コピー、削除

| 操作 | ショートカット |
| --- | --- |
| Duplicate | `Ctrl+D` |
| Copy | `Ctrl+C` |
| Paste | `Ctrl+V` |
| Undo | `Ctrl+Z` |
| Redo | `Ctrl+Y` または `Ctrl+Shift+Z` |
| Save | `Ctrl+S` |
| Save As | `Ctrl+Shift+S` |
| Open Document | `Ctrl+O` |

Duplicate／Paste は新しい永続 Entity ID を生成するため、ID 衝突は起こしません。

### 6.6 保存

- Scene は `*.scene.json` として保存します。
- Graph は意味データ `*.graph.json` と表示データ `*.graph.view.json` に分離して保存します。
- Play 中は Save／Save As が無効です。
- 保存は置換方式を使用し、編集中ドキュメントの Dirty 状態はステータスバーで確認できます。

## 7. 組み込み Component 一覧

Inspector から追加できる現行の組み込み Component は次のとおりです。

| 分類 | 表示名／ID | 用途 |
| --- | --- | --- |
| 基本 | Transform / `engine.transform` | 位置、回転、Scale、親子 Transform の基礎 |
| 基本 | Player Marker / `engine.player_marker` | プレイヤー Entity の識別 |
| 描画 | Static Mesh Renderer / `engine.static_mesh_renderer` | Mesh、基本 Material、Submesh ごとの Material Slots |
| 描画 | Skinned Model / `engine.skinned_model` | Skeleton サブアセットからリグを生成。参照中の Renderer 一覧は読み取り専用で表示 |
| 描画 | Bone Attachment / `engine.bone_attachment` | Rig の指定ボーンに追従（この Entity の Transform がボーンからのオフセット） |
| 描画 | Skinned Mesh Renderer / `engine.skinned_mesh_renderer` | Mesh サブアセット、Material、Material Slots、使用する Skinned Model |
| 描画 | LOD Group / `engine.lod_group` | 距離順の Mesh 切替 |
| 描画 | Camera / `engine.camera` | 透視カメラ |
| 描画 | Directional Light / `engine.directional_light` | 平行光源 |
| 描画 | Ambient Light / `engine.ambient_light` | 環境光 |
| 描画 | Shadow Settings / `engine.shadow_settings` | Directional shadow 設定 |
| 描画 | Environment Lighting / `engine.environment_lighting` | Scene 固有の環境ライティング |
| 描画 | Post Process / `engine.post_process` | Exposure、Tone Mapping、Bloom |
| 描画 | Particle Emitter / `engine.particle_emitter` | Mesh と Material を含む CPU simulation + instanced particle |
| 移動 | Player Controller / `engine.player_controller` | Input Action から移動意図を生成 |
| 移動 | Character Controller / `engine.character_controller` | Kinematic 移動、Slope、Step、Snap、Ceiling |
| カメラ | Orbit Camera / `engine.orbit_camera` | Orbit 追従 |
| カメラ | Follow Camera / `engine.follow_camera` | Entity 追従 |
| カメラ | Lock-On Camera / `engine.lock_on_camera` | 対象フレーミングと壁回避 |
| 物理 | Collider / `engine.collider` | Box、Sphere、Y Capsule、Trigger |
| 物理 | Physics Body / `engine.physics_body` | Static／Dynamic／Kinematic |
| 戦闘 | Damage Receiver / `engine.damage_receiver` | HP、Team、無敵時間 |
| 戦闘 | Lock-On Target / `engine.lock_on_target` | ロックオン対象 |
| AI | NavMesh Agent / `engine.nav_mesh_agent` | Path 追従、停止距離、Repath、回避 |
| AI | NavMesh Surface / `engine.nav_mesh_surface` | Scene の NavMesh AssetRef |
| AI | Behavior Tree Runner / `engine.behavior_tree_runner` | Behavior Tree の実行 |
| Animation | Animation Controller / `engine.animation_controller` | 同じ Entity の Skinned Model が持つリグを再生対象として、Graph 状態遷移、Event、Root Motion を管理 |
| UI | UI Document / `engine.ui_document` | `*.ui.json` を HUD／Menu として表示 |
| Audio | Audio Emitter / `engine.audio_emitter` | authored SE、2D／3D 再生 |
| Audio | Audio Listener / `engine.audio_listener` | 聴取位置 |
| Audio | Music Controller / `engine.music_controller` | BGM と Crossfade |
| Metadata | Runtime Metadata / `engine.runtime_metadata` | Game 表示名、Tags、Team |

依存関係がある Component は不足を Problems に報告します。例として Animation Controller は同じ Entity の Skinned Model、Graph を使う場合は Animation Set、Spatial Audio Emitter は Transform、Character 系は Kinematic Controller 経路が必要です。Entity 参照フィールドは受け付ける Component を宣言しているため、Skinned Mesh Renderer の Model ドロップダウンには Skinned Model を持つ Entity だけが並びます。未設定はバインドポーズで編集でき、Clear で参照を外せます。誤った参照は Problems に `scene.entity_reference_wrong_target` として報告されます。

## 8. アセット操作

### 8.1 アセットを追加する基本手順

OS のファイルブラウザーから Assets タブへ、ファイルまたはフォルダーをドラッグ＆ドロップできます。フォルダーは再帰的に走査され、対応しているアセットファイルだけが元の相対フォルダー構造を維持してコピー・登録されます。`.txt`、`.fx`、`.x` などの未対応ファイルはエラーにせず無視されます。コピー失敗や Manifest 保存失敗が発生した場合は、そのドロップで作成したファイルとフォルダーがロールバックされます。

1. 対象ファイルをプロジェクトの `assets/` 以下へコピーします。
2. Assets タブで空白を右クリックし **Refresh** を選びます。
3. ファイルを右クリックして **Register Asset** を選びます。
4. `asset_manifest.json` に新しい永続 Asset ID と相対パスが保存されます。
5. Inspector の AssetRef 欄から登録済みアセットを選択します。

シーンは `assets/scenes/` の相対パスで扱います。その他の実行時アセットは原則として Asset ID 経由です。ファイルを移動するときは、Scene の AssetRef を書き換えるのではなく Manifest の path を更新します。

### 8.2 Asset Browser の分類

| 表示 | 対象 |
| --- | --- |
| `[scene]` | `*.scene.json` |
| `[graph]` | `*.graph.json` |
| `[tex]` | PNG、JPG／JPEG、WebP、BMP |
| `[mesh]` | OBJ、glTF、GLB |
| `[audio]` | WAV、OGG、MP3、FLAC（Browser の分類。現行 runtime loader が再生できるのは WAV／OGG） |
| `[mat]` | `*.material.json` |
| `[prefab]` | `*.prefab.json` |
| `[ui]` | `*.ui.json` |
| `[nav]` | `*.navmesh.json` |
| `[script]` | `*.rhai` |

Graph のノード位置やズーム状態を保存する `*.graph.view.json` は、対応する `*.graph.json` の内部的な表示用サイドカーとして管理されるため、Asset Browser には表示されません。Graph を移動、名前変更、または削除すると、対応する表示用サイドカーも同じ操作で追従します。

### 8.3 Mesh の配置

登録済みの Mesh または glTF の imported mesh sub-asset は、Assets から Scene View へドラッグできます。Drop すると Transform、Mesh、Material を持つ Entity が作成されます。

### 8.4 Texture と Material

Texture をダブルクリックすると Texture Preview が開きます。Material をダブルクリックすると Material Editor が開きます。

Material Editor で編集できる主な項目:

- Base color
- Roughness
- Metallic
- Base color／Normal／Emissive texture
- Emissive color
- Alpha mode（Opaque／Mask／Blend）と Alpha cutoff
- Cull mode（Back／Front／None）
- Shading（Lit／Unlit）

Material の変更は検証後に `*.material.json` へ保存されます。Material v1 は v2 の既定値を補って読み込めます。Texture は一辺 8,192 px が上限です。

### 8.5 glTF／GLB

現行 UI では glTF／GLB を Register すると background import が開始され、Import 進捗が表示されます。成功すると Mesh、Material、Texture、Skeleton／Skin、Animation Clip の stable sub-asset 情報が Manifest に保存されます。再出力後は右クリックの **Reimport** を使用します。

ただし公式の機能到達性表では glTF pipeline 全体はまだ `code_only`、Mesh／Material authoring は `partial` と評価されています。一般的な DCC データすべてを保証する完成済み importer としては扱わず、実データごとに Scene View、Play、Package を確認してください。Sub-asset の元 index を維持すると Reimport 後も ID が安定します。

### 8.6 FBX

`ufbx` ライブラリ経由で FBX を直接読み込めます（ADR 0081）。Mixamo などから取得した `.fbx` をそのまま `assets/` に置き、glTF/GLB と同じ手順で **Register Asset** してください。Unit／Axis 変換、Pivot／PreRotation／PostRotation の平坦化、Animation stack のリサンプルはインポータが自動で行います。

ブレンドシェイプや NURBS など、直接インポートが対応していない機能を使ったソースだけは、従来どおり DCC ツールで glTF 2.0 の `.gltf` または `.glb` へ変換してから置いてください（詳細は [FBX 変換手順](FBX_IMPORT.md) を参照）。同じモデルを FBX と glTF の両方で登録した場合、サブアセット ID は一致しません。フォーマットを切り替える場合は Reimport ではなく再登録として扱い、シーン／Prefab 側の参照を新しい ID に張り替えてください。

### 8.7 欠損アセット

- Manifest にあるファイルが消えた場合は `asset.missing_file`。
- `assets/` にあるが未登録のファイルは `asset.unregistered_file`。
- 無効な Mesh／Material／Texture の一部は、診断を出したうえで Cube／Magenta checker の可視 fallback を使用します。
- Package では欠損必須アセットが blocking error になります。

## 9. 描画、Animation、Particle

### 9.1 Scene 固有の描画設定

Shadow Settings、Environment Lighting、Post Process は Scene Entity の Component として配置します。同種を複数置かないでください。Directional Light と Ambient Light も原則 1 個で、複数時は警告と安定順による扱いになります。

Post Process は HDR `Rgba16Float`、既定 ACES fitted Tone Mapping、任意の Reinhard、Exposure、Bloom を使用します。

### 9.2 LOD

1. Entity に **LOD Group** を追加します。
2. `+ Add LOD` で距離と Mesh を追加します。
3. 距離は正値かつ厳密な昇順にします。
4. Scene View の **LOD Debug** を有効にして閾値と現在レベルを確認します。

通常の静的 Mesh は Entity に **Static Mesh Renderer** を追加し、Mesh と Material をその Component 内で設定します。Material を変更しても Mesh 参照は維持され、Mesh を変更しても Material は維持されます。Material の初期値は組み込み White なので、追加直後から白色で描画されます。複数 Submesh を持つ Mesh では Material Slots を Submesh 順に追加します。

### 9.3 Skinned Mesh と Animation

1. glTF／GLB／FBX を `assets/` に置いて import を完了します。
2. Asset Browser からモデルを Scene View へドラッグします。**Skinned Model** を持つ Entity と、その配下の Render Part（Skinned Mesh Renderer）が Mesh・Material 込みで生成されます。Skeleton や Skin を手作業で接続する操作はありません。
3. 身体・服・髪のように描画部位が複数ある場合も、それぞれの Skinned Mesh Renderer の **Model** に同じ Skinned Model が設定され、1つのリグを共有します。Model Inspector の Renderer 一覧はこの参照を逆引きした読み取り専用表示です。別のリグで動かす場合は Renderer 側の Model を変更します。
4. アニメーションさせる場合は、Skinned Model と同じ Entity に **Animation Controller** を追加し、Animation Set、Graph、Loop、Speed、Root Motion を設定します。Controller に Skeleton を選ぶ欄はありません。Animation Set と Graph が両方未割り当てならリグはレストポーズのままです。Inspector の **Animation Assets** 欄では、割り当て済みの Graph／Set を直接開けます。未割り当ての場合は Create Graph／Create Set から作成、Manifest 登録、Controller への割り当て、エディター表示を続けて実行できます。Animation Set の作成には先に Graph が必要です。
5. idle→walk→run などの遷移が必要なら、**File → New Animation Graph** でアセットを作成します。Graph Canvas の **Add Node → State** で状態を追加し、State の Inspector に Motion Slot Name を設定します。内部では名前変更に影響されない Motion Slot ID が保持されます。
6. Entry を右クリックして **Connect From** を選び、最初に再生する State をクリックします。同様に State から別の State へ接続します。遷移線をクリックすると Inspector で Boolean の **Condition Parameter** と Crossfade の **Fade Duration** を編集できます。
7. Asset Browser で Animation Graph を右クリックして **Create Animation Set** を選びます。生成された `*.animset.json` で各 Motion Slot に Animation Clip サブアセットを割り当てます。1つの Set から複数の glTF／GLB／FBX ソースの Clip を参照できます。攻撃判定などの Animation Event は、この Animation Set ウィンドウの各 Binding にある **Events** で追加します（時刻とイベント名）。Animation Controller 側にイベント欄はありません。
8. 作成した Animation Graph と Animation Set を同じ **Animation Controller** に割り当て、Condition Parameter と同名の Boolean parameter default を設定します。Animation Set だけを設定した単体クリップ再生はサポートしません。
9. Scene View の **Animation Preview...**、または **View → Animation Preview...** を開きます。Clip タブは単体 Motion、Transition タブは指定した From／To、開始時刻、Fade、Graph タブは実際の Boolean parameter 遷移を確認します。Transition の Repeat と Cycle で同じブレンドを繰り返せます。
10. State Inspector の **Preview Bound Motion**、または遷移 Inspector の **Preview Transition** からも対象を直接開けます。Pause、Restart、Speed、Time スライダーは Preview 専用で、Scene 文書や Play world へ保存されません。

Skinned Model Component を削除すると、それを参照していた Renderer の Model は同じ Undo 操作内で未設定になります。Renderer 自体は削除されず、Mesh はバインドポーズの編集状態で残ります。Hierarchy で Skinned Model Entity を削除した場合も、それを参照する子 Renderer の枝は削除サブツリーの外へ移され、ワールド Transform を保ったまま Model が未設定になります。Renderer 自身も削除選択へ明示的に含めた場合は削除されます。

モデルソースを再インポートしたとき、Mesh の中身が更新されただけなら Scene はそのまま反映します。ソース側で Mesh ノードが追加・削除された場合は Scene を自動では書き換えません。**Project → Resync Model Parts** で、選択中の Skinned Model を参照する Renderer を逆引きし、不足している Render Part を追加できます。ソースから消えた Mesh を描く Render Part は削除せず、警告として報告します。

武器やエフェクトをボーンに持たせるには、その Entity に **Bone Attachment** を追加し、Rig に対象の Skinned Model、Bone に骨を指定します。Bone はドロップダウンから名前で選び、内部では名前変更に影響されない Bone ID が保存されます。変換時にその Entity は対象の joint の子になるため、Entity の Transform はボーンからのオフセットとして働き、配下の Entity も一緒に追従します。Rig やボーンが解決できない場合は Entity を動かさず、Problems に報告します。

アニメーションさせない Skinned Model は、Skinned Model の Inspector にある **Bake to Static Mesh** でレストポーズの静的 Mesh に変換できます。生成した OBJ は `assets/baked_meshes/` に保存・Manifest 登録され、Render Part は Static Mesh Renderer に置き換わり、未設定の Animation Controller と Skinned Model は取り除かれます。複数 Submesh は Material を保った複数の兄弟 Entity になります。Graph が設定済みの Animation Controller、または対象リグを使う Bone Attachment がある場合は安全のため変換せず、Problems に理由を表示します。Scene 側の変更は1回の Undo で戻せますが、生成済み OBJ と Manifest 登録は新規プロジェクトアセットとして残ります。

ゲームコードから任意のボーン姿勢を問い合わせる API は用意していません。必要な位置には Bone Attachment で Entity を置き、その Entity の Transform を読みます。

Authoring Component の形は 1 つずつです。旧形式（Animation Controller に Skin を設定し、Skinned Mesh Renderer が Skeleton Entity を参照する構成）や旧 `engine.mesh`／`engine.material`／`engine.material_slots`／`engine.skinned_mesh`／`engine.skeleton`／`engine.animator`／`engine.animation_graph_player` は ADR 0091 で削除済みで、読み込めません。変換コマンドも提供していません。

現行の遷移条件は Boolean parameter です。「一定時間後」は game code の timer が parameter を `true` にし、「Space が押されたら」は Input Action が jump parameter を `true` にし、「着地したら」は Character Controller の grounded 状態が landing parameter を `true` にする形で構成します。Condition Parameter を空欄にした遷移は次の graph tick で直ちに発火するため、待ち時間の代用にはなりません。

Preview は一時的な World を使用し、Scene 文書や Play world を変更しません。現行機能到達性では Animation は `partial` です。最終的な visual／package acceptance はプロジェクトごとに確認してください。

### 9.4 Particle

1. Entity に **Particle Emitter** を追加します。
2. Mesh、Material、最大 Particle 数、Spawn rate、Lifetime、Velocity、Color 等を設定します。Material の初期値は組み込み White です。
3. Scene View の **Particle Preview** を有効にします。
4. **Particle Debug** で Bounds、方向、Rate、Pool 使用数を確認します。
5. **Restart Particles** で再スタートします。

1 emitter の最大数は 65,536、1 frame の spawn は最大 256 です。Scene 全体の worst-case render instance は 100,000 が上限です。

## 10. Collision、Character、Combat

### 10.1 Collision の基本構成

動かない床や壁:

- Transform
- Collider
- Physics Body = Static

Character:

- Transform
- Collider（Box／Sphere／CapsuleY）
- Physics Body = Kinematic
- Character Controller

Trigger:

- Collider の Trigger を有効化
- 必要な Layer／Mask を設定

Mesh Collider はありません。複合 Static 環境は、複数の子 Entity に Primitive Collider と Static Body を付けて構成します。

### 10.2 Character Controller

Character Controller は FixedUpdate で動作し、次を扱います。

- Slope limit
- Step offset
- Ground snap
- Skin width
- Ceiling
- 高速移動の細分化
- Character 同士の対称分離
- Root motion の移動取り込み

Player Controller は Project Settings の Input Action を読み、カメラ相対 XZ 移動と加減速を Character Controller へ渡します。Transform を Update で直接積分する方式ではありません。

### 10.3 Combat

- Damage Receiver が HP、Team、Invulnerability を保持します。
- Attack Hitbox は Box／Sphere／Y Capsule の Trigger として Game code から生成します。
- 同一 activation での one-hit 履歴を engine が管理します。
- Hit result と任意 Knockback は FixedUpdate で処理されます。
- Lock-On Target と Lock-On Camera で対象選択、Camera framing、Static wall 回避を行います。

Play 中に **Debug Draw** を有効にすると Collider、NavMesh path、Combat 関連の debug line を確認できます。上部には broad-phase proxy／candidate／contact 数も表示されます。

## 11. NavMesh と Behavior Tree

### 11.1 NavMesh を Bake する

1. Scene に Floor と Static obstacle の Primitive Collider を配置します。
2. 対象 Scene を保存します。
3. **Project → Bake NavMesh** を選びます。
4. `assets/navigation/<scene名>.navmesh.json` と bake document が生成されます。
5. Asset Manifest に NavMesh が登録されます。
6. Scene に NavMesh Surface がなければ自動追加されます。
7. Save してから Play します。

Bake は Static、non-trigger の障害物を walkable height／agent height band で判定します。Scene 変更後は fingerprint により古い Bake を検出します。現行方式は平面的な Arena 向け grid NavMesh です。

### 11.2 NavMesh Agent

Enemy などへ NavMesh Agent を追加し、Speed、Stopping distance、Repath interval、Avoidance radius を設定します。Target の設定／解除はプロジェクト Rust の Navigation command から行います。状態は Idle、Missing、NoPath、Moving、Arrived として参照できます。

### 11.3 Behavior Tree Graph

現行 Graph Canvas は既存の `*.graph.json` を開いて編集します。

Graph Canvas（Behavior Tree／Animation Graph 共通）の視点操作:

- **中ボタンドラッグ**でキャンバスをパンします。Node の上から始めても Node は動かず、視点だけが移動します。Node の移動は左ドラッグのみです。
- ツールバーの **Frame All**、または何もない場所を右クリックして **Frame All Nodes** を選ぶと、すべての Node が見える位置まで視点を移動します。**Reset View** は視点を graph 原点へ戻します。
- Node を画面外までドラッグして見失った場合も、上記いずれかで復帰できます。視点は表示状態だけの情報で、Node の座標や保存ファイルには影響しません。

1. Behavior Tree graph を Assets からダブルクリックします。
2. **Add Node** から Root、Sequence、Selector、Condition、Action、Decorator を追加します。
3. Node を選択し、Inspector の Properties JSON を編集します。
4. 接続元 Node の context action を使い、接続先 Node を選びます。
5. **Layout** で incremental layout を実行します。
6. Node を drag して配置し、必要なら pin します。
7. Save すると semantic graph と view file が分離保存されます。

Behavior Tree Runner を Entity へ追加し、登録済み Behavior Tree を割り当てます。Action／Condition の stable behavior ID に対する実装結果は project Rust module から登録します。未登録 behavior、invalid graph、NavMesh 不足は診断になります。

### 11.4 Animation Graph

1. **File → New Animation Graph** を選ぶと、`assets/animation/` に semantic graph と editor view が作成され、Asset Manifest に登録されます。
2. **Add Node → State** で必要な状態を追加します。
3. State を選択し、Inspector の **Motion Slot Name** に用途名を入力して **Apply State** を押します。保存済みの Motion Slot ID は表示名を変更しても維持されます。
4. Node を右クリックして **Connect From** を選び、遷移先 Node をクリックします。Entry は初期 State に、State は別の State に接続できます。
5. 遷移線をクリックし、Inspector の **Condition Parameter** と Fade を設定して **Apply Transition** を押します。空欄の Condition は無条件遷移です。
   **Preview Transition** を押すと、選択した From／To の遷移だけを Animation Preview ウィンドウで繰り返し確認できます。
6. **Layout** または drag で見やすく配置し、Save します。
7. Asset Browser で Graph を右クリックして **Create Animation Set** を選び、各 Motion Slot に imported Animation Clip サブアセットを割り当てます。
8. Animation Controller の Graph と Animation Set に作成したアセットを割り当てます。

Node と遷移線の変更は Undo／Redo の対象です。クリップそのものの作成や編集は Graph Editor の責務ではなく、Blender などの DCC と import workflow で行います。

### 11.5 CLI で Behavior Tree を扱う

```powershell
cargo run -p engine-cli -- behavior-tree schemas
cargo run -p engine-cli -- behavior-tree example
cargo run -p engine-cli -- behavior-tree validate <graph.json>
cargo run -p engine-cli -- behavior-tree compile <graph.json>
cargo run -p engine-cli -- behavior-tree layout <graph.json>
cargo run -p engine-cli -- behavior-tree nodes <graph.json>
cargo run -p engine-cli -- behavior-tree edges <graph.json>
cargo run -p engine-cli -- behavior-tree preview <graph.json> <commands.json>
cargo run -p engine-cli -- behavior-tree apply <graph.json> <commands.json>
```

`commit` は `apply` の非推奨 alias です。CLI、MCP、Editor は同じ Authoring Command、Validation、Transaction 実装を使用します。

## 12. 宣言 UI

### 12.1 UI Document の仕様

`*.ui.json` は schema version 2 の tree document です。version 1 は読み込み時に自動移行されます。

| Node | 用途 |
| --- | --- |
| panel | 9 方向 Anchor、Offset、縦／横 Layout、Spacing、Background、Padding |
| text | Literal または Binding の文字列、Size、Color |
| button | Label と Event name |
| spacer | 固定間隔 |
| image | Texture path、Tint、Width／Height、Nine-slice border |
| progress_bar | Literal／Binding の現在値と最大値、Fill、Label |
| stack | 縦／横のレスポンシブ配置 |
| grid | 列数を指定した行優先 Grid |
| overlay | 子要素を重ねる Container |
| scroll_view | 縦／横 Scroll と最大表示サイズ |

各 Node の `id` は document 内で一意にします。Binding は `{ "$bind": "score" }` のように名前で参照し、Game code が UiBindings を更新します。未解決 Binding は `--` を表示して warning を出します。

### 12.2 Scene へ配置する

1. Assets の右クリックメニューから **Create → UI Document** を選びます。
2. 作成した UI をダブルクリックすると、Scene View を閉じずに新しいドキュメントタブで UI Builder が開きます。上部タブで Scene View と UI Builder を切り替えます。
3. UI Builder は **Hierarchy で追加先を選択 → Palette から要素を追加 → Canvas または Hierarchy で選択 → Inspector で編集** の順に操作します。選択中の追加先は Palette 上部に表示されます。
4. Save して Assets で Register Asset します。
5. Entity を作り **UI Document** Componentを追加し、UI Asset を選びます。
6. Play すると Game View 上へ描画されます。

UI Builder は Undo／Redo、Copy／Paste、Delete、Node ID の変更、Hierarchy 検索、主要解像度の Preview に対応します。保存前には document 全体を Validation し、30 秒ごとに crash-recovery snapshot を作成します。

Scene View のツールバーでは **UI表示** と **UI選択** を切り替えられます。初期状態は UI表示がON、UI選択がOFFです。どちらもScene Viewだけの一時的な表示状態で、Scene、UI Document、Play時の表示状態には保存されません。

- **UI表示 OFF**: Scene ViewのUIを隠し、UI選択も自動的にOFFにします。
- **UI選択 OFF**: UIを表示したまま、その背後にある3D Entityを選択できます。
- **UI選択 ON**: クリック位置で最前面のUI Nodeを選び、所有するUI DocumentをUI Builderで開きます。選択Nodeは水色の枠で表示され、UI Builderの選択と同期します。
- UI Nodeがない場所は通常の3D Entity選択へフォールバックします。Edit ModeのButtonをクリックしてもゲーム用イベントは発行されません。

同じ Display メニューの **Game Frame** は、UI を配置・スケールする基準となるターゲット画面を選びます。初期値は 16:9 です。フレームは「その解像度の画面を縮小表示したもの」として扱われ、`16:9` は 1920x1080、`4:3` は 1440x1080 の画面としてレイアウトされます。したがって、1920x1080 用に作った HUD が画面幅の 3 割を占めるなら、Scene View のフレーム内でも幅の 3 割に見えます。ドックを縦長・横長にしても、アンカー位置・余白・占有率が Play 時と一致します。フレームの外側は暗く表示され、実際の画面に映る範囲が分かります。`Free` を選ぶとパネルそのものを画面サイズとして扱います（ADR 0090）。

Game View の解像度プリセットも同じ扱いです。`1920x1080` はその解像度でレンダリングした画像をパネルに縮小表示し、UI も同じ倍率で縮小されるため、ウィンドウサイズに関係なく製品画面の見た目を確認できます。UI Builder のプレビューでも Zoom は「拡大縮小」であり、レイアウトそのものは選んだプレビュー解像度で決まります。

Button は callback ではなく event name を発行します。Project Rust、既存 Rhai、Behavior Tree が event を消費します。デスクトップ Play 中は source file の mtime を監視し、約 0.5 秒間隔で hot reload します。Parse 中の不正 JSON は旧 UI を維持します。

日本語表示には host／project から CJK font bytes を登録する必要があります。エンジン自体はフォントを同梱しません。

## 13. Audio

### 13.1 Authored Audio

1. Audio file を `assets/audio/` へ置き、Register Asset します。
2. 3D／Entity 付随音は Transform と Audio Emitter を追加します。
3. Scene に Audio Listener を配置します。
4. BGM 制御には Music Controller を使用します。
5. Play で確認します。

Master、BGM、SE の volume は独立しており、実効音量は `master × bus` です。BGM は loop、fade-in、crossfade に対応します。Audio device がない headless 環境では診断を出し、Gameplay queue は drain して停止しません。

## 14. プロジェクト固有 Rust ゲームコード

### 14.1 Rust Game の初期化

新規プロジェクトでは自動初期化されます。既存データプロジェクトでは **Project → Initialize Rust Game** を選びます。

生成される `game/` は独立した `cdylib` Cargo project です。Engine の Rust layout や `World` 参照を DLL 境界へ渡さず、ABI v3 の deterministic JSON buffer と固定幅値だけを使用します。

### 14.2 Component／Resource／System を作成する

1. **Project → Create Rust Script…** を開きます。
2. Kind を Component、Resource、System から選びます。
3. Rust type name を UpperCamelCase の ASCII 英数字で入力します。
4. System の場合は Update または Fixed Update を選びます。
5. **Create** を押します。
6. 外部 editor で生成ファイルが開きます。
7. 生成直後に Game code build が自動 queue されます。

既定の作成場所（Asset Browser で `scripts/rust/` 以下のフォルダを選んでいるときは、そのフォルダに作られます）:

- Component: `assets/scripts/rust/components/<name>.rs`
- Resource: `assets/scripts/rust/resources/<name>.rs`
- System: `assets/scripts/rust/systems/<name>.rs`
- Shared Module: `assets/scripts/rust/shared/<name>.rs`

Component の `game.c_<ULID>`、Resource／System の dotted ID は永続 contract です。Rust type や file を rename／移動しても ID を変えないでください。

### 14.2.1 Rust スクリプトの配置ルール

`assets/scripts/rust/` 以下は自由配置です。上記 4 フォルダは推奨であり、作成後は任意の Rust サブフォルダへ移動できます。

- **フォルダ構造がそのまま Rust のモジュールパスになります。** `assets/scripts/rust/player/movement/move_rule.rs` は `crate::player::movement::move_rule::MoveRule` で参照します。
- **種類はソースコードの属性・derive から判定されます。** `GameComponent` derive は Component、`GameResource` derive は Resource、`#[engine::game_system(...)]` は System、どれも持たないファイルは通常の補助モジュールとしてコンパイルされます。フォルダ名では判定しません。
- **別フォルダに同名ファイルを置けます。** `player/health.rs` と `enemy/health.rs` は両立します。同じフォルダ内で同じモジュール名になる組み合わせだけがエラーです。
- **フォルダ名・ファイル名は Rust のモジュール名である必要があります。** `player` `player_movement` `player2` `_player` は可、`player-movement` `Player Movement` や日本語名は不可です。
- **`.rs` を `assets/scripts/rust/` の外へ移動することはできません。**
- **`assets/scripts/rust/` 以下に `mod.rs` を作らないでください。** モジュール索引はエンジンが `game/src/project_modules.rs` に自動生成します。`mod.rs` があるとエラーとして報告されます。
- **移動しても Component のメタデータ（`*.rs.meta.json`）と安定 ID は維持されます。** シーン・Prefab・Inspector の参照はそのままです。
- **移動時に `use` パスは自動更新されません。** モジュールパスが変わるため、`use crate::components::move_rule::MoveRule;` のような行は手で直す必要があります。移動後の Game code build が該当行を報告します。

### 14.3 Build

- **Build → Check Rust Game**: `cargo check` 相当の確認。
- **Build → Build Rust Game**: Editor Play で読み込む native module を build。
- Play の隣の **Build**: native Game module と Game Component schema を更新。
- **Cancel Build**: 実行中 build の中止。

Windows では DLL を generation ごとの shadow path へ copy して load するため、停止中なら元 DLL を再 build できます。Play 中は新 module generation に差し替えません。変更は Stop 後の次 Play で採用します。

Compiler diagnostics は Console と Problems の両方へ入り、source-linked 行を選ぶと `game/` 内を検証してから外部 editor で開きます。

### 14.4 System の access 宣言

各 System は、読み取る Query、書き込む Game Component／Resource、Input Action、Event stream、Command family を `GameSystemAccess` で明示します。未宣言データは入力へ含まれず、未許可 command は System 全体を失敗させます。

System callback は `GameInvocation` を読み、`GameInvocationOutput` に patch／deferred command を追加します。1 callback の出力は atomic です。1 件でも不正な patch、stale handle、未許可 command があれば、その callback の出力全体を適用しません。

詳しい API とコード例は [PROJECT_RUST_GAMEPLAY.md](PROJECT_RUST_GAMEPLAY.md) を参照してください。

### 14.5 主な GameModule 機能

- Game Component／Game Resource の read／write
- Transform、Despawn、Project Component の enable／disable／add／remove
- Character motion、Lock-on
- Animation 再生、停止、Crossfade、Condition
- UI binding と UI document visibility
- Scene switch
- SE／BGM／Volume
- Save set／remove／write／load
- Timer、Game event
- Hitbox、Damage、Knockback
- NavMesh target／state
- Behavior Tree action／condition result
- Runtime Prefab spawn と後続 result event

### 14.6 ABI の制限

- 1 invocation: 最大 16,384 query rows
- Event record: 最大 4,096
- Input／Output: 各最大 1 MiB
- Command: 最大 1,024
- Save request queue: 64
- Prefab spawn queue: 256
- Audio request queue: 256
- Event history: 4,096
- 古い ABI v1／v2 module は load せず、rebuild 診断を出す

## 15. Input Actions と Systems

### 15.1 対応入力名

Keyboard:

- `KeyA`～`KeyZ`
- `Digit0`～`Digit9`
- Arrow keys、`Space`、`Enter`、`Escape`、`Tab`
- Shift、Control 系

Mouse button:

- `Left`、`Right`、`Middle`、`Back`、`Forward`

Gamepad button index:

- 0～7 = South、East、West、North、Left shoulder、Right shoulder、Select、Start

Gamepad axis index:

- 0～5 = Left X、Left Y、Right X、Right Y、Left trigger、Right trigger

Gamepad axis は deadzone、invert、scale を Action ごとに設定できます。`key_axes` は negative／positive key を X または Y 成分へ合成できます。結果は `[-1, 1]` に clamp されます。

### 15.2 Input Debugger

1. Play を開始します。
2. Game View をクリックして focus します。
3. **View → Input Debugger** または下部 **Input** を開きます。
4. Physical keyboard／mouse、gamepad、resolved action の pressed／just pressed／value を確認します。

Window focus を失うと keyboard／mouse の held state を release します。Gamepad disconnect 時も安全側として gamepad state を release します。

### 15.3 Systems panel

1. **View → Systems** を開きます。
2. Update または FixedUpdate を選びます。
3. Search、Engine、Game filter で絞り込みます。
4. Checkbox で System を enable／disable します。
5. 矢印ボタンで順序を調整します。
6. `before`／`after` 制約を確認します。
7. **Reset to Default** で既定順へ戻します。

保存順は制約に反しない範囲で tie breaker になります。制約 cycle は Play 開始を block します。変更は `project_settings.json` へ atomic write され、次の Play で反映されます。

## 16. Play、デバッグ、フレームキャプチャ

### 16.1 Play の開始条件

Play 開始前に Scene validation、Asset validation、Game code stale check を行います。Game code が古い場合は Build 後に Play を続行します。Blocking diagnostic があると Play は開始しません。

### 16.2 Game View の操作

- Game View をクリックすると keyboard／mouse focus を取得します。
- Game View の外をクリックすると入力を release します。
- `Esc` はエディターの Stop Play にも使われるため、ゲーム内 Escape Action と競合する場合があります。
- **Debug Draw** で runtime debug line を表示します。
- **Capture Frame** で現在の Game View を PNG 保存します。

表示される統計:

- Runtime Entity 数
- Render frame 数
- Fixed step 数
- 経過秒
- Collision proxy／candidate pair／contact 数

### 16.3 Pause と Step

1. Play 中に **Pause** を押します。
2. 状態を確認します。
3. **Step** を押すと FixedUpdate 1 回と Update 1 回が実行されます。
4. **Resume** で通常進行へ戻ります。

Animation、Navigation、Collision、Behavior Tree の再現性確認に使用します。

### 16.4 Console と Problems

- Console は現在の Session log と一時的な runtime message を確認する場所です。
- Problems は修正が必要な持続的診断を確認する場所です。
- Error／Warning／Info で filter できます。
- 対象 Entity の診断を選ぶと Inspector 選択へ移動します。
- Game code の source 診断は外部 editor で該当ファイルを開きます。

## 17. Scene 切替、Save、Prefab

### 17.1 Runtime Scene 切替

Game code は `request_scene("scenes/result.scene.json")` のように project-relative Scene path を要求します。実際の切替は次の host frame boundary で行われます。

1. 新 Scene を read、parse、validate。
2. 成功後に旧 Scene 所属 Entity を despawn。
3. 新 Scene を spawn。
4. World Resource と asset cache は維持。

同じ current／pending Scene への重複要求は無視します。別 path の複数要求は最後の要求が優先です。Load 失敗時は可能な限り旧 Scene を維持します。Game code が実行時 spawn した非 Scene Entity は自動では消えないため、必要なら Scene generation 変化時に despawn してください。

### 17.2 Save

Save は schema version 1 の flat key-value document です。値は Text、Number、Flag。Slot file は `slot_<n>.save.json` です。

- Editor Play: `<project>/saves/`
- 配布版: OS local-data directory
- `GAMEENGINE_PORTABLE_SAVES` を明示設定した配布版: Package local `saves/`

書込みは atomic replacement です。壊れた Slot は metadata diagnostic として列挙し、他 Slot を隠したり crash したりしません。Save は tamper resistance を持たない人間可読 JSON です。

### 17.3 Prefab

Prefab は schema version 1 の `*.prefab.json` です。定義内 Entity ID は Prefab 内で安定していますが、Scene へ instantiate するたびに新 Entity ID へ remap されます。実行時 Entity ID は保存されません。

ライブラリ層には Create、Place、Inspect source、Apply、Revert、Unpack、Dependency traversal、Runtime spawn が実装されています。ただし現在のメインエディター UI にはこれらのメニュー／ドラッグ操作が接続されていません。project Rust の runtime spawn、または `engine-editor` の公開 API／テスト経路で使用してください。UI から通常操作できる機能としてはまだ扱わないでください。

Nested Prefab の dependency cycle は Package で拒否されます。Instance root の `editor.prefab_instance` marker は editor-only で、runtime bridge は無視します。

marker の `source` はプロジェクトルート相対パス（例: `assets/prefabs/hero.prefab.json`）で保存されます（ADR 0134）。Scene を別マシンや別チェックアウトへ移動しても壊れません。ADR 0134 より前に作られた Scene は絶対パスを持つことがあります。読み込みは互換維持されますが、そのマシンでしか解決できません。Inspector が該当 Instance に注意を表示するので、Revert（または再 Instantiate）して保存すると相対パスへ移行します。

## 18. 配布パッケージの作成

### 18.1 Player を先に Build する

エディターの Package は Player 自体を Cargo build しません。最初にリポジトリルートで実行します。

```powershell
cargo build -p engine --release --bin player
```

### 18.2 Package 操作

1. `project_settings.json` に有効な `start_scene` を設定します。
2. 全 Scene を保存します。
3. glTF など変更済み source を Reimport します。
4. **Build → Build Rust Game** が成功することを確認します。
5. **Play** で動作確認し、Stop します。
6. **Build → Package Project…** を選びます。
7. 空または上書きしてよい出力フォルダーを選択します。
8. Rust Game がある場合は release GameModule build 後に package されます。
9. Console／Problems で結果を確認します。

### 18.3 出力内容

```text
<output>/
  game.exe
  game_module.dll           # Project Rust がある場合。実名は platform contract に従う
  project.json
  project_settings.json
  asset_manifest.json
  assets/
    scenes/...
    <Manifest に登録されたアセットと glTF sidecar>
  build_report.json
  THIRD_PARTY_NOTICES.txt
```

出力には全 authored Scene を含め、runtime Scene switch 後の欠損を防ぎます。v1 は conservative policy のため、Manifest の未使用アセットも含みます。

### 18.4 Package が失敗する代表例

- Start Scene が未設定または存在しない
- Manifest の登録ファイルが欠損
- glTF の buffer／image sidecar が欠損
- glTF source が Reimport 後から変更され stale
- imported sub-asset ID が不正
- Prefab dependency cycle
- release Player が見つからない
- Rust Game の release build failure

Package は Windows の空白、日本語 path を想定した test があります。Cross-compilation は行わないため、対象 OS／Architecture の Player をその環境で build してください。

### 18.5 配布版のログと Portable mode

配布版の Save と startup log は既定で OS local-data directory を使用します。Package 削除で進行データが消えないための仕様です。

- `GAMEENGINE_PORTABLE_SAVES`: Save を Package local にする opt-in
- `GAMEENGINE_PORTABLE_LOGS`: Log を Package local にする opt-in

`build_report.json` には file list、Start Scene、GameModule 有無、Save location、debug symbol／crash policy が記録されます。Debug symbol は package に同梱しないため、symbolication が必要なら build artifact を保管してください。

## 19. CLI と AI Agent Bridge

Behavior Tree 以外に、AI Agent Bridge 用の CLI があります。

```powershell
cargo run -p engine-cli -- ai-agent describe-tools
cargo run -p engine-cli -- ai-agent validate-input <json>
cargo run -p engine-cli -- ai-agent inject-input <inbox_path> <json>
cargo run -p engine-cli -- ai-agent capture-frame <inbox_path>
```

入力 injection は OS key event を偽装せず、engine-owned VirtualInputQueue を通します。Frame capture も runtime の readback 境界を使用します。MCP は Behavior Tree schema／validate／compile／layout／apply と AI agent tool descriptor を薄く公開し、編集 semantics 自体は authoring core が所有します。

## 20. 永続データと安全規則

### 20.1 Stable ID

永続 ID は種類 prefix と ULID の組み合わせです。例:

```text
entity_<ULID>
asset_<ULID>
graph_<ULID>
node_<ULID>
```

Rename しても ID は変えません。Scene／Prefab／Graph 内に runtime Entity、RuntimeAssetId、GPU handle、process-local animation clip ID を保存しないでください。

### 20.2 JSON を手動編集するときの注意

- `schema_version` を削除／変更しない。
- Asset path は `assets/` 基準の相対 path にする。
- `..`、absolute path、symlink escape は拒否される。
- Asset reference は path 文字列ではなく `{ "$type": "asset_ref", "id": "asset_..." }` を使う。
- Entity reference は `{ "$type": "entity_ref", "id": "entity_..." }` を使う。
- Stable component ID、System ID、Action ID、Behavior ID を無断で rename しない。
- Manifest v1 は読めますが、保存時は v2 になります。
- 新しい build が書いた Manifest v2 は古い build では読めない場合があります。

### 20.3 Source control

通常は次を version control 対象にします。

- `project.json`
- `project_settings.json`
- `asset_manifest.json`
- `assets/`
- `game/Cargo.toml`、`game/Cargo.lock`、`game/src/`

通常は次を除外します。

- `game/target/`
- `game/.iroha/`
- `saves/`
- Package output

## 21. 描画とランタイムの上限

| 項目 | 上限／仕様 |
| --- | --- |
| Texture width／height | 8,192 px |
| Material texture slot | Base、Normal、Emissive の 3 |
| Skin joint | 128 |
| Directional Light | 1（複数は warning） |
| Ambient Light | 1（複数は warning） |
| Particle／Emitter | 65,536 |
| Particle spawn／Emitter／Frame | 256 |
| Scene worst-case render instances | 100,000 |
| Shadow map | Depth32Float、既定 2,048 px、2 cascades |
| HDR target | Rgba16Float |

Device capability により実際の quality が低くなる場合はありますが、Device ごとに Scene semantics の上限が暗黙に増えることはありません。

## 22. 既知の制限事項

1. **Project Settings の専用 UI は未接続**: Tags、Layers、Input Actions、Start Scene は外部 editor で JSON 編集が必要です。

2. **Prefab workflow の main UI は未接続**: Core API と runtime 経路はありますが、Create／Apply／Revert／Unpack の menu は表示されません。

3. **glTF は部分完成扱い**: Register／background import／Reimport のコード経路はありますが、公式 reachability では code-only／partial 評価が残っています。

4. **Scene View Gizmo は Move 中心**: Rotate／Scale mode は表示されますが、直接操作は Inspector 併用を前提にしてください。

5. **Editor Game View の key forwarding は限定的**: 現在明示転送される key は `W/A/S/D`、Space、Arrow keys です。Action 全体の検証は Gamepad または配布 Player を使用してください。

6. **FBX は直接 import 対応（ブレンドシェイプ等は非対応）**: `ufbx` 経由で mesh／skin／animation を直接 Register できます（ADR 0081）。ブレンドシェイプや NURBS など未対応の機能を使うソースのみ glTF／GLB へ外部変換してください。

7. **WASM は完成した実行対象ではない**: Build compatibility の足場はありますが、HTTP asset load、完成した browser GPU initialization、web package は未完成です。Native GameModule も desktop-only です。

8. **NavMesh は grid／planar arena 向け**: Dynamic obstacle carving、本格 crowd、多層 navigation はありません。

9. **Physics は Primitive 中心**: Mesh Collider と外部 rigid-body engine 相当の全面機能はありません。

10. **Package は folder 配布**: Asset embedding、installer、unused asset stripping、cross compile はありません。

## 23. トラブルシューティング

### エディターが起動しない

- `cargo --version` を確認する。
- `cargo run -p engine-editor` で詳細 error を見る。
- GPU driver を更新する。
- 既に別 build が lock していないか確認する。

### Project を開けない

- 選択した folder 直下に `project.json` があるか確認する。
- JSON の `schema_version` が対応 version か確認する。
- Folder ではなく `assets/` を選んでいないか確認する。

### Scene が表示されない

- `start_scene` の path を確認する。
- Scene に Camera と renderable Entity があるか確認する。
- Mesh／Material が Manifest に登録されているか確認する。
- Problems の missing asset／invalid component を確認する。

### Game Component が Add Component に出ない

- `game/Cargo.toml` があるか確認する。
- **Build → Build Rust Game** を実行する。
- Problems で compiler error／ABI mismatch を確認する。
- Play 中なら Stop してから再 build する。

### 入力が効かない

- Game View をクリックして focus を与える。
- Input Debugger で physical state と resolved action を見る。
- `project_settings.json` の key 名を確認する。
- Gamepad backend warning／disconnect を確認する。
- `F`、`Q`、Enter などは Editor Game View の既知の転送制限を確認する。

### NavMesh Agent が動かない

- Scene に NavMesh Surface が 1 個あるか確認する。
- Bake 後に Scene を Save したか確認する。
- Bake が stale でないか確認する。
- Agent status が Missing／NoPath になっていないか確認する。
- Target が walkable grid 内か確認する。

### Package が作れない

- release Player を先に build する。
- Start Scene を設定する。
- 全 Scene と Manifest asset が存在するか確認する。
- glTF を Reimport する。
- Rust Game の release build error を Problems で確認する。

### 配布版の Save が Package folder に見つからない

正常です。既定では OS local-data directory にあります。Portable save が必要なら起動前に `GAMEENGINE_PORTABLE_SAVES` を設定します。

## 24. 制作ワークフロー

- Hierarchy は Ctrl で追加・解除、Shift で表示範囲を選択できます。複製、オフセット、整列、均等配置、Component の追加・削除は選択全体を 1 回の Undo として処理します。
- Prefab instance を選ぶと Inspector に source badge と **Open Prefab / Apply / Revert / Unpack** が表示されます。初期実装は instance 全体の Apply／Revert です。
- UI Builder の Responsive Document で reference resolution、scale policy、safe area を設定します。Desktop、Ultrawide、Handheld、Portrait、Custom の各 preview を切り替えられます。
- Assets dock は左の folder tree と右の内容一覧を使います。folder 作成、rename、trash、複数 asset の folder drop は事前検証後にまとめて実行されます。
- Project component の Inspector header から明示的に Rust source を開けます。Built-in component は SDK と同じ版の source を editor 内で read-only 表示します。
- **Project → Editor Preferences** で external editor と `{path}`、`{line}`、`{column}` の引数 template を設定します。Rust script 作成だけでは外部 application を起動しません。
- **Project → Open Project Terminal** は `game/` を作業 directory として SDK 設定済み PowerShell を開きます。別 terminal では **Copy Cargo Command** で `cargo check --all-targets` を取得できます。
- Game View の **Record / Stop Recording / Replay / Load Replay / Save Replay** は OS key injection を使わず、engine virtual input を fixed tick ごとに記録します。
- 通常起動は最後の project と document を復元します。Shift を押した起動、または `GAMEENGINE_SAFE_START` を設定した起動は復元を回避します。

## 25. 完成確認チェックリスト

ゲームを「完成」と判断する前に、最低限次を確認してください。

- Project Hub からプロジェクトを開ける。
- Start Scene と全遷移先 Scene が開ける。
- Problems に blocking error がない。
- Game Rust の Check／Build が成功する。
- Scene View、Editor Play、Package Player で主要な見た目が一致する。
- Keyboard と Gamepad の Input Action を Input Debugger で確認した。
- 30／60／120 FPS 相当で Collision／Combat の FixedUpdate 結果が破綻しない。
- NavMesh path、Behavior Tree、Animation event が期待順に動く。
- Pause／Step で再現可能に確認できる。
- Save の write／load／corrupt slot 処理を確認した。
- Scene switch 後に不要な runtime-spawn Entity が残らない。
- Package の `build_report.json` と `THIRD_PARTY_NOTICES.txt` を確認した。
- 空白または日本語を含む path で必要な動作を確認した。
- Package を別 folder から起動して Start Scene、Asset、GameModule、Save、Log を確認した。

## 26. 関連資料

- [AI-Friendly Authoring 仕様](AI_FRIENDLY_AUTHORING_SPEC.md)
- [Rust コード規約](RUST_CODE_STYLE.md)
- [Project Rust Gameplay API](PROJECT_RUST_GAMEPLAY.md)
- [Runtime Host Profile](RUNTIME_HOST_PROFILE.md)
- [ECS System Scheduling](SYSTEM_SCHEDULING.md)
- [Renderer Limits](RENDERER_LIMITS.md)
- [FBX 変換手順](FBX_IMPORT.md)
- [Architecture Decision Records](adr/README.md)
- [Editor Feature Reachability](editor_feature_reachability.json)
