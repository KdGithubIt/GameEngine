# Phase 10: Editor Runtime Preview / Game View

## Goal

エディタの GUI から Play ボタンを押すと、現在編集中の scene が runtime world に変換され、
Game View として描画される。Stop で editor に戻れる。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**Phase 9 の後にこのフェーズが来る理由**:  
Phase 9 でファイルベースの scene 編集ができるようになった。しかし「編集した結果がどう動くか」を
確認する手段がない。Play/Stop なしでは開発サイクルが成立しない。

**Phase 11（ライティング等）より先にこのフェーズが来る理由**:  
Game View が動いてから「見た目が正しいか」を確認できる。
ライティングや物理が先では、どのように見えるかを確認する手段がない。

**なぜ別プロセスや別ウィンドウではなく editor 内に Game View を作るか（ADR 0024）**:  
別ウィンドウだと editor との状態を同期する仕組みが必要になり複雑化する。
egui + egui_wgpu の texture 機能を使えば、同一ウィンドウ内の Panel に描画できる。

**前提: wgpu バージョン統一（Track W、ADR 0024）**:  
editor（eframe 0.34.3）は wgpu 29 を、engine（renderer crate）は wgpu 22 を使用中。
wgpu オブジェクトはメジャーバージョンをまたいで共有できない。Phase 10-C（Game View）は
Track W（wgpu 22→29 移行）の完了が前提。Track W が 2 週間タイムボックスを超過した場合は
別プロセス Player 方式（ADR 0024 fallback）に切り替える。

---

**Current status (2026-06-11)**: Track W is complete. `engine` and
`engine-renderer` now use `wgpu = "29"`, matching `eframe 0.34.3` /
`egui-wgpu 0.34.3`. Phase 10-C uses the in-process Game View path through
`egui_wgpu::RenderState`; the ADR 0024 out-of-process fallback is retained
only as a retreat option and is not active.

**追補 (2026-06-11)**: 10-E（AI Observation — Game View frame capture）を
追加し、同日実装済み。設計境界は ADR 0026（Accepted）を参照。
readback は ADR 0026 の改訂どおり engine ではなく editor
（`RuntimePlayState::capture_game_view`）が所有する。Capture Frame
ボタンは PNG ファイル保存まで完了。

## Scope

### 作るもの

- `EditorMode` 列挙型（Edit / Playing）
- Play / Stop ボタン（Toolbar）
- Authoring Scene → runtime world の変換フロー（editor 内から呼ぶ）
- Game View パネル（wgpu テクスチャ → egui 画像として表示）
- Runtime Diagnostics（変換エラー・no camera 等を Diagnostics パネルに出す）

### 作らないもの（次フェーズ以降）

- Pause モード（後でよい）
- runtime のセーブ / ロード（authoring の scene とは別物）
- Step 実行・デバッガ機能
- Game View のサイズ変更への動的対応（最初は固定サイズで可）

---

## Design Decisions

### なぜ Play 中も AuthoringScene を変更しないか

Play 中にエディタが authoring data を書き換えると、runtime world との二重管理が発生する。
「Stop したら編集前の状態に戻る」という保証が崩れる。

**ルール**: Play 中の runtime world と authoring document は完全に分離する。  
runtime world への変更は Play が終われば消える。authoring への変更は Stop 後に行う。

### Game View の実装方針：wgpu テクスチャ → egui 画像

eframe は内部で egui_wgpu を使っている。`CreationContext` 経由で wgpu の `Device` / `Queue` /
`Renderer` にアクセスできる。

フロー:
1. Play 開始時に wgpu `Texture`（ゲームのレンダーターゲット）を作成
2. `egui_wgpu::Renderer::register_native_texture()` で `TextureId` を取得
3. 毎フレーム: ECS + render state で そのテクスチャに描画
4. `ui.image(texture_id, size)` で Game View パネルに表示

**代替案が却下された理由**:
- 別 OS ウィンドウ: eframe のイベントループと競合するため複雑
- 別スレッド: wgpu は `Send` でないため共有が困難
- 別プロセス: 状態共有のための IPC が必要で UX が悪い。ただし Track W が失敗した場合の**retreat 案**として ADR 0024 に文書化済み（永久却下ではない）

### なぜ `crates/editor` に `engine` クレートを追加するか

Play 機能では `spawn_from_authoring_scene()` 等が必要で、これは `crates/engine` にある。
現在の `crates/editor` は `engine-authoring` のみに依存しているが、Play のために `engine` も追加する。

