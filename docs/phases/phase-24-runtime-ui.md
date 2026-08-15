# Phase 24: Runtime UI — In-Game HUD / Menu

> **2026-06-13 再構成**: 旧 Phase 16。egui / egui-winit / egui-wgpu の runtime 依存は
> このフェーズで追加する。旧→新対応表は
> `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` を参照。

## Goal

ゲーム実行中にスコア・HP・メニューなどの UI をゲーム画面に重ねて表示できるようにする。  
`egui` をゲームの HUD レンダリングエンジンとして runtime world から使えるようにする。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**Phase 15 の後にこのフェーズが来る理由**:  
シーン切り替えができると「タイトル画面（UI のみ）」「ゲーム画面（HUD あり）」「ゲームオーバー画面」という
流れが作れる。Phase 15 なしでは UI が意味をなさない。

**なぜ Runtime UI が必要か**:  
スコア・残り時間・HP バー・ゲームオーバー表示がないゲームは不完全。  
Phase 20 のサンプルゲームには最低限の HUD が必要。

**egui を選ぶ理由**:  
editor（Phase 9〜）で既に `egui` を使っている。同じライブラリを runtime HUD でも使うことで、
新しいレンダリングシステムを追加せずに済む。  
独自のスプライトベース UI システムを作るのはスコープが大きすぎる（Phase 21 以降で検討）。

**egui は immediate mode なので状態を外部に持つ**:  
`egui` は毎フレーム UI を記述し直す即時モード GUI。  
HUD の「状態（スコア・HP）」は ECS の component / resource として持ち、
毎フレームそれを読んで UI を描画する。

---

## Scope

### 作るもの

- `UiSystem` trait — ゲームシステムが毎フレーム egui を書けるインターフェース
- `UiContext` resource — egui の `Context` への安全なラッパー
- `EcsApp::add_ui_system()` — UI 描画システムの登録
- `HudLabel` / `HudRect` コンポーネント — エンティティに紐づく単純な HUD 要素（オプション）
- Game View への egui オーバーレイ（egui を Game View の上に描画）

### 作らないもの

- Canvas / 9-slice スプライト UI（専用 UI システムは Phase 21 以降）
- アニメーション（フェードイン・スライドイン）
- World-space UI（3D 空間に浮かぶ UI）— Screen-space のみ
- Input によるフォーカス管理（Tab キー操作）

---

## Design Decisions

### なぜ `UiSystem` trait を使うか（クロージャ方式との比較）

クロージャ方式（`app.add_ui(|ctx, world| { ... })`）は型消去が必要で、
`world` の借用ライフタイムが複雑になる。  
`UiSystem` trait の実装を struct に持たせることで、ECS の `System` trait と同じパターンで扱える。  
将来的に `SystemParam` を使った依存注入にも対応しやすい。

### egui を Game View にオーバーレイする方法

Game View は wgpu テクスチャに描画されている（Phase 10 参照）。  
そのテクスチャを egui `Image` として表示した後、同じ Panel の上に `egui` の子ウィジェットを
重ねて描画することで HUD を表現する。  
`egui::Area` に `interactable(false)` を設定して、クリックが素通りするレイヤーを作る。

### なぜ `UiContext` を resource として world に挿入するか

UI システムも ECS の system であるため、`Res<UiContext>` として受け取れるようにする。  
`egui::Context` の内部は `Arc` で参照カウント管理されており、clone しても安全。  
resource にすることで複数の UI システムが同じ Context を共有できる。

---

## Implementation Plan

### 16-A: UiContext Resource

```rust
// crates/engine/src/ui.rs（新規）
pub struct UiContext {
    pub ctx: egui::Context,
    pub viewport: UiViewport,  // Game View の表示矩形 + ターゲット画面サイズ
}

impl UiContext {
    pub fn new(ctx: egui::Context, viewport: UiViewport) -> Self;
    pub fn viewport_rect(&self) -> egui::Rect;
}
```

