# エディタ UX フィードバック調査・改善計画（2026-07-20）

> 当時のスナップショットです。本文が挙げる `engine.mesh` / `engine.material` /
> `engine.skeleton` / `engine.animator` / `engine.animation_graph_player` は
> ADR 0091 で削除済みで、現行の authoring 形は Static/Skinned Mesh Renderer と
> Skinned Model + Animation Controller です。

Status: Implemented（2026-07-20 実装完了。全27実装項目 + 回答2件。
検証: fmt / clippy -D warnings / workspace tests / rustdoc 全通過。
残タスク: 7-4 sprite コンポーネント・11-C HDRI skybox・20-3 サイクル選択・
9-5 ツリードラッグ並べ替え・19 mesh サムネイルは将来課題として据え置き）
対象: `crates/editor/`（一部 `crates/engine/`・`crates/authoring/`）

> **注（2026-07-20）**: 本書作成中に `crates/editor/src/ui/mod.rs` が
> `ui/{assets,chrome,documents,game_tools,hierarchy,inspector,play,viewport}.rs`
> へ分割された。項目 1〜19 の `ui/mod.rs:行番号` 参照は**分割前**のものであり、
> 現在は対応する分割先ファイルを検索して読み替えること
> （例: Add Component ピッカー → `ui/inspector.rs`、Hierarchy → `ui/hierarchy.rs`、
> Scene View ドロップ → `ui/viewport.rs`）。項目 20 以降は分割後の参照。

ユーザーテストで挙がった 11 件のフィードバックについて、コードを調査して原因を特定し、
実装方針をまとめる。各項目は独立して着手できる。優先度の目安を末尾に記載。

---

## 1. ギズモがつかみづらい

### 現象
移動/回転/スケールギズモの軸をドラッグしようとしても掴めないことが多い。

### 原因（特定済み）
`crates/editor/src/scene_view.rs:1299` の `hit_test_gizmo_axis` は、
各軸の**先端 1 点**（`center + dir * 1.5` をスクリーン投影した点）から
半径 14px の円だけを判定している。軸の線分上のどこを押しても反応しない。

さらに軸長 `LEN = 1.5` はワールド単位固定のため、カメラが離れると
ギズモ全体が画面上で小さくなり、有効な判定領域はさらに縮む。
ホバー時のハイライトも無く（`draw_gizmo` は固定色の線を描くだけ）、
「どこを掴めるのか」の視覚的な手がかりが無い。

### 実装方針
1. 判定を「点との距離」から「線分（center→先端）とのスクリーン空間距離」に変更する。
   しきい値は 8〜10px 程度。先端は現状どおり優先判定（軸が重なった場合の解決順も定義する）。
2. ギズモをスクリーン空間で一定サイズにする。カメラとの距離 `d` に応じて
   `LEN = k * d`（k は視野角から算出）とし、near/far どちらでも同じ画面サイズで描画・判定する。
3. `hit_test_gizmo_axis` を描画前にも呼び、ホバー中の軸を明色・太線でハイライトする。
4. 先端に矢印（translate）/ 立方体（scale）ハンドルを描き、掴む場所を明示する。

### 影響範囲
`scene_view.rs` のみ。判定関数は純関数なので単体テストを追加する。規模: 小〜中。

---

## 2. Add Component をカテゴリ分けしたい

### 現象
Add Component ピッカーに 32 個の builtin コンポーネントが同列に並び、目的の物を探しづらい。

### 原因（特定済み）
`ComponentSchema` には既に `category` フィールドが存在する
（`crates/engine/src/components.rs`。現状の内訳: Engine 15 / Rendering 5 /
Audio 3 / Animation 2 / Gameplay 2 / Navigation 2 / AI 1）。
しかし Add Component ピッカー（`crates/editor/src/ui/mod.rs:6543` 付近）は
`"{source} / {display_name}"` のフラットなボタン列を出すだけで `category` を使っていない。

### 実装方針
1. ピッカーを `schema.category` ごとの見出し付きグループ（`ui.collapsing` または
   セクションラベル）で表示する。検索文字列が入力されている間は現状のフラット表示のままとする。
2. "Engine" が 15 個の雑多な受け皿になっているため、category 値を再割り当てする
   （例: Core / Physics / Scripting / Camera など）。`category` は表示専用メタデータで
   あり、シーンのシリアライズ形式・`ComponentTypeId` には一切影響しない。
3. Game モジュール由来のコンポーネントは従来どおり "Game" グループにまとめる
   （game module の schema export に category が含まれるかを実装時に確認。
   含まれない場合は "Game" 固定で開始してよい）。

### 影響範囲
`engine/components.rs` の category 文字列 + editor ピッカーの描画のみ。規模: 小。

---

## 3. コンポーネントが追加できない時がある

### 現象
Add Component でボタンを押しても何も追加されない（ように見える）ことがある。

### 原因（複合・特定済み）
1. **複数選択との不整合（主原因）**: ピッカーの候補は「Inspector に表示中の entity が
   まだ持っていない物」だけでフィルタされる（`ui/mod.rs:6516-6528`）が、
   実際の追加は `selected_scene_ids()` の**全 entity へ atomic に**適用される
   （`ui/mod.rs:6555`、`session.rs:1028`）。選択中の他の entity が 1 体でも
   その component を既に持っていると、`component.already_exists`
   （`crates/authoring/src/transaction.rs:532`）が blocking error になり
   **トランザクション全体がロールバック**され、何も追加されない。
   なお Remove 側は既に「持っている entity のみ」にフィルタしてから実行しており
   （`ui/mod.rs:6494-6503`）、Add 側だけ対称性が欠けている。
2. **選択残留バグ（項目 8）との複合**: Scene View クリックで選択しても古い複数選択が
   `selected_entities` に残るため、ユーザーは 1 体選択のつもりでも 1. を踏む。
3. **失敗が見えない**: 失敗は `apply_ui_result`（`ui/mod.rs:6850`）が Console 用
   diagnostic を積むだけで、Console タブを開いていなければ無反応に見える。
4. Game コンポーネントはビルド済み module が無いと一覧に出ない（これは仕様であり
   Inspector に説明とビルドボタンが表示される。`show_game_component_build_status`）。

### 実装方針
1. Add 時も Remove と対称に、「まだその component を持っていない entity のみ」へ
   targets をフィルタしてからコマンドを発行する。
2. 項目 8 の選択同期修正を先に入れる。
3. それでも失敗した場合は toast 通知（`push_notification`）も出す（項目 4 の統合と同時に）。
4. 回帰テスト: 「片方が既に持つ 2 体選択で Add → 持っていない方にだけ追加される」。

### 影響範囲
`ui/mod.rs` のピッカー処理のみ（session API 変更不要）。規模: 小。

---

## 4. エラーをコンソールにも出してコピペ可能にしたい

### 現象
エラーがその場に一瞬出るだけで、後から確認・コピーできない。

### 原因（特定済み）
エラー表示経路が 3 系統に分裂している。

| 経路 | 行き先 | 問題 |
|------|--------|------|
| `session.push_diagnostic` | Console / Problems パネル | 残るがコピー不可 |
| `push_notification`（`ui/mod.rs:1150`） | toast。**6 秒で消滅** | Console に残らない |
| その場の `ui.colored_label(RED, ...)`（`material_editor.rs:213` ほか多数） | パネル内表示のみ | Console に残らない |