依存関係の変化:
```
Before: editor → authoring
After:  editor → authoring
        editor → engine
```

`engine` → `authoring` の依存は既にある（`scene_bridge.rs`）ため循環はなし。

### なぜ変換エラーを Diagnostics パネルに出すか

Play ボタンを押した瞬間にコンソールにエラーが出ても、ユーザーはウィンドウを切り替えなければ
確認できない。editor の Diagnostics パネルに出すことで、作業を中断せずエラーを確認できる。

---

## Implementation Plan

### 10-A: Play / Stop 状態管理

```rust
// crates/editor/src/session.rs に追加
pub enum EditorMode {
    Edit,
    Playing,
}

// EditorApp に追加
editor_mode: EditorMode,
runtime_state: Option<RuntimePlayState>,

struct RuntimePlayState {
    world: engine_ecs::World,
    game_texture_id: egui::TextureId,
    game_render_target: wgpu::Texture,
    game_render_view: wgpu::TextureView,
    // 必要なシステムのリスト（transform propagation 等）
}
```

Play ボタン押下フロー:
1. `session.current_scene()` を取得。なければ "No scene open" diagnostic
2. `scene.validate()` を呼び、blocking diagnostic があれば Play キャンセル
3. `engine_ecs::World::new()` を作成
4. 基本リソースを挿入（`Time`, `Input<KeyCode>`, `Input<MouseButton>`, `MouseInput`）
5. `spawn_from_authoring_scene(&mut world, &scene)` を呼ぶ
6. `RuntimePlayState` を作成し `runtime_state = Some(...)` にセット
7. `editor_mode = EditorMode::Playing`

Stop ボタン押下フロー:
1. `runtime_state = None`（Drop で world・テクスチャが解放される）
2. `editor_mode = EditorMode::Edit`

### 10-B: Authoring → Runtime World

`engine::scene_bridge::spawn_from_authoring_scene()` を呼ぶだけ。既存実装で十分。

カメラが scene に含まれていない場合は `RuntimeDiagnostic::NoCamera` を追加し、
デフォルトカメラを自動挿入する（ゲームが真っ暗になるよりはよい）。

### 10-C: Game View パネル

```rust
// EditorApp::ui() の CentralPanel 内
if let Some(state) = &self.runtime_state {
    let available = ui.available_size();
    ui.image(egui::load::SizedTexture::new(
        state.game_texture_id,
        available,
    ));
}
```

毎フレームの処理（eframe の `update()` 内）:
```rust
fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
    if let Some(state) = &mut self.runtime_state {
        // ECS を 1 tick 進める
        state.world.update_time(delta);
        ecs_app.update();
        // wgpu でテクスチャに描画
        render_to_texture(frame, state);
    }
    // egui の UI を構築
    self.draw_ui(ctx, frame);
}
```

**Game View サイズ変更**: Panel サイズが変わったらテクスチャを再作成する。
再作成のタイミングは「前フレームのサイズと今フレームのサイズが違う場合」でよい。

### 10-D: Runtime Diagnostics

```rust
pub enum RuntimeDiagnosticKind {
    SceneConversionFailed(Vec<Diagnostic>),
    NoScene,
    NoCamera,
    MissingAsset { path: String },
    RenderError(String),
}
```

Play 開始時に diagnostics をクリアし、上記の条件に応じて追加する。
既存の Diagnostics パネルに表示するだけで新しい UI は不要。

### 10-E: AI Observation — Game View Frame Capture（2026-06-11 追加・同日完了）

**目的**: 現在の Game View フレームを画像（生 RGBA8）として取得できるようにする。
AI エージェントに「いまゲーム画面がどう見えているか」を渡すための観測手段であり、
同時にスクリーンショットによるバグ報告・golden image テストの土台にもなる。

**設計境界（ADR 0026）**:

- engine は「テクスチャを画像として読み戻す」だけを提供する。
  PNG エンコードは Capture Frame のデバッグ保存では editor が担当するが、
  AI API 通信・プロンプト構築・AI bridge 向けの画像変換は CLI / MCP /
  将来の agent クレートが担当する（authoring に対する cli/mcp と同じ
  thin adapter 構成）。
- キャプチャ対象は Game View のオフスクリーンテクスチャ（10-C で実装済みの
  render-to-texture）。OS スクリーンキャプチャは使わない（エディタ UI が
  写り込み、ピクセル座標と `MouseInput.position` の 1:1 対応も壊れるため）。
