# Agent Workflow Guide

## 1. タスクルーティングテーブル

作業するファイルの場所でグループを判定し、そのグループのドキュメントだけを読む。

| グループ | 作業場所 | 読む文書 |
|---------|---------|---------|
| **G1 Authoring** | `crates/authoring/` `crates/cli/` `crates/mcp/` | `AI_FRIENDLY_AUTHORING_SPEC.md` + ADR 0020, 0021, 0023 参照 |
| **G2 Editor** | `crates/editor/` | `docs/phases/phase-09-project-workflow.md` `docs/phases/phase-10-game-view.md` + ADR 0016, 0017, 0018, 0022, 0024。Phase 15〜17 の作業は `docs/phases/phase-15-component-registry.md` / `phase-16-asset-editor-integration.md` / `phase-17-scene-editing.md` + ADR 0025, 0027 も読む。Phase 26〜40 の作業は `docs/phases/phase-N-*.md`（該当 Phase）+ ADR 0028 も読む |
| **G3 Runtime Rendering** | `crates/engine/src/render.rs` `mesh.rs` `shaders/` `crates/renderer/` | `docs/phases/phase-11-rendering.md` |
| **G3 Runtime Systems** | `crates/engine/src/` (`collider` `physics` `audio` `ui` 等の新規ファイル) | 対応する `docs/phases/phase-N-*.md`（Collision=21 / Physics=22 / Audio=23 / Runtime UI=24） |
| **G4 Asset & Scene** | `crates/engine/src/asset.rs` `scene_bridge.rs` | `docs/phases/phase-14-asset-pipeline.md` + ADR 0021 参照。component spawn の登録は ADR 0027 / `docs/phases/phase-15-component-registry.md` |
| **G4 Asset & Scene** | `crates/engine/src/scene_loader.rs` | Phase 18 対象ファイル。`docs/phases/phase-18-runtime-scene-loading.md` + `AI_FRIENDLY_AUTHORING_SPEC.md` §5.2 のみ |
| **G5 Sample Project** | `examples/coin_collision_loop/` | `docs/phases/phase-20-sample-project.md` |

> **注**: 複数グループにまたがる変更（例: `Vertex` 変更で `engine` と `renderer` 両方が変わる）は
> 破壊的変更プロトコル（§3）に従う。

---

## 2. 標準ワークフロー（全タスク共通）

```
1. CLAUDE.md を読む（エントリポイント）
2. 上のテーブルでグループを判定し、該当 phase ドキュメントを読む
3. Grep / Glob で関連ファイルを特定する（ディレクトリ全体の丸読みは禁止）
4. 実装する
5. 検証コマンドを実行して完了
```

### ファイル検索のルール

- ディレクトリを `ls` で丸読みしない。`Grep` で型名・関数名を検索する。
- 大きなファイルを読むときは、関係するセクションの見出し付近だけ読む。
- 同一セッション内で既に読んだファイルは再読しない。

---

## 3. 破壊的変更プロトコル

**破壊的変更の定義**: 2つ以上のクレートで同時にコードを変更しなければならない修正。

### 典型例

| 変更 | 影響クレート |
|-----|------------|
| `Vertex` にフィールド追加 | `engine`（mesh.rs）, `engine`（render.rs, shaders/mesh.wgsl）|
| `spawn_from_authoring_scene` のシグネチャ変更 | `engine`, `editor` |
| `AuthoringScene` の公開 API 変更 | `authoring`, `cli`, `mcp`, `editor` |
| `ComponentTypeId` のフォーマット変更 | `authoring` + 全シリアライズ済みファイル |

### 手順

1. **着手前**: 影響を受けるクレートをすべて列挙してコメントに書く
2. **同一 PR**: 全呼び出し元の更新を同じ PR に含める。分割しない
3. **テスト**: `cargo test --workspace` がパスするまで完了と言わない
4. **シリアライズ変更**: フォーマットが変わる場合は移行テストを書く
5. **記録**: PR 説明に「破壊的変更」と明記し、影響範囲を書く