> **ADR 0090 で変更**: `viewport_rect` 単独では「HUD を配置する矩形」と
> 「レイアウトの基準になるターゲット画面サイズ」が区別できず、エディタが
> 画面を縮小表示すると HUD の占有率が変わってしまう。`UiViewport` が両方を
> 持ち、`UiContext::viewport_rect()` は表示矩形を返す。

Play 開始時に `RuntimePlayState` に egui Context を持たせ、world に resource として挿入する。

### 16-B: UiSystem trait

```rust
pub trait UiSystem: Send + Sync + 'static {
    fn run(&mut self, ctx: &egui::Context, world: &World);
}

// システム関数でも使えるようにフォールバック実装
impl<F> UiSystem for F
where
    F: Fn(&egui::Context, &World) + Send + Sync + 'static,
{
    fn run(&mut self, ctx: &egui::Context, world: &World) {
        (self)(ctx, world);
    }
}
```

### 16-C: EcsApp::add_ui_system

```rust
// crates/ecs/src/app.rs
impl App {
    pub fn add_ui_system(&mut self, system: impl UiSystem) -> &mut Self;
}
```

フレームの通常 update 後、egui 描画前に `UiSystem::run` を呼ぶ。

### 16-D: HUD の例（サンプルコード）

```rust
// ゲームコード側での使用例
struct ScoreHud;
impl UiSystem for ScoreHud {
    fn run(&mut self, ctx: &egui::Context, world: &World) {
        let score = world.get_resource::<Score>().map(|s| s.value).unwrap_or(0);
        egui::Area::new(egui::Id::new("hud_score"))
            .fixed_pos(egui::pos2(10.0, 10.0))
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(format!("Score: {score}"))
                    .color(egui::Color32::WHITE)
                    .size(24.0));
            });
    }
}

app.add_ui_system(ScoreHud);
```

### 16-E: Game View オーバーレイの描画順序

```
1. ECS update()
2. UI systems: UiSystem::run() 全部呼ぶ（egui コマンドをバッファに溜める）
3. wgpu でゲームシーンをテクスチャに描画
4. egui の CentralPanel に Game View テクスチャを表示
5. egui が UI コマンドを上からレンダリング（HUD が前面に来る）
```

---

## Cautions（注意点・落とし穴）

**egui の `Context` はスレッド安全だが、`world` は `&World` のみ**:  
`UiSystem::run` は `&World`（不変参照）しか受け取れない。  
UI からゲームの状態を変更したい場合は `Commands` 経由で行う（次フレームに反映）。

**`egui::Area` の Z オーダー**:  
複数の `egui::Area` を重ねると、後で描画した方が上に来る。  
`order(egui::Order::Foreground)` で前面に強制できる。

**Game View テクスチャのサイズと egui の座標**:  
`UiContext::viewport_rect()` でゲーム画面の egui 座標上の領域を把握しておく。  
HUD を画面外に描かないようこの矩形内に収める。

**egui のフォント**:  
デフォルトフォントは英数字のみ。日本語テキストを HUD に表示する場合は
`FontDefinitions` に日本語フォントを追加する必要がある。  
このフェーズでは英数字のみの HUD を前提とする。

---

## Prohibited（禁止事項）

- `UiSystem::run` の中で `world` を mutably 借用することを禁止（`&World` のみ）
- 独自 Canvas / スプライト UI システムをこのフェーズで実装することを禁止
- World-space UI（3D 空間に浮かぶテキスト）をこのフェーズで実装することを禁止

---

## Completion Criteria（完了基準）

- `app.add_ui_system(ScoreHud)` でスコアが Game View に重なって表示される
- HUD が Game View のテクスチャより前面に描画されている
- Stop で editor に戻ったとき HUD が消える
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 20: sample game でスコア・残り時間・ゲームオーバー画面を UiSystem で実装