さらに Console 自体も各行が `egui::Label`（`console.rs:92`）でありテキスト選択不可。

### 実装方針
1. `EditorApp` に一元ヘルパー `report_error(code, message)` を追加し、
   (a) toast 表示、(b) `session.push_diagnostic`、(c) `log::error!`（ターミナル stdout にも
   同文言が出てコピー可能）の 3 つを必ず同時に行う。
   `notify_asset_error` などの既存呼び出しをこれに寄せ、`colored_label` 系のうち
   「操作失敗」を表すもの（フィールド検証の常時表示エラーは除く）を順次移行する。
2. Console のコピー対応:
   - 各行の右クリックメニューに「Copy message」
   - パネル上部に「Copy all (filtered)」ボタン（表示中の全行を整形してクリップボードへ）
   - あわせて Label を selectable にする
3. diagnostic の `code` は既存の `editor.*` 命名を踏襲する。

### 影響範囲
`console.rs` + `ui/mod.rs` + 各パネルの呼び出し箇所の洗い出し。規模: 中。

---

## 5. シーンが作れない

### 現象
新しいシーンを作る方法が無い。

### 原因（特定済み）
File メニュー（`ui/mod.rs:1694-1725`）には
New Project / Open Project / Open Document / Save / Save As しか無く、
**「New Scene」が存在しない**。シーンはプロジェクト作成時に
`scenes/main.scene.json` が 1 つ生成される（`ui/mod.rs:7225`）だけで、
2 つ目以降を作る UI 導線が無い。既存シーンを Save As で複製する回避策しかなく、
空のシーンから始める手段が無い。

### 実装方針
1. **File > New Scene**: 現在のドキュメントが dirty なら保存確認 → 空の
   `AuthoringScene` を current document（パス未設定）として開く。保存時の既定
   ファイル名は既存の `default_save_file_name`（`untitled.scene.json`）を流用し、
   既定ディレクトリを `assets/scenes/` にする。
2. **Asset Browser の右クリックに「New Scene」**: 「New Folder」（`ui/mod.rs:7669` 付近）
   と並べて追加。その場で `<name>.scene.json`（`{"schema_version":1,"entities":[]}`）を
   書き出し、自動で開く。
3. Play 中は無効化（既存の Save 系と同じ gating）。

### 影響範囲
`ui/mod.rs` のみ。シーンフォーマットには触れない。規模: 小。

---

## 6. Play を押したときにビルド中であることが分かりづらい

### 現象
Play を押しても何も起きない（ように見える）時間が続く。

### 原因（特定済み）
game code が stale の場合、`start_play`（`ui/mod.rs:6951`）は
`play_after_game_build = true` を立てて cargo ビルドを開始し、**黙って return** する。
ビルド完了時に自動で Play が始まる（`ui/mod.rs:4190-4195`）が、その間:
- ツールバーの Play ボタンは見た目が変わらない（`ui/mod.rs:2046-2057`。
  `is_playing()` はまだ false のため）
- ビルド中表示は Inspector 下部の `show_game_component_build_status`
  （`ui/mod.rs:6578`）にしか無く、Inspector を開いていないと何も見えない

### 実装方針
1. `game_build.state() != GameBuildState::Idle` または `play_after_game_build` 中は、
   ツールバーの Play ボタンを spinner + 「Building…」表示に差し替えてクリック無効化する。
   ホバーテキストで「ビルド完了後に自動で Play します」を出す。
2. workspace 中央にオーバーレイ（「Building game code… Play will start when finished」+
   Cancel ボタン。Cancel は `play_after_game_build = false` にするだけでビルドは継続）。
3. ビルド完了/失敗を toast + Console に出す（項目 4 の `report_error` / info 版を利用）。

### 影響範囲
`ui/mod.rs` のツールバー・オーバーレイ描画のみ。規模: 小。

---

## 7. モデルや画像を「コンポーネントとして」追加する方法が分からない

### 現象
3D モデルや画像をエンティティに付ける手順が発見できない。

### 原因（現状仕様の整理）
導線は存在するが発見不能になっている。
- **モデル**: Asset Browser で glTF/OBJ をインポート → entity に `engine.mesh`
  （AssetRef）+ `engine.material` を追加する。または mesh アセットをシーンへ
  ドラッグ＆ドロップすると entity が生成される（Phase 32、`session.rs:850`）。
- **画像**: 「画像コンポーネント」は存在しない。画像は (a) material の texture、
  (b) UI ドキュメントの Image ノード、のどちらかとして使う設計。

つまり機能欠落ではなく、ガイドと UI 上のアフォーダンスの欠如。

### 実装方針
1. Asset Browser の mesh / glTF エントリ右クリックに **「Add to Scene」** を追加
   （既存のドロップ生成と同じ `create_scene_entity_from_dropped_mesh` 系を呼ぶだけ）。
2. Add Component ピッカーの先頭（検索欄の下）に 1 行ヘルプ:
   「To show a model: add Mesh + Material. Images are used by Materials or UI documents.」
3. `docs/manual/`（新設）にクイックガイド 2 本:
   「モデルを表示する」「画像を使う（material texture / UI Image）」。
4. 中期検討（別タスク・優先度低）: ビルボード表示の `engine.sprite`
   コンポーネント（quad mesh + texture）。builtin registry 追加になるため
   ADR 0027 の手順に従う。

### 影響範囲
1〜2 は editor のみで小。3 はドキュメントのみ。4 は engine+editor で中（今回はやらない）。

---

## 8. Scene View で選択し直すとヒエラルキーのハイライトが 2 つになる

### 現象
Hierarchy で entity A を選択後、Scene View で entity B をクリックすると、
Hierarchy 上で A と B の両方が選択表示になる。

### 原因（特定済み）
選択状態が 2 本立てになっている:
- `selected_entity: Option<EntityId>`（単一・Inspector 対象）
- `selected_entities: Set<EntityId>`（複数選択）

Hierarchy クリックは両方を更新する（`ui/mod.rs:5290-5293`）が、
Scene View のピック結果処理（`ui/mod.rs:5646-5647`）は
`selected_entity` **だけ**を書き換えて `selected_entities` を放置する。
行のハイライトは OR 判定
（`ui/mod.rs:5224`: `selected_entities.contains(&id) || selected_entity == Some(id)`）
のため、旧選択と新選択の 2 行が同時に光る。

### 実装方針
1. 選択変更を `fn set_selection(&mut self, primary: Option<EntityId>, additive: bool)`
   のようなヘルパーに集約し、`selected_entity` / `selected_entities` /
   `hierarchy_selection_anchor` を常に同時更新する。
2. Scene View ピックはこのヘルパー経由にする（Ctrl 押下時は additive 追加、
   通常クリックは置き換え）。`selected_entity = Some(..)` を直接代入している
   他の箇所（`ui/mod.rs` に約 15 箇所: 2538, 5390, 5527, 6948 など）も順次ヘルパーへ移行する。
3. 回帰テスト: 「複数選択後にピックで置き換えると selected_scene_ids が 1 件になる」。
   ※このバグは項目 3（atomic Add の全体失敗）の誘発要因でもあるため先に直す。

### 影響範囲
`ui/mod.rs` のみ。規模: 小（ただし代入箇所の網羅確認が必要）。