---

## 4. 意思決定の記録ルール

| 決定の種類 | 記録場所 |
|-----------|---------|
| クレート境界をまたぐ設計決定 | `docs/adr/NNNN-*.md`（ADR テンプレートを使う） |
| 単一クレート内の判断 | PR の説明文 |
| フェーズの実装詳細の変更 | 対応する `docs/phases/phase-N-*.md` を更新 |
| 「なぜそうしなかったか」の記録 | 各 phase ドキュメントの Design Decisions セクション |

### ADR を書くべき状況

- クレートの依存関係を新たに追加する
- 既存の公開 API を削除・変更する
- シリアライズフォーマットを変更する
- 複数のアプローチを比較検討した結果として方針を決める

---

## 5. フェーズドキュメント一覧

各フェーズドキュメントには **Goal / Why / Scope / Design Decisions / Implementation Plan /
Cautions / Prohibited / Completion Criteria / Feeds Into** が書かれている。

> **実装順序・依存グラフの正規ソースは `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md`**。
> phase ドキュメントと実装計画に矛盾があるときは計画を優先する。

> **2026-06-13 再構成**: Phase 15〜20 を「最低限エディタから使えるエンジン」
> 方針で再構成した。旧→新対応表は `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md`
> の Phase 15 直前を参照。

> **2026-06-14 Phase 41+ 修正**: 旧 Phase 41: Consolidation は番号付き Phase ではなく
> Roadmap consolidation prerequisite として扱う。新 Phase 41 以降の番号は
> `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` の Phase 41-47 対応表を正とする。