- 戻り値は `FrameCapture { width, height, rgba8 }`。スクリーンショットの
  ピクセル座標 = レンダーターゲットの物理ピクセル座標であり、12-F の
  仮想マウス入力（`InputCommand::MouseMove`）にそのまま渡せる。

**タスク分解（各 30 分〜1 時間）**:

| # | タスク | 場所 | 状態 |
|---|--------|------|------|
| 10-E-1 | wgpu テクスチャ → RGBA8 読み戻し（`copy_texture_to_buffer`、256 バイト行アライメントの除去、`map_async` + poll）。ADR 0026 の改訂により、レンダーターゲット所有者である editor に実装（第二の消費者が現れたら engine へ抽出） | `crates/editor/src/runtime.rs` | 完了 2026-06-11 |
| 10-E-2 | `FrameCapture` 型 + 公開 API と rustdoc | `crates/editor/src/runtime.rs`（re-export: `editor/src/lib.rs`） | 完了 2026-06-11 |
| 10-E-3 | Game View カラーテクスチャに `COPY_SRC` usage を追加し、`RuntimePlayState::capture_game_view()` を実装 | `crates/editor/src/runtime.rs` | 完了 2026-06-11 |
| 10-E-4 | 動作確認用の "Capture Frame" デバッグボタン（PNG でファイル保存。PNG エンコードは editor 側の `png` crate で行い、engine には入れない） | `crates/editor/src/ui/mod.rs` | 完了 2026-06-11 |

**今すぐ実装するもの / 将来の拡張**:

- 実装済み（2026-06-11）: 10-E-1〜10-E-4
- 将来: MCP/CLI への公開（`game.screenshot` ツール等）と AI API 通信は
  AI Agent Bridge（旧称 Phase 26。実装計画の Phase 26+ 表を参照）

---

## Cautions（注意点・落とし穴）

**eframe の wgpu RenderState へのアクセス**:  
eframe 0.29 以降では `frame.wgpu_render_state()` で取得できる。
`CreationContext` でも取得できるが、Play は実行時に始まるため `frame` 経由が適切。

**wgpu テクスチャのライフタイム**:  
`register_native_texture` で登録したテクスチャは、Stop 時に
`egui_wgpu::Renderer::free_texture()` を呼んで解放する。呼ばないと VRAM リークになる。

**runtime world の Drop 順序**:  
`RuntimePlayState` の Drop では `world` を先に Drop してから GPU リソースを解放する。
逆順だと GPU リソースが使用中に解放される可能性がある。

**ECS の update をメインループのどこで呼ぶか**:  
egui の `update()` は UI スレッドで呼ばれる。ECS の update も同じ場所で呼ぶ。
別スレッドにしない（wgpu が `Send` でない）。

**Play 中にウィンドウリサイズが起きたとき**:  
`game_render_target` のサイズを更新しないと描画がずれる。
毎フレーム Panel のサイズを確認し、変わっていたらテクスチャを再作成する。

---

## Prohibited（禁止事項）

- Play 中に `AuthoringScene` を直接変更することを禁止
- Stop せずに別の scene を開くことを禁止（必ず Stop → open の順）
- wgpu テクスチャを解放せずに `runtime_state = None` にすることを禁止
- Play 中のエラーを silent に捨てることを禁止（必ず Diagnostics に出す）

---

## Completion Criteria（完了基準）

- Play ボタンを押すと runtime world が生成される
- Game View パネルに runtime scene が描画される（Phase 11 前なので flat shading）
- Stop で editor の Edit モードに戻れる
- Play 失敗（scene 未選択・バリデーション失敗・no camera 等）が Diagnostics に表示される
- runtime world と authoring scene が独立しており、Stop 後に authoring は変わっていない

**10-E の完了基準（追補分）**:

- Play 中に Game View の現在フレームを `FrameCapture`（RGBA8 + サイズ）として取得できる
- 取得した画像のピクセル座標が `MouseInput.position` と同じ座標系である
- engine に画像エンコード・ネットワーク・AI SDK の依存が増えていない

---

## Feeds Into（次フェーズへの依存）

- Phase 11: Game View で描画される内容を改善する（深度・ライト・プリミティブ）
- Phase 13: Game View 内でプレイヤーを操作する（PlayerController）
- Phase 12-F: 仮想入力（`InputCommand`）と組み合わせ、AI / Replay / Test が
  「観測 → 入力注入」のループを回せるようになる（ADR 0026）
- AI Agent Bridge（先送り・旧称 Phase 26）: スクリーンショット送信・入力注入を MCP/CLI として公開する
  AI Agent Bridge