---

## 9. UI Builder プレビューの直接操作性が壊れている（Spacer 不可視・クリック選択がほぼ効かない）

### 現象（2026-07-20 詳細監査で拡充）
- Spacer を置いても何も見えず、どこからどこまでが Spacer なのか分からない。
- プレビュー上でノードをクリックしても選択できないことが多く、
  実質 Hierarchy ツリーからしか選択できない。

### 原因（特定済み）
プレビューのクリック選択機構自体は存在する（`ui_builder.rs:878-884` が
クリックされたノード id を `update_ui_selection` に渡す）が、
**クリックを検知できるノード種別がほぼ無い**。判定は各ノードの
`response.clicked()`（`ui_builder.rs:1055`）に依存しており:

| ノード種別 | プレビュー描画 | クリック検知 |
|-----------|---------------|-------------|
| Button | `ui.button` | ○（唯一まともに動く） |
| Text | `ui.label` | ×（Label は hover しか sense しない） |
| Panel / Container / Overlay | `Frame::show(...).response` | ×（hover のみ） |
| Image | `allocate_ui(...).response` | ×（hover のみ） |
| Spacer | `allocate_response(size, Sense::click())` | ○だが**完全に不可視**で狙えない |
| ScrollView | `allocate_response(Vec2::ZERO, click)` | ×（サイズゼロ） |

さらに**選択中の表示も種別依存**: 選択枠（LIGHT_BLUE stroke）が出るのは
Panel/Container 系だけ（`ui_builder.rs:911-915`, `1071-1078`）。
Text / Spacer / Image は選択しても**何もハイライトされない**。
Spacer は「選択しても見えない・見えないから押せない」の二重苦になっている。
ホバーハイライトも無い。

### 実装方針
1. **全ノード種別をクリック可能にする**:
   - Text: `egui::Label::new(..).sense(egui::Sense::click())`
   - Panel / Container / Overlay / Image: 描画後に
     `response.interact(egui::Sense::click())` で判定を付与
   - ScrollView: 子領域全体を覆う response に差し替え
   - 既存の「深い子を優先」ロジック（`prefer_descendant_click`）はそのまま活かす。
     `preview_interactions` トグル ON 時（ウィジェット操作モード）は現状どおり
     選択しない。
2. **選択枠を全ノード種別で統一**: 各ノードの `response.rect` に対して
   種別を問わず選択 stroke を描く（Frame の stroke 依存をやめ、描画後に
   `painter.rect_stroke` を重ねる方式に統一）。
3. **ホバーハイライト**: 非選択ノードにホバー中は薄い枠を出し、
   「クリックすれば選べる」ことを見せる。
4. **Spacer の可視化**: エディタプレビューでは常に斜線ハッチ + サイズラベル
   （例 `8px`）を描画する（選択の有無に関わらず。ランタイムは不可視のまま）。
   Inspector の Size 横に「親が Vertical なら高さ、Horizontal なら幅」の説明を追加。
5. ノードの並べ替えは Up / Down / Move to Root ボタンが既にある
   （`ui_builder.rs:377-383`）ため当面維持。ツリーのドラッグ並べ替えは
   将来の改善候補として記録のみ。

### 影響範囲
`ui_builder.rs` のみ。`*.ui.json` フォーマット・ランタイムには触れない。規模: 中。

---

## 10. System が全部あってシーンごとに on/off という設計はどうなのか（Unity は？）

### 現状の整理
Systems パネル（`systems_panel.rs`）の on/off は**シーンごとではなくプロジェクト単位**
（`ProjectSettings` の `SystemSettings` に保存）。全 engine/game system の
enable/disable と schedule（Update/FixedUpdate）を編集できる。

### 設計上の回答
- **Unity（MonoBehaviour）には「システム一覧を on/off する」概念自体が無い。**
  挙動はコンポーネントを付けるか外すかで決まる。Unity DOTS や Bevy でも
  system は基本すべて登録され、query にマッチする entity が無ければ実質 no-op に
  なるのが普通で、ユーザーが手で system を切るのは例外的（デバッグ・プロファイル用途）。
- 本エンジンも component-driven（対象コンポーネントを持つ entity が無ければ
  何もしない）なので、**既定は「全部 on」が正しい**。パネルは「高度な設定 +
  デバッグ用の可視化」として温存する位置づけでよい。
- **シーンごとの on/off は導入しない**ことを推奨する。シーン切替（Phase 55
  `SceneManager`）時の再適用・保存先・undo との絡みで状態管理が複雑化する割に、
  同じ結果はコンポーネントの有無で表現できる。将来本当に必要になった場合は
  `scene.json` 側に override を持たせる設計になるため、その時点で ADR を書く
  （`docs/AGENTS.md` §4 のルールどおり）。

### 実装タスク
コード変更は不要。Systems パネル上部に 1 行の説明
（「Systems run only when matching components exist. Disabling is for
debugging/profiling; project-wide, not per-scene.」）を追加するのみ。規模: 極小。

---

## 11. 開始状態の背景が不格好 — 天球（スカイ）が欲しい

### 現象
シーンの背景が暗い単色で、新規プロジェクトの第一印象が悪い。

### 原因（特定済み）
背景は `crates/engine/src/render.rs:686-691` にハードコードされた
clear color `(0.05, 0.05, 0.08)`。sky 描画は存在しない。
Environment 設定（`editor/environment.rs`）には ambient / directional light しか無い。

### 実装方針（段階導入）
1. **Phase A — procedural gradient sky（推奨・先行）**:
   メッシュ描画の前に depth-write 無しのフルスクリーントライアングルパスを追加し、
   カメラの逆 view-projection から視線方向を復元して zenith / horizon / ground の
   3 色グラデーションを描く。`EnvironmentSettings` に `sky { zenith, horizon, ground,
   enabled }` を追加（serde default で後方互換。既存プロジェクトは従来の clear color
   相当の見た目を維持）。エディタの Environment パネルに色編集を追加。
2. **Phase B — 新規シーンの既定を整える**: 新規プロジェクト生成時の main.scene.json
   に directional light + 床 plane を含め、sky を既定 ON にする。
3. **Phase C（将来）** — HDRI / キューブマップ skybox。Phase 41（IBL）の環境
   ライティング資産と整合させるため、着手時に ADR を書く。

### 影響範囲
Phase A は `engine/render.rs` + shader 追加 + editor 設定 UI で**複数クレートに
またがる**。破壊的変更プロトコル（AGENTS.md §3）に従い同一 PR で行う。規模: 中。

---

## 12. アニメーションとかクリップとかよく分からない

### アニメーション制作パイプラインの現状（前提の整理）

**クリップをエンジン内で作る機能（キーフレームエディタ）は存在せず、意図的に
DCC ツール任せの設計**になっている。これは Unity/Unreal でも本格的なキャラクター
アニメは DCC（Blender/Maya 等）で作るのが標準であり、方針として妥当。