| フェーズ | ドキュメント | 状態 |
|---------|------------|------|
| Phase 9  | `docs/phases/phase-09-project-workflow.md` | 完了 |
| Phase 10 | `docs/phases/phase-10-game-view.md` | 完了 |
| Phase 11 | `docs/phases/phase-11-rendering.md` | 完了 |
| Phase 12 | `docs/phases/phase-12-runtime-foundation.md` | 一部完了（12-A/C/F 完了。12-B は Phase 21 先頭へ、12-D/E は先送り） |
| Phase 13 | `docs/phases/phase-13-vertical-slice.md` | 完了 |
| Phase 14 | `docs/phases/phase-14-asset-pipeline.md` | 完了（editor 統合の残課題は Phase 16 へ繰越） |
| Phase 15 | `docs/phases/phase-15-component-registry.md` | 完了（ADR 0027 Accepted・15-B/C/D/E すべて実装済み） |
| Phase 16 | `docs/phases/phase-16-asset-editor-integration.md` | 完了（2026-07-05 監査で 16-B Register Asset・16-C manifest picker・16-E 統合テスト（runtime.rs の temp project + OBJ テスト）も実装済みと確認） |
| Phase 17 | `docs/phases/phase-17-scene-editing.md` | 未着手（手動チェックリスト 17-D の実行が必要） |
| Phase 18 | `docs/phases/phase-18-runtime-scene-loading.md` | 実質完了（18-A SceneLoader・18-B reload_from_path 実装済み。18-C SceneManager は任意スコープ） |
| Phase 19 | `docs/phases/phase-19-minimal-runtime.md` | 一部完了（19-A PlayerController・19-B OrbitCamera/FollowCamera 実装済み。19-C BT 検証・19-D debug draw 確認は未済） |
| Phase 20 | `docs/phases/phase-20-sample-project.md` | 一部完了（同梱プロジェクトは `examples/coin_collision_loop` のみ。ADR 0091 で他のサンプルは削除済み） |
| Phase 21 | `docs/phases/phase-21-collision.md` | 完了（先頭タスク = 12-B Fixed Timestep） |
| Phase 22 | `docs/phases/phase-22-physics.md` | 完了 |
| Phase 23 | `docs/phases/phase-23-audio.md` | 完了 |
| Phase 24 | `docs/phases/phase-24-runtime-ui.md` | 完了 |
| Phase 25 | `docs/phases/phase-25-full-sample-game.md` | 完了 |
| Phase 26 | `docs/phases/phase-26-project-hub.md` | 完了（preferences.rs / hub.rs / EditorPreferences・HubAction） |
| Phase 27 | `docs/phases/phase-27-scene-view.md` | 完了（scene_view.rs / SceneView・EditorViewCamera・offscreen render） |
| Phase 28 | `docs/phases/phase-28-scene-picking.md` | 完了（ray-AABB picking in SceneView::pick） |
| Phase 29 | `docs/phases/phase-29-transform-gizmo.md` | 完了（gizmo.rs / duplicate_scene_entity / Ctrl+D/C/V / T/R/S gizmo keys） |
| Phase 30 | `docs/phases/phase-30-console-problems.md` | 完了（console.rs / problems.rs / validation.rs / ConsolePanel・ProblemsPanel） |
| Phase 31 | `docs/phases/phase-31-asset-database-v2.md` | 完了 |
| Phase 32 | `docs/phases/phase-32-drag-drop.md` | 完了 |
| Phase 33 | `docs/phases/phase-33-prefab.md` | 完了 |
| Phase 34 | `docs/phases/phase-34-project-settings.md` | 完了 |
| Phase 35 | `docs/phases/phase-35-material-lighting.md` | 完了 |
| Phase 36 | `docs/phases/phase-36-gltf-pipeline.md` | 完了 |
| Phase 37 | `docs/phases/phase-37-animation-runtime.md` | 完了 |
| Phase 38 | `docs/phases/phase-38-animation-authoring.md` | 完了 |
| Phase 39 | `docs/phases/phase-39-build-packaging.md` | 完了 |
| Phase 40 | `docs/phases/phase-40-ai-agent-bridge.md` | 完了 |
| Pre-Phase | `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` | Roadmap consolidation prerequisite（Phase 15〜20 ギャップ監査・docs/ADR整合・fmt/clippy/test/doc確認）。正式 Phase 番号は消費しない |
| Phase 41 | `docs/phases/phase-41-shadow-ibl.md` | 完了（ADR 0036。shadow / environment lighting contracts 実装済み） |
| Phase 42 | `docs/phases/phase-42-scripting.md` | 完了（ADR 0037。Rhai runtime・`ScriptComponent` lifecycle・profiler 実装済み） |
| Phase 43 | `docs/phases/phase-43-gamepad.md` | 完了（ADR 0038。gilrs desktop + WASM Web Gamepad stub 実装済み） |
| Phase 44 | `docs/phases/phase-44-navmesh.md` | 完了（ADR 0039。grid-backed A* navmesh 実装済み） |
| Phase 45 | `docs/phases/phase-45-postprocess.md` | 完了（ADR 0040。HDR + tone mapping + bloom 実装済み） |
| Phase 46 | `docs/phases/phase-46-wasm.md` | 完了（ADR 0041。`wasm32-unknown-unknown` cargo check が通る。実行時 Web 対応は未了） |
| Phase 47 | `docs/phases/phase-47-instancing-lod.md` | 完了（ADR 0042。GPU instancing・`LodGroup`/`LodLevel` 実装済み） |
| Phase 48 | `docs/phases/phase-48-skinned-mesh.md` | 実装完了（ADR 0043。skinning.rs / mesh_skinned.wgsl / glTF skin+animation import / `example skinned_mesh`。起動確認済み・見た目の目視確認のみ未実施） |
| Phase 49 | `docs/phases/phase-49-particles.md` | 実装完了（ADR 0044。particles.rs / render.rs 収集パス / `example particles`。起動確認済み・見た目の目視確認のみ未実施） |
| Phase 50 | `docs/phases/phase-50-shadow-pass.md` | 実装完了（ADR 0036 の GPU 実装。2 カスケード depth-only パス / shadow_depth.wgsl / mesh.wgsl・mesh_skinned.wgsl の comparison sampling。起動確認済み・見た目の目視確認のみ未実施） |
| Phase 51 | `docs/phases/phase-51-packaging.md` | 実装完了（ADR 0045。`player` バイナリ + `plan_package`/`package_project`） |
| Phase 52 | `docs/phases/phase-52-particle-authoring.md` | 実装完了（新 ADR 不要・ADR 0027 + 0044 に従う。`engine.particle_emitter` を builtin registry の 11 番目として追加。エディタ/シーン/パッケージからパーティクル使用可能） |
| Phase 53 | `docs/phases/phase-53-ui-foundation.md` | 実装完了（ADR 0046 Accepted。authoring `ui` モジュール + engine `ui_document.rs` インタープリタ + `UiBindings`/`UiEvents` + `App::add_ui_font`。`*.ui.json` は schema_version 付き永続フォーマット） |
| Phase 54 | `docs/phases/phase-54-ui-interactivity.md` | 実装完了（`UiEventFrame` リレー + Rhai `on_event` ディスパッチ + `engine.ui_document`（registry 12 番目・built-in UI アセット）+ mtime ホットリロード。`engine_ecs` に `Option<ResMut<T>>` SystemParam 追加） |
| Phase 55 | `docs/phases/phase-55-scene-management.md` | 実装完了（ADR 0047。`SceneManager`/`SceneSwitchState` + `App::process_scene_requests`。実行時シーン切替・load-then-despawn・リソース永続。editor Play / player 両ホスト配線済み） |
| Phase 56 | `docs/phases/phase-56-save-load.md` | 実装完了（ADR 0048。engine `save.rs` = `SaveData`/`SaveStore`（`*.save.json` v1・atomic write・slot 管理）+ Rhai `save_get/save_set/save_write/save_load`。player = package root/saves・editor Play = project root/saves） |
| Phase 57 | `docs/phases/phase-57-action-collision.md` | 実装完了（Sphere/CapsuleY 形状 + 全ペア push-out・`CollisionLayers`・`TriggerVolume`・`KinematicCharacterController`・authoring 3 種（registry 13〜15 番目）・Rhai `ctx.collisions()`。controller 同士のすり抜けは既知の制限） |
| Phase 58 | `docs/phases/phase-58-targeting-camera.md` | 実装完了（`LockOnTarget`/`TargetLock`/`lock_on_system` + `LockOnCamera` + 壁衝突回避（線分 vs Static 外接 AABB の slab 法）。authoring 2 種で registry 17 件） |
| Phase 59〜63 | `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` の M1 セクション | 計画済み（2026-07-11 策定。M1 = バスターズ級アクション RPG 制作可能ライン。各 phase doc は着手時に作成） |

---

## 6. 完了済みフェーズの要点（参照用）

Phase 0〜8 の完成済みシステムについて、触る可能性がある箇所のみ記載。

### Authoring System（Phase 0〜8）

- `AuthoringCommand` / `Transaction` / `AuthoringScene` は `crates/authoring` が所有
- CLI と MCP は薄いアダプター。ビジネスロジックを持たない
- `crates/ecs` は `authoring` に依存してはならない（単方向）
- `StableId` のフォーマットは `<prefix>_<ULID>` で固定。変えてはならない

### 現在の Editor（Phase 10 完了）

- egui + eframe ベース
- Behavior Tree のグラフキャンバス実装済み
- undo/redo: JSON スナップショット方式（ADR 0018、上限 100 ステップ）実装済み
- ファイル保存（ADR 0022 分離形式: `*.scene.json` / `*.graph.json` + `*.graph.view.json`）実装済み（ADR 0019 legacy 読み込みは 2026-06-11 撤去済み）
- Scene Hierarchy・Entity Inspector・Project 管理・Asset Browser 実装済み（Phase 9）
- Play/Stop・Game View（オフスクリーン描画 + PNG キャプチャ）実装済み（Phase 10）
- `EditorSession` がグラフ状態・undo/redo・カレントドキュメントを管理

### Runtime Engine の現状