```
Blender 等の DCC ツール
  └─ モデル + スケルトン + アニメーションを作成し glTF 2.0 (.gltf/.glb) で export
     ※ FBX は直接パースしない（docs/FBX_IMPORT.md）。DCC 側で glTF に変換する
エディタ: Asset Browser で Register Asset
  └─ background import が Mesh / Material / Texture / Skin / AnimationClip を
     stable sub-asset として Manifest にカタログ化（Phase 36/48。Reimport で ID 維持）
  └─ 併せて配置用 prefab を生成（ADR 0074）。通常はこれを Scene にドロップすれば
     マテリアル込みで正しく出るので、下の component 設定は手で組む場合の参考
entity 側:
  engine.skeleton            … どの glTF の skin からジョイント階層を作るか
  engine.skinned_mesh        … どの mesh サブアセットを、どの Skeleton Entity で変形するか
  engine.animator            … clip_source（AssetRef）+ clip_name + looping/autoplay/速度
  engine.animation_graph_player … 状態遷移 + crossfade（Animation Graph、ADR 0033）
Scene View の Animation Preview で Play せずに再生確認
```

パイプライン自体（import → clip 抽出 → 再生 → preview → パッケージ同梱）は
**実装済み**。手順は `docs/USER_MANUAL_JA.md` §9.3 に記載がある。
FBX 直接対応は glTF 変換手順の案内（`docs/FBX_IMPORT.md`）で代替しており、
FBX SDK 依存を増やさない現方針を維持する。

### UI 上の問題（原因）
- 上の対応関係（source → clips → animator）が UI のどこにも図示されない。
- `clip_name` がほぼ手打ちのテキスト入力（`ui/mod.rs:9059-9076` など）で、
  typo すると**何も再生されず無反応**になる。
- **Animation サブアセットがピッカー対応表から漏れている**:
  `imported_sub_asset_matches_picker_kind`（`ui/mod.rs:8453`）は
  Mesh / Material / Texture しかマッチせず、検証側
  （`engine/components.rs:1593` の `Animation → AnimationClip` 対応）と非対称。
- クリッププレビュー機能は存在する（`ui/mod.rs:5608` 「Sample authored Animator
  clips…」）が導線が弱い。

### 実装方針
1. **clip_name を選択式に**: `clip_source` に設定された glTF の Manifest sub-asset
   から実在クリップ名を列挙して選択できるようにする（手入力もフォールバックで許可）。
   1 ソースあたりのクリップ数は少数なので一覧が破綻しない。
   存在しない名前が入っている場合は Inspector に警告を出す。
   `clip_source` 自体の設定は項目 13 のドラッグ＆ドロップを主導線とする。
2. Animator インスペクタに「Preview clip」ボタンを常設し、既存プレビューへの導線を明示。
3. `docs/manual/` にアニメーションのクイックガイド（上の概念図 + 手順:
   DCC → glTF export → Register Asset → 生成 prefab を Scene にドロップ → Preview）。

### 影響範囲
1 は editor + asset catalog 参照で中。2〜3 は小。

---

## 13. アセット参照はドロップダウンではなく Asset Browser からのドラッグ＆ドロップにしたい

### 現象（要望）
AssetRef のドロップダウン一覧はアセットが増えると破綻する。Asset Browser から
Inspector のフィールドへ直接ドラッグ＆ドロップで割り当てたい。

### 調査結果
**設計は Phase 32 で既に存在するが、配線されていない。**
- `crates/editor/src/drag_drop.rs:34` に `DropTarget::InspectorField { entity,
  component_type, field }` が定義されテストも書かれている。
- Asset Browser のエントリはドラッグ開始時に `DragPayload`（asset_id + path + kind）
  をセット済み（`ui/mod.rs:7932`）。
- しかし受け側で `DragPayload` を受理するのは **Scene View だけ**
  （`ui/mod.rs:5677-5690`）。Inspector の AssetRef ウィジェット
  （component 単位のピッカーと `InspectorFieldControl::AssetRef` フィールドの両方）は
  ドロップを受け付けない。`DropTarget::InspectorField` は一度も構築されない。

### 実装方針
1. AssetRef を表示する 2 種のウィジェット（component 単位の asset ピッカー / 
   field 単位の `AssetRef(kind)` コントロール）を `DragPayload` のドロップターゲットにする:
   - `dnd_hover_payload::<DragPayload>()` で kind 互換（既存の
     `manifest_path_matches_asset_kind` を再利用）なら枠をハイライト、
     非互換なら不可カーソル表示。
   - `dnd_release_payload` で受理したら、ピッカーで選択した場合と同じ編集経路
     （`SetProperty` / `SetComponentValue`）で asset id を書き込む（undo 可能）。
2. **glTF ソースファイルをドロップした場合のサブアセット解決**:
   - Mesh / Material / Texture フィールドへ glTF ファイルをドロップ →
     サブアセットが 1 件ならそれを自動選択、複数ならその場に小ポップアップで
     該当 kind のサブアセットのみ列挙して選ばせる。
   - Animator の `clip_source` へは glTF ファイル自体が正しい値なのでそのまま割り当て、
     `clip_name` は項目 12-1 の選択式で続けて選ぶ。
3. ドロップダウンは残すが補助扱いにする（検索付き・項目 2 と同様に
   ソース別グループ表示にして破綻を防ぐ）。
4. テスト: kind 不一致で書き込まれないこと / ドロップが undo 1 ステップになること。

### 影響範囲
`ui/mod.rs`（AssetRef ウィジェット群）+ `drag_drop.rs`（既存型をそのまま使用）。
シリアライズ・engine 側の変更なし。規模: 中。

---

## 14. UI Builder で Image を配置しても画像が表示されない

### 現象（追加フィードバック 2026-07-20）
UI Builder のプレビューに Image ノードを置いても実画像が出ず、
「Image / パス名」のテキストプレースホルダが表示されるだけ。

### 調査結果
- **ランタイム側は実画像を描画できる**: `crates/engine/src/ui_document.rs:497` の
  `draw_image_node` が `image::open(source)` で実ファイルを読み、egui テクスチャに
  キャッシュして描画する（失敗時は「Missing image」表示）。Play すれば画像は出る。
- **UI Builder のプレビューだけが未実装**: `crates/editor/src/ui_builder.rs:945-966`
  はサイズ枠に「Image\n{source}」というラベルを置くだけで、画像読み込みを行わない。
- **source はただのテキスト入力**（`ui_builder.rs:608` の `text_edit_singleline`）。
  アセットピッカーもドラッグ＆ドロップも無く、パスを手打ちする必要がある。
- **注意（潜在バグ）**: ランタイムの `image::open(source)` は**プロセスのカレント
  ディレクトリ基準**で相対パスを解決する。editor Play / packaged player の cwd に
  依存して同じ `assets/ui/x.png` が出たり出なかったりし得る。プレビュー実装時に
  project root / package root 基準の解決に統一する。

### 実装方針
1. **プレビューで実画像を描画**: engine の `draw_image_node` を公開 API に昇格
   （rustdoc 必須）して editor のプレビューから再利用する。nine-slice / tint も
   ランタイムと同じコードパスで描画されるため、見た目の乖離が起きない。
   読み込み失敗時はランタイムと同じ「Missing image + パス」表示。
2. **パス解決の統一**: `source` の相対パスを editor では project root 基準、
   player では package root 基準で解決するよう、描画側に base path を渡す形にする
   （`*.ui.json` の保存内容は相対パス文字列のまま変更しない）。