- 深度バッファ: **実装済み**（`render.rs` に `DepthStencilState` あり）
- `Time` / `Input<KeyCode>` / `Input<MouseButton>` / `MouseInput`: **実装済み**
- `spawn_from_authoring_scene()`: **実装済み**（`scene_bridge.rs`。`AssetManifest` / `AssetServer` を world resource から読む manifest 解決も実装済み・Phase 14-C/D）
- `Vertex` は `position` / `normal` / `color` / `uv` の 4 フィールド（11-D-1 で `normal` 追加済み。構成変更は破壊的変更プロトコル対象）
- 親子 Transform 伝播: **実装済み**（2026-07-04。`transform.rs` の `transform_propagation_system` が `Parent` 階層を解決。`spawn_from_authoring_scene` が authoring の parent から `Parent`/`Children` を付与）
- スキンドメッシュ/スケルタルアニメーション: **実装済み**（2026-07-04・Phase 48・ADR 0043。`skinning.rs`、`mesh_skinned.wgsl`、glTF skin+animation インポート。2026-07-26・ADR 0086 で `Skeleton`（スケルトンアセット全骨のリグ姿勢・`spawn_rig`）と `SkinnedMesh`（描画メッシュごとの Skin バインディング。skin ジョイント順・逆バインド行列を所有）に分離。1 つのリグを複数 Skin が共有できる）
- Skinned Model オーサリング: **実装済み**（2026-07-26・ADR 0087。`engine.skinned_model` が Skeleton サブアセットからリグを生成し、各 `engine.skinned_mesh_renderer` の任意 `model` EntityRef が使用する Skinned Model を明示。Model Inspector の Renderer 一覧は逆引き・読み取り専用。`engine.animation_controller` v4 は同一 Entity の Skinned Model のリグを使う。旧 `skeleton`／`rig_source` 値は読み込み互換。変換は `model_migration.rs` + エディタの Project メニュー）
- ボーンアタッチメント: **実装済み**（2026-07-26・ADR 0088。`engine.bone_attachment`（rig EntityRef + BoneId + 表示用 bone_name）。変換の後段パスで対象 joint Entity へ親子付けし直すため毎フレームのシステムは不要。ゲームコード向けの任意ボーン照会 API は提供せず、アタッチした Entity の Transform ビューを読む）
- パーティクル: **実装済み**（2026-07-05・Phase 49・ADR 0044。`particles.rs` の `ParticleEmitter`/`particle_update_system`。描画は既存 instanced パイプラインに合流・エンティティ非生成・xorshift で依存追加なし。2026-07-11・Phase 52 で `engine.particle_emitter` として authoring 可能化 — builtin registry 登録・`spawn_particle_emitter_component`・mesh は AssetRef フィールド）
- ディレクショナルシャドウ: **実装済み**（2026-07-05・Phase 50・ADR 0036 の GPU 実装。2 カスケード・`cascade_view_projections`（shadow.rs）・depth-only パス・light bind group(2) に shadow uniform/depth array/comparison sampler を追加。スキンドメッシュのキャストは 50-D の `shadow_depth_skinned.wgsl`・3x3 PCF は 50-E で対応済み）
- `lod_selection_system`・`particle_update_system`・`joint_palette_system` は `App::new` が自動登録。`animation_system` と物理系は opt-in（`App::add_fixed_system`）
- パッケージング: **実装済み**（2026-07-05・Phase 51・ADR 0045。`crates/engine/src/bin/player.rs` の汎用 player + editor `build.rs` の `plan_package`/`package_project`。packaging では MissingAsset が blocking）
- `Mesh::cube()` / `plane()` / `sphere()` / OBJ ロード（tobj）: **実装済み**（11-B / 14-B）
- `PlayerController` / `OrbitCamera` / `FollowCamera` / BT 接続: **実装済み**（13-A/B/D）。Phase 15 の component registry により editor から追加可能。
- `RuntimePlayState::start_with_project(...)`: **実装済み**（Phase 16-D）。`AssetServer` / `AssetManifest` を world に挿入し、manifest 登録済み mesh を editor Play に反映できる。