3. **source フィールドの改善**: テキスト入力に加えて
   (a) Texture kind のアセットピッカー、(b) 項目 13 の `DragPayload` ドロップ受理
   （Asset Browser から画像を Image ノード / source 欄へドラッグ）を付ける。
   Image ノード自体を UI Builder キャンバスへのドロップで新規作成できるとなお良い。
4. ファイル変更検知（既存の mtime ホットリロード）でテクスチャキャッシュを無効化する。

### 影響範囲
engine（`draw_image_node` の公開化 + base path 引数）+ editor（プレビュー・ピッカー）。
2 クレートにまたがるため同一 PR で行う（破壊的変更プロトコル §3）。
`*.ui.json` フォーマットは不変。規模: 中。

---

## 15. カメラの構図を Play せずに確認できない

### 現象（フロー監査 2026-07-20 で発見）
シーンにカメラ entity を置いても、そのカメラから見た構図を編集モードで確認する
手段が無い。確認には Play が必要で、game code が stale だと cargo ビルド待ちまで
挟まる。レベルデザインの反復が非常に遅くなる。

### 原因
- Scene View のプレビューワールドは**カメラ entity を despawn してから**
  エディタカメラを挿入する（`scene_view.rs:1111-1119` の
  `despawn_camera_entities`）。
- カメラのフラスタム（視錐台）を示すデバッグ描画も無い。ギズモで動かしても
  何がどう写るのか全く分からない。

### 実装方針
1. **フラスタム描画**: 選択中（または常時）のカメラ entity について、fov/near/far
   から視錐台のエッジを `DebugLines` で描画する（collider debug draw と同じ経路）。
2. **ピクチャインピクチャ preview**: カメラ entity 選択中、Scene View の隅に
   そのカメラの view-projection でレンダリングした小窓を表示する。既存の
   オフスクリーンレンダラをカメラ行列だけ差し替えて 2 回目のパスを回す。
3. **Align Camera to View**: 現在のエディタ視点をカメラ entity の Transform に
   書き込むコマンド（undo 1 ステップ）。逆方向の Align View to Camera も。

### 影響範囲
editor（`scene_view.rs` 中心）。engine 変更なし（既存 API で可能）。規模: 中。

---

## 16. Play 中にコンポーネントの実値が見えない

### 現象（フロー監査 2026-07-20 で発見）
Runtime Debugger は entity の一覧（Runtime ID / Name / Authoring ID /
**コンポーネント名のみ**）を表示するだけ（`ui/mod.rs:2447-2462`）。
「なぜこの entity がここにいるのか」「速度はいくつか」「Animator は今どの
クリップか」を Play 中に確認する手段が無く、挙動デバッグがログ頼みになる。

### 実装方針
1. `RuntimePlayState::entity_debug_snapshot` を拡張し、**選択中 entity のみ**
   主要コンポーネントの値を `(表示名, 値文字列)` ペアで返す
   （Transform 位置/回転、PhysicsBody 速度、Animator クリップ名+再生位置、
   KinematicCharacterController 状態など。まず読み取り専用）。
2. Runtime Debugger の右側に選択 entity の値ペインを追加。Pause/Step
   （実装済み）と組み合わせてフレーム単位の値確認ができるようにする。
3. Authoring ID がある entity は Hierarchy 選択と連動させる
   （Play 中に Hierarchy で選ぶと Runtime Debugger でも選択される）。
4. 書き換え（live tweak）はスコープ外。必要になったら別項目にする。

### 影響範囲
editor `runtime.rs` + `ui/mod.rs`。engine の公開 API 変更なし
（snapshot は editor 側で World を読むだけ）。規模: 中。

---

## 17. ギズモにスナップが無い

### 現象（フロー監査 2026-07-20 で発見）
移動/回転/スケールのドラッグに刻み（グリッドスナップ）が無く、
「1 ユニット間隔で並べる」「90 度回す」が手作業では困難。
Align/Distribute コマンドはあるが、ドラッグ中のスナップは未実装
（`scene_view.rs` / `gizmo.rs` に snap 実装は存在しない）。

### 実装方針
1. **Ctrl ホールドでスナップ**: ドラッグ中に Ctrl が押されていたら、
   累積 delta を translate = 1.0（Shift+Ctrl で 0.1）、rotate = 15°、
   scale = 0.1 刻みに丸めてから `apply_*_delta` に渡す。
2. 刻み幅は `EditorPreferences` に保存し、ツールバーに数値入力を置く。
3. 丸めは `gizmo.rs` 側の純関数として実装しテストする。
4. 項目 1（ギズモ判定改善）と同じファイル群のため**同時に実施する**。

### 影響範囲
`gizmo.rs` + `scene_view.rs` + `preferences.rs`。規模: 小。

---

## 18. Inspector に色フィールドのカラーピッカーが無い

### 現象（詳細監査 2026-07-20 で発見）
ライトやパーティクルの色を r / g / b の**数値 3 つを別々にドラッグ**して
編集するしかなく、狙った色を作るのが困難。

### 原因
`InspectorFieldControl`（`engine/components.rs:122-137`）に `Color` バリアントが
**存在しない**（Enum / AssetRef / LayerMask / Number / LodLevels /
AnimationEvents / StringBoolMap のみ）。そのため色を持つコンポーネント
（directional_light / ambient_light / particle_emitter 等）の r/g/b は
汎用の数値 DragValue で個別表示される。
Material Editor 側には `color_edit_button` があり（`material_editor.rs`）、
コンポーネント Inspector とアセットエディタで編集体験が分裂している。

### 調査済みで問題なしの点
数値フィールドは DragValue（範囲制約付き）、Transform は
Position/Rotation/Scale のグループ専用エディタ（`ui/mod.rs:8560`）、
EntityRef は entity 一覧からの選択式が既にあり、これらは良好。

### 実装方針
1. `InspectorFieldControl::Color { fields: [&'static str; 3], hdr: bool }` を追加し、
   該当コンポーネントの field hint に設定する（schema/シリアライズは r/g/b の
   数値フィールドのまま — **保存形式は不変**。表示だけスウォッチ+ピッカーにする）。
2. Inspector 側は 3 フィールドをまとめて 1 つの `color_edit_button_rgb` として描画し、
   編集結果を 3 つの `SetProperty` （1 undo ステップ）に落とす。
3. HDR ライト（intensity > 1 相当）は既存の intensity フィールドと組み合わせるため、
   ピッカー自体は 0..=1 で運用する。

### 影響範囲
`engine/components.rs`（enum バリアント追加 + hint 更新）+ editor Inspector。
公開 enum への追加のため rustdoc 必須・破壊的変更プロトコル対象（2 クレート同一 PR）。
規模: 小〜中。

---

## 19. Asset Browser / アセットピッカーにサムネイルが無い

### 現象（詳細監査 2026-07-20 で発見）
Asset Browser もアセットピッカー（ドロップダウン）も**テキストのみ**
（`[texture] icon.png` のような kind プレフィックス + ファイル名）。
画像やマテリアルを名前だけで判別する必要があり、アセットが増えると
「モデルを一覧から名前で選ぶしかない」問題（項目 13）をさらに悪化させる。

### 原因
サムネイル生成・キャッシュ機構が存在しない（`asset_browser.rs` は
パス分類のみ。エディタ内に texture プレビュー用の実装は
Material Editor のプレビューを除いて無い）。

### 実装方針（段階導入）
1. **Texture のみ先行**: 画像ファイルを `image` crate で縮小読み込み →
   egui テクスチャとして LRU キャッシュ。Asset Browser のグリッド表示
   （現行のリスト表示にサムネイル列を足すだけでも可）と、
   項目 13 のピッカー / ドロップ先ポップアップに同じキャッシュを流用する。
   読み込みはバックグラウンド（既存の asset_import 同様のワーカー）で行い
   UI をブロックしない。
2. **Material**: base color + テクスチャ有無から単色スウォッチを合成（安価）。
3. **Mesh / glTF**: 将来課題として記録のみ（オフスクリーンレンダラで
   サムネイルベイクは可能だが、費用対効果を見てから）。

### 影響範囲
editor のみ（`asset_browser.rs` + `ui/mod.rs` + 小さなキャッシュモジュール追加）。
規模: 中。

---

# Unity 比較監査（2026-07-20 追加）

「Unity でできるのにこのエンジンでできない」操作を体系的に洗い出した結果。

## 20. Scene View のピッキングが固定サイズ立方体（オブジェクトの実サイズを無視）

### 現象
大きいオブジェクト（引き伸ばした床など）は**中心付近をクリックしないと選択できず**、
逆に小さいオブジェクトははるか外側でも選択されてしまう。重なった物は狙った方を選べない。

### 原因（特定済み）
`SceneView::pick`（`scene_view.rs:871-896`）は全 entity について
**半辺 0.5 の固定立方体**（`Vec3::splat(0.5)`）と ray の交差しか見ていない。
メッシュの実 AABB もスケールも無視している。Unity はレンダラの実ジオメトリで
ピックするため、この差が「クリックが効かない/誤爆する」体感差になる。

また、メッシュを持たない entity（ライト・オーディオ・トリガー等）は
**画面に何も表示されない**まま固定立方体でピック対象になっており、
「見えない物を盲撃ちで選択してしまう」逆問題も起きる。Unity はアイコン
（ビルボードギズモ）を描いて見えるようにしている。

### 実装方針
1. ピック AABB を実サイズにする: mesh handle から `Mesh` の AABB を取り、
   entity のワールド行列（scale/親子含む）で変換した AABB を使う。
   メッシュ無し entity は従来の小さい固定ボックスで維持。
2. **アイコンビルボード描画**: カメラ / ライト / オーディオ / パーティクル /
   トリガーの位置に常時アイコン（DebugLines のスプライト化 or 単純な
   ワイヤ形状: ライト=太陽マーク、カメラ=錐台ミニチュア、音=音符円）を描画し、
   「見えて、クリックで選べる」状態にする。項目 15（カメラフラスタム）と同時実施。
3. 手前の小物と奥の大物が重なる場合に備え、交差距離順のサイクル選択
   （同じ場所を再クリックすると次の候補）を検討（Unity と同挙動）。

### 影響範囲
`scene_view.rs` 中心。規模: 中。

---

## 21. 選択・ギズモの操作モードが Unity 比で不足

### 調査結果（いずれも未実装であることを確認）
| Unity の操作 | 本エンジン |
|-------------|-----------|
| ドラッグでボックス（マーキー）選択 | 無し（1 クリック 1 選択のみ。複数選択は Hierarchy の Ctrl/Shift だけ） |
| ギズモの Local / Global 切替 | 無し（`hit_test_gizmo_axis` も delta 計算も `Vec3::X/Y/Z` のワールド軸固定。回転した entity を自分の向きに動かせない） |
| 右クリック + WASD のフライスルーカメラ | 無し（右ドラッグ orbit / 中ドラッグ pan / ホイール zoom のみ） |

### 実装方針
1. **ボックス選択**: Scene View 上の空白から primary ドラッグでスクリーン矩形を描き、
   entity 中心の投影点が矩形内に入る物を `selected_entities` に一括設定
   （項目 8 の `set_selection` ヘルパー経由）。ギズモドラッグ・カメラ操作との
   入力競合は「ギズモ命中 > 矩形開始」の優先順で解決。
2. **Local / Global トグル**: ツールバーに切替を置き、Local 時は entity の回転行列を
   軸方向に適用してから既存の delta 計算に渡す（`gizmo.rs` は軸ベクトルを
   引数化するだけで対応可能）。
3. **フライスルー**: 右ボタン押下中の WASD + QE 移動（速度はホイールで調整）。
   `EditorViewCamera`（orbit 型）に fly モードを追加。

### 影響範囲
`scene_view.rs` + `gizmo.rs`。規模: 各小〜中（独立に実施可能）。

---

## 22. エンティティの有効/無効（Unity の SetActive チェックボックス）が無い

### 調査結果
`AuthoringEntity`（`crates/authoring/src/entity.rs:24-64`）は
id / name / display_name / description / parent / components のみで、
**enabled / active に相当するフィールドが無い**。エディタの目玉アイコン
（`hidden_entities`）はエディタ表示専用の一時状態で、保存されず Play にも影響しない。
「この敵だけ一時的に無効にしてテスト」という Unity の日常操作ができない。

### 実装方針
1. `AuthoringEntity` に `#[serde(default = "default_true")] pub enabled: bool` を追加
   （旧シーンは field 欠落 = true で後方互換。**シリアライズ形式の追加変更**にあたるため
   移行テスト必須・AGENTS.md §3 に従い authoring / engine / editor 同一 PR）。
2. `spawn_from_authoring_scene` は `enabled: false` の entity（と子孫）を spawn しない
   （まずはこの単純仕様。ランタイム中の SetActive は Rhai API 含め別項目とする）。
3. Hierarchy の行にチェックボックスを追加し、Inspector にも表示。
   undo 対象のコマンド（SetProperty 相当）として実装。
4. クレート境界をまたぐ設計決定のため **ADR を書いてから実装**する。

### 影響範囲
authoring + engine + editor。規模: 中。

---

## 23. コンポーネント値の Copy / Paste / Reset が無い

### 調査結果
Unity のコンポーネント右クリック（Copy Component / Paste Component Values /
Reset）に相当する機能が**一切無い**（entity 単位の Copy/Paste/Duplicate はある）。
「調整済みの Collider 設定を別の entity に写す」には全フィールドを手で写経するしかない。

### 実装方針
1. Inspector のコンポーネントヘッダ右クリックメニューに
   - **Copy Component Values**: `(ComponentTypeId, Value)` をエディタ内クリップボードへ
   - **Paste Component Values**: 同型コンポーネントを持つ選択 entity へ
     `SetComponentValue`（1 undo ステップ、複数選択対応）
   - **Reset**: `schema.default_value()` で上書き
2. 型が違う場合は Paste を無効表示。既存の entity クリップボードとは別枠で持つ。

### 影響範囲
`ui/inspector.rs` のみ。規模: 小。

---

## 24. 複数選択時に Inspector で一括プロパティ編集ができない

### 調査結果
複数選択に対応しているのは Add/Remove Component・Align/Distribute・削除・複製、
および **Transform 位置のみの一括絶対値編集**（`ui/inspector.rs:165-208` の
Common Transform Position。Mixed 表示付きで Unity 相当の挙動が既にある）。
それ以外の**コンポーネントプロパティ編集は常に primary の 1 entity にしか
適用されない**（`apply_component_edit` が単一 entity 前提）。Unity は任意の
共通フィールドをまとめて編集できる。位置編集の実装が良い前例なので、
これを一般化する方向で進める。

### 実装方針（段階）
1. まず「選択中の全 entity が同型コンポーネントを持つ場合、編集した
   フィールドを全員に適用する」トグル（Inspector 上部の "Edit all selected"）
   から始める。値の混在表示（—）は初期実装では見送り、primary の値を表示する。
2. 全員適用は既存の atomic トランザクション（`apply_scene_commands`）で
   1 undo ステップにする。
3. 本格的な混在値表示は需要を見て別項目に切り出す。

### 影響範囲
`ui/inspector.rs` + `session.rs`。規模: 中。

---

## 25. Game View にアスペクト比 / 解像度プリセットが無い

### 調査結果
Game View はパネルサイズに追従するのみ（`ui/play.rs:239` の
`maintain_aspect_ratio(false)`）。Unity の「16:9 / 1920x1080 固定」のような
プリセット確認ができず、UI の safe area 確認（UI Builder 側にはプリセットあり）と
実行画面の確認が食い違う。

### 実装方針
Game View ヘッダにプリセット選択（Free / 16:9 / 4:3 / 1920x1080 / カスタム）を置き、
固定時はレターボックスで中央表示。`ViewportSize` リソースへ反映するだけで
ランタイム側は既対応。UI Builder の preview_preset と選択肢を共通化する。

### 影響範囲
`ui/play.rs` + `preferences.rs`。規模: 小。

---

## 26. マテリアルを Scene View のオブジェクトへドロップして割り当てられない

### 調査結果
Scene View への `DragPayload` ドロップは **Mesh kind のみ**処理し
（`ui/viewport.rs:227-239` — entity 新規作成）、Material のドロップは無視される。
Unity の「マテリアルをオブジェクトに投げて着せ替える」操作ができない。

### 実装方針
ドロップ位置で `SceneView::pick`（項目 20 の実 AABB 版）を実行し、
命中 entity に対して `engine.material` を `SetComponentValue` /
無ければ `AddComponent` で割り当てる（1 undo ステップ）。
ホバー中は命中 entity をハイライトしてどこに落ちるかを見せる。
項目 13（Inspector D&D）と同じ kind 判定・ハイライト実装を共有する。

### 影響範囲
`ui/viewport.rs`。規模: 小。

---

## 27. Inspector の編集品質（コンポーネント順序・undo 粒度・無ラベル入力欄）

### 調査結果（2026-07-20 重点監査。`ui/inspector.rs` = 分割後）
1. **Transform が一番下に沈む**: コンポーネントは `entity.components`（BTreeMap）の
   イテレーション順 = **ComponentTypeId のアルファベット順**で表示される
   （`ui/inspector.rs:369`）。`engine.transform` は t 始まりのためほぼ最下部。
   Unity は Transform 固定最上部。
2. **文字列編集が 1 キーストローク = 1 undo ステップ**: コンポーネントの
   String フィールドは `text_edit_singleline(..).changed()` の度に
   `ComponentEdit::Property` を発行する（`ui/inspector.rs:1990-1996`）。
   entity の name / display_name / description も同様（`ui/inspector.rs:222-244`）。
   「hero」と打つと undo 4 回分になり、Ctrl+Z が実質使い物にならない。
   数値には既に draft → commit 機構（ドラッグ完了で 1 undo）があるのに、
   文字列だけ未対応で非対称。
3. **entity ヘッダの 3 連テキストボックスにラベルが無い**: name /
   display_name / description が無記名で縦に並び、どれが何か見分けられない。
4. Remove Component が展開ボディ内の大きいボタンで場所を取る
   （ヘッダの右クリックメニューへ移す方が Unity 慣習に近い）。

### 実装方針
1. 表示順を「Transform 最優先 → builtin をカテゴリ順 → game コンポーネント」の
   固定順序にする（表示のみ。シリアライズは BTreeMap のまま不変）。
2. 文字列フィールドを数値と同じ draft 機構に載せ、**フォーカス喪失または
   Enter で 1 undo ステップ**として commit する（`lost_focus()` 判定）。
   entity name / display_name / description も同様に変更。
3. name / display_name / description に左ラベル（または hint_text）を付ける。
4. Remove Component をヘッダ右クリックメニューへ移動（項目 23 の
   Copy/Paste/Reset と同じメニューに統合）。

### 影響範囲
`ui/inspector.rs` のみ。規模: 小〜中。

---

## 28. Asset Browser: マテリアルを新規作成できない・glTF サブアセットが見えない

### 調査結果（2026-07-20 重点監査。`ui/assets.rs` = 分割後）
1. **マテリアルの新規作成 UI が存在しない**: `MaterialAsset::default()` の呼び出しは
   テストコードにしか無く、エディタ内で standalone material
   （`*.material.json`）を作る手段が無い。Material Editor は**既存ファイルを
   開いて編集する**ことしかできず、マテリアルの入手経路が実質
   「glTF インポートの副産物」だけになっている。Unity の
   Assets > Create > Material に相当する基本操作が欠落。
2. **glTF のサブアセット（メッシュ / クリップ / テクスチャ）がブラウザに
   表示されない**: ブラウザのツリーはファイル単位のみで展開機能はフォルダだけ
   （`ui/assets.rs:1299-1455` 付近）。インポート済みクリップの存在は
   ピッカーを開くまで確認できない。Unity は FBX の下に子アセットを展開表示する。
3. **Show in Explorer（OS のファイラで開く）が無い**: 右クリックメニューは
   Open / Rename / Move / Delete / Reimport 系のみ。
   `open::that` は他所で使用済みなので追加は容易。
4. 問題なしと確認: 検索フィルタあり・OS からのファイルドロップ取り込みあり
   （`ui/chrome.rs:725-737`）・ダブルクリックで開く・Register/Reimport 導線あり。

### 実装方針
1. ブラウザ右クリックに **Create > Material**（`MaterialAsset::default()` を
   `<name>.material.json` として書き出し → Material Editor で自動オープン →
   manifest 登録）。項目 5 の Create > Scene と同じメニューに統合する。
2. glTF エントリに展開矢印を付け、サブアセット（kind アイコン + 名前）を
   子行として表示する。子行は項目 13 の `DragPayload` ドラッグ元にもなる
   （クリップを Animator へ直接ドロップ、の起点）。
3. 右クリックに **Show in Explorer** を追加（`open::that(親フォルダ)`）。

### 影響範囲
`ui/assets.rs` + 小さな asset_management 追加。規模: 中。

---

## 29. UI Builder: バインディングのテスト値を流し込めない

### 調査結果
`{score}` のような Bind 指定の Text は、プレビューでは**そのまま
`{score}` という文字列**が描画される（`ui_builder.rs:1623` の
`preview_string`、数値 Bind は固定 0.5）。実データ相当の桁数
（例: `999999`）でレイアウトが崩れないかをビルダー内で確認できず、
Play するまで分からない。

問題なしと確認: ノード ID リネーム（Advanced 内）・Copy / Paste Child・
ツリーの Shift 範囲選択・Font Size 編集・undo（`UiDocumentCommand`
トランザクション経由で Ctrl+Z 可能）は実装済み。

### 実装方針
1. UI Builder に「Preview Bindings」パネル（binding 名 → テスト値のテーブル）を
   追加し、`preview_string` / 数値 Bind の解決に使う。値は
   `UiBuilderState`（エディタ一時状態）にのみ保持し、`*.ui.json` には保存しない。
2. ドキュメント内の Bind 名を走査してテーブル行を自動列挙する
   （手入力不要にする）。
3. 将来: ProgressBar / Image の Bind も同テーブルを参照。

### 影響範囲
`ui_builder.rs` のみ。規模: 小〜中。

---

## Unity 比較で「同等機能あり」と確認できた操作（2026-07-20）

| Unity の操作 | 本エンジンでの対応 |
|-------------|------------------|
| ダブルクリックでアセットを開く | あり（`ui/assets.rs` の `double_clicked` → シーン/グラフ/UI を開く） |
| Scene View カメラ operate（orbit / pan / zoom） | あり（右ドラッグ / 中ドラッグ / ホイール） |
| F でフォーカス | あり |
| Ctrl+D 複製・Ctrl+C/V | あり（entity 単位） |
| ドラッグで親子付け（Hierarchy） | あり（Scene Root への drop で解除も可） |
| Play 中の Inspector 値見学 | 不可だが項目 16 で計画済み |
| アセットのドラッグ配置（Scene View へ） | メッシュのみ可（マテリアルは項目 26 で拡張） |

> 補足: Console の Clear / Collapse / テキスト検索も Unity 標準だが未実装。
> これは項目 4（Console 改善）のスコープに含めて実施する。

---

## フロー監査で「問題なし」と確認できた領域（2026-07-20）

再調査を避けるため、ゲーム制作フローを通しで確認した結果、
以下は既に整備されており今回の改善対象から除外した。

| 領域 | 確認結果 |
|------|---------|
| Play 制御 | Pause / Resume / Step（1 フレーム実行）実装済み（`runtime.rs:396-411`） |
| 入力設定 | Project Settings に action / キー / ゲームパッド軸 / deadzone / invert の編集 UI あり（`project_settings_panel.rs`） |
| プレハブ | create / instantiate / apply overrides / revert / unpack が揃っている（`prefab_workflow.rs`） |
| アセット整理 | Rename / Move / Delete / フォルダ操作あり（Asset Browser 右クリック）。Delete は .engine/asset_trash へ退避して復元可能 |
| スクリプト編集 | Rhai 新規作成モーダル + OS 既定エディタで開く（`open::that`）。エディタ内蔵コードエディタは持たない方針で妥当 |
| Hierarchy 検索 | Scene / UI Builder 双方に検索フィルタあり（`Search entities...` / `Search nodes`） |
| Focus 機能 | F キーで選択 entity にカメラフォーカス（`ui/mod.rs:2755` + `focus_entity`） |
| Inspector 数値編集 | DragValue + 範囲制約（`NumericRange`）実装済み |
| Transform 編集 | Position / Rotation / Scale のグループ専用エディタあり（`ui/mod.rs:8560`） |
| Entity 参照編集 | entity 一覧からの選択式エディタあり |
| UI Builder 整列 | Up / Down / Move to Root / Align / Distribute / Duplicate ボタンあり |
| Play 中のシーン再読込 | Reload ボタンあり（Play を止めずに disk から再読込） |
| クラッシュ回復 | 30 秒ごとの recovery autosave あり |
| プロファイラ | tick 時間（last/avg/max）・entity 数・fixed steps 表示あり |

---

## 優先度と実施順の提案

| 優先 | 項目 | 理由 | 規模 |
|------|------|------|------|
| P0 | 8. 選択同期 | 1 行級の明確なバグ。項目 3 の誘発要因 | 小 |
| P0 | 3. Add Component 失敗 | 8 と合わせて「壊れている」印象の主因 | 小 |
| P0 | 6. Play ビルド中表示 | 「反応しない」と誤認される | 小 |
| P1 | 5. New Scene | 基本ワークフローの欠落 | 小 |
| P1 | 4. エラーの Console 集約 + コピー | 以後のデバッグ効率全般に効く | 中 |
| P1 | 1. ギズモ判定改善 + 17. スナップ | 操作感の主要因。同一ファイル群なので同時実施 | 小〜中 |
| P1 | 15. カメラ構図の確認手段 | レベルデザイン反復速度に直結 | 中 |
| P1 | 20. ピッキングの実 AABB 化 + メッシュレス entity アイコン | 「クリックが効かない」体感の主因。15 と同時実施 | 中 |
| P1 | 9. UI Builder の直接操作性（クリック選択・選択枠・Spacer 可視化） | ユーザーが実際に詰まった箇所。全ノード種別への Sense 付与が本体 | 中 |
| P2 | 2. カテゴリ分け | category フィールド既存のため安価 | 小 |
| P2 | 18. Inspector カラーピッカー | 保存形式不変・表示のみの変更 | 小〜中 |
| P1 | 27. Inspector 編集品質（undo 粒度は実害大） | 文字列 1 打鍵 = 1 undo の解消・Transform 最上部化 | 小〜中 |
| P2 | 23. コンポーネント Copy/Paste/Reset | 局所変更で日常操作の写経を解消。27-4 と同メニュー | 小 |
| P2 | 28. Asset Browser: Create Material / サブアセット表示 / Show in Explorer | マテリアル新規作成は基本フローの欠落 | 中 |
| P2 | 26. マテリアルの Scene View ドロップ割り当て | 13/20 と実装を共有 | 小 |
| P2 | 21. ボックス選択 / Local-Global 切替 / フライカメラ | 独立に実施可能な 3 点セット | 各小〜中 |
| P3 | 22. entity 有効/無効フラグ | フォーマット追加変更のため ADR 必須 | 中 |
| P3 | 24. 複数選択の一括プロパティ編集 | 段階導入（Edit all selected トグルから） | 中 |
| P3 | 25. Game View アスペクトプリセット | ViewportSize 反映のみで安価 | 小 |
| P3 | 29. UI Builder バインディングテスト値 | エディタ一時状態のみで完結 | 小〜中 |
| P2 | 10. Systems 説明文 | 回答済み。説明 1 行のみ | 極小 |
| P2 | 13. Asset Browser → Inspector の D&D | 型・payload 送出は実装済みで受け側配線のみ | 中 |
| P2 | 14. UI Builder の実画像プレビュー | ランタイム描画コードを再利用可能。パス解決の潜在バグ修正含む | 中 |
| P2 | 16. Runtime Debugger の値表示 | Pause/Step と組み合わせて挙動デバッグを完結させる | 中 |
| P3 | 19. アセットサムネイル（Texture 先行） | 13 のピッカー/D&D と組み合わせて効果大 | 中 |
| P3 | 7. モデル/画像ガイド + Add to Scene | ドキュメント主体 | 小 |
| P3 | 12. clip 選択式 + アニメガイド | 13 の後にやると自然（clip_source は D&D で設定） | 中 |
| P3 | 11. Gradient sky | 複数クレート・シェーダ追加 | 中 |

- P0 の 3 件は独立した小修正なので 1 PR ずつ、または「editor 選択/追加/Play 表示
  修正」として 1 PR にまとめてもよい（クレートは editor のみ）。
- 11 のみ engine を跨ぐため破壊的変更プロトコル対象。7-4（sprite）と 11-C（HDRI）は
  本計画のスコープ外とし、必要になった時点で ADR を書く。
