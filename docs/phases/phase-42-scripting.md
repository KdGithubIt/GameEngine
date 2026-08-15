# Phase 42: Scripting System

> **着手前に ADR 0037 必須**（Rhai via `rhai`、サンドボックスモデル、
> API surface、スクリプトアセット形式、diagnostics / profiler 方針）。

## Goal

エンジンを再コンパイルせずに、scene-specific なゲーム挙動を書ける
designer-facing scripting layer を追加する。Rhai は Rust native gameplay code を
置き換えるものではなく、Unity の MonoBehaviour 的に Entity へ attach する
`ScriptComponent` レイヤーとして扱う。

ただし Unity と違い、Rust 側に `DoorControllerComponent` や `EnemyAIComponent` を
毎回増やすのではなく、共通の `ScriptComponent { script: AssetRef, enabled, state }`
に `.rhai` asset を貼る方式にする。新しい `.rhai` script asset を増やしても
Rust の `cargo build` は不要。Rust build が必要なのは `ScriptContext` API、
Rust native component/system、engine/editor 実装を変更する場合だけにする。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

現状、ゲームロジックはすべて Rust でコンパイル時に決まる。非 Rustacean の
ゲーム制作者や designer が毎日触る production workflow では、軽量な
スクリプト層が必要になる。

MVP scripting language は **Rhai via `rhai`** とする。Lua 5.4 / `mlua` は
ゲームスクリプトとしての実績では有利だが、このエンジンでは Pure Rust 統合、
wasm32 への持ち込みやすさ、サンドボックス境界の明確さ、ビルド依存の軽さを優先する。

---

## Scope

### 作るもの

- ADR 0037 Rhai / `rhai` 採用・サンドボックス（42-A）
- `ScriptAsset { source: String }` — `.rhai` asset として manifest に登録（42-B）
- `ScriptEngine` resource — Rhai / `rhai` runtime wrapper と AST cache（42-B）
- `ScriptComponent` component — entity に Rhai script を attach する（42-C）
- MonoBehaviour 的 lifecycle hooks: `on_start`, `on_update`, `on_event`（42-C）
- ECS Binding API — command/event-oriented な `ScriptContext` facade（42-D）
- Diagnostics / profiler / max operation safety（42-E）
- Editor: Script asset 作成・表示・Console error・hot reload（42-F）
- Rhai から Rust native component/system への promotion policy document（42-G）

### 作らないもの

- Rhai だけでゲームロジック全体を書く設計
- Rhai から直接 ECS component type を新規定義すること
- raw ECS `World` / `AssetStorage` / filesystem / network / process への直接アクセス
- arbitrary module/package loading または script 側の任意 `require` / `import`
- heavy ECS query を script 内で自由に実行すること
- JIT コンパイル・ネイティブ FFI（サンドボックス外れる）
- Shadow / Gamepad / NavMesh / Post-process / WASM の本実装

Editor-defined data-only components は後続 Phase で検討してよいが、MVP では
Rust/editor が定義した component schema を `ScriptContext` 経由で読み書きするだけにする。

---

## Design Decisions

### MVP scripting language は Rhai via `rhai`

ADR 0037 で Rhai / `rhai` を採用する。Lua はゲームスクリプトとしての実績では有利だが、
Pure Rust 統合、wasm32 target との相性、ビルド依存の軽さ、エンジン側から登録した
API だけを公開しやすい点を優先する。

### Rust native components / systems remain first-class

Rust は正式な ECS component / system / 高速処理 / reusable gameplay module を担当する。
Rhai は scene-specific behavior / trigger / cutscene / UI flow /
prototype gameplay を担当する。Rhai を Rust native gameplay architecture の置換として
扱わない。

Rhai に向くもの:

- trigger
- simple state machine
- cutscene event
- UI flow
- scene-specific behavior
- prototype gameplay

Rust に残すもの:

- pathfinding
- physics
- collision
- animation sampling
- massive ECS query
- many-entity update
- renderer
- reusable gameplay module

### Rhai scripts are attached through `ScriptComponent`

Rhai script は entity の `ScriptComponent` から参照する。
`ScriptComponent` は script asset、enabled flag、実行順序、private script state を保持する。
private script state は script instance ごとに分離し、別 entity の state に直接触れない。

### MonoBehaviour-style lifecycle hooks

MVP hook は以下に限定する:

- `on_start(ctx)`
- `on_update(ctx, dt)`
- `on_event(ctx, event)`

初期実装では、hook が存在しない script は単に skip する。missing hook は error にしない。

後続候補として以下を optional / future にする:

- `on_collision_enter(ctx, other)`
- `on_trigger_enter(ctx, other)`
- `on_enable(ctx)`
- `on_disable(ctx)`

### `ScriptContext` が ECS boundary を制御

Rhai 側からは直接 `World` にアクセスできない。MVP の `ScriptContext` API は
command/event-oriented に制限する。

MVP API 例:

- `ctx.self()`
- `ctx.log(message)`
- `ctx.input_pressed(action)`
- `ctx.send_event(target, event_name)`
- `ctx.get_component(entity, component_name)`
- `ctx.set_component(entity, component_name, value)`
- `ctx.param(name)`

raw ECS `World`、raw AssetStorage、filesystem、network、process、
arbitrary module/package loading、script 側の任意 `require` / `import`、
heavy ECS query を script 内で自由に実行することは禁止する。

### Script state / hot reload

`.rhai` 保存時は script asset を再読み込みし、Rhai AST に compile し直す。
frame 中には compile しない。compile 済み AST は script asset 単位で cache する。

同じ script を複数 Entity に貼る場合、AST は共有し、Entity ごとに
`ScriptInstance` / private state を分ける。MVP の hot reload では
script private state は原則 reset でよい。

重要な状態、保存すべき状態、Inspector に出すべき状態は Rust native component
または将来の editor-defined data component に置く。script private state は
一時的な timer / local flag / phase 程度に限定する。

### Script diagnostics / profiler

`ScriptComponent` の処理負荷は見える化する。最低限記録するもの:

- total script time per frame
- per-script execution time
- per-entity script execution time
- hook 別実行時間: `on_start`, `on_update`, `on_event`
- script compile time
- last / average / max time
- slow script warning
- script error diagnostic

profiler mode / debug mode では可能なら以下を記録する:

- Rhai operation count
- `ScriptContext` API call counts: `get_component`, `set_component`, `send_event`, `input_pressed`, `log`
- max operations exceeded
- per-frame top N slow scripts

表示イメージ:

```text
Script Profiler

Total script time: 1.42 ms / frame

Top Scripts:
1. enemy_ai.rhai        0.82 ms   120 calls   45,000 ops
2. door_interact.rhai   0.12 ms    30 calls    3,200 ops

Entity:
Slime_023 / enemy_ai.rhai
  on_update last: 0.031 ms
  avg: 0.018 ms
  max: 0.071 ms
  ctx calls:
    get_component: 3
    send_event: 1
```

### Rhai safety limit

Rhai `Engine::set_max_operations` を使い、runaway script を止める。
operation limit 超過は Console / Diagnostic に出す。detailed operation profiling は
overhead があるため editor/debug/profiler mode 限定でよい。
release runtime では wall-clock timing と max operation limit を優先する。

### Rhai から Rust への昇格方針

Rhai は試作・軽い挙動・シーン固有ロジック用に使う。重い処理を Rhai に残し続ける
方針ではない。Script profiler / diagnostics で、重い script、重い Entity、
重い lifecycle hook を特定できるようにする。

`ScriptContext` API は小さく command/event-oriented に保つ。Rhai script から
raw ECS `World` を直接触らせない。Rhai と Rust の両方から同じ command / event を
使えるようにし、重い処理は Rust native function / Rust system / Rust component に
逃がせる設計にする。

script が performance-critical / reusable / stable gameplay logic になった場合は、
Rust native component/system へ promote する。promotion は完全自動変換ではなく、
手動または AI 補助による Rust 実装への移植とする。

ボタン一発で `.rhai` を Rust component/system に完全保証変換する仕組みは非目標。
必要なら将来 `engine-cli promote-script` のような Rust 雛形生成コマンドは検討してよいが、
挙動保証つきの完全変換は非目標とする。

### エラーは `Diagnostic` として出力

Rhai parse error / runtime error / max operations exceeded / slow script warning は
`Severity::Error` または `Severity::Warning` として `AuthoringSession::diagnostics()` に追加し、
Console / Problems に表示する。

---

## Implementation Plan

### 42-A: ADR 0037 / Rhai scripting runtime design

ADR 0037 を更新し、以下を明記して Accept する:

- Rhai は Rust native gameplay code を置き換えない
- Rust native components/systems remain first-class
- `ScriptComponent { script: AssetRef, enabled, state }` に `.rhai` asset を attach する
- `.rhai` asset 追加・編集では Rust build 不要
- `ScriptContext` API / Rust component/system / engine/editor 変更時だけ Rust build が必要
- API surface は command/event-oriented な `ScriptContext` facade 経由に限定する
- diagnostics / profiler / max operation safety を MVP contract に含める

### 42-B: ScriptAsset / ScriptEngine / AST cache

```
assets/scripts/enemy_ai.rhai
```

- `ScriptAsset { source: String, asset_id: AssetId }` — `AssetManifest` に `kind = "script"` で登録
- `ScriptEngine` — Rhai / `rhai` runtime wrapper（`World` resource として保持）
- `.rhai` 保存時に script asset を再読み込みし、Rhai AST に compile
- frame 中には compile しない
- compile 済み AST は script asset 単位で cache
- 同じ script を複数 Entity に貼る場合、AST は共有する
- script compile time を profiler / diagnostics 用に記録

### 42-C: ScriptComponent lifecycle hooks

- `ScriptComponent { script, enabled, order, state }` — entity component
- Entity ごとに `ScriptInstance` / private state を分ける
- MVP hook:
  - `on_start(ctx)`
  - `on_update(ctx, dt)`
  - `on_event(ctx, event)`
- missing hook は skip
- future hook:
  - `on_collision_enter(ctx, other)`
  - `on_trigger_enter(ctx, other)`
  - `on_enable(ctx)`
  - `on_disable(ctx)`
- MVP hot reload では private script state reset でよい

### 42-D: ScriptContext ECS facade

```rhai
fn on_start(ctx) {
    ctx.log("enemy started");
    ctx.set_component(ctx.self(), "engine.transform", #{ y: 1.0 });
}

fn on_update(ctx, dt) {
    if ctx.input_pressed("interact") {
        ctx.send_event(ctx.self(), "enemy.interact");
    }
}

fn on_event(ctx, event) {
    ctx.log(event);
}
```

- `ScriptContext` MVP API:
  - `self() -> EntityId`
  - `log(message)`
  - `input_pressed(action) -> bool`
  - `send_event(target, event_name)`
  - `get_component(entity, component_name) -> ScriptValue`
  - `set_component(entity, component_name, value)`
  - `param(name) -> ScriptValue`
- raw ECS `World` / raw AssetStorage / filesystem / network / process へ直接触らせない
- arbitrary module/package loading、任意 `require` / `import` を禁止
- heavy ECS query を script 内で自由に実行させない

### 42-E: Script diagnostics / profiler / max operation safety

- `total script time per frame`
- `per-script execution time`
- `per-entity script execution time`
- hook 別実行時間: `on_start`, `on_update`, `on_event`
- `script compile time`
- `last / average / max time`
- slow script warning
- script parse/runtime error diagnostic
- Rhai `Engine::set_max_operations` による runaway script 防止
- operation limit 超過を Console / Diagnostic に出す
- profiler/debug mode では Rhai operation count、`ScriptContext` API call counts、
  per-frame top N slow scripts を記録してよい

### 42-F: Editor integration / Console errors / hot reload

- Asset Browser で `.rhai` を `[script]` バッジ表示
- Inspector: `ScriptComponent` component に Script ドロップダウン
- Console に Rhai parse/runtime/max-operation error を `code: "script.parse_error"` /
  `code: "script.runtime_error"` / `code: "script.max_operations_exceeded"` で出力
- slow script warning を `code: "script.slow"` で出力
- Script asset 新規作成ボタン（`enemy_ai.rhai` 形式のテンプレート生成）
- `.rhai` 保存時 hot reload。MVP では private script state reset でよい

### 42-G: Rust promotion policy documentation

- prototype gameplay logic は Rhai でよい
- profiler / diagnostics で重い script、Entity、lifecycle hook を特定する
- `ScriptContext` API を小さく command/event-oriented に保つ
- Rhai と Rust の両方から同じ command / event を使えるようにする
- 重い処理は Rust native function / Rust system / Rust component に逃がせる設計にする
- performance-critical / reusable / stable gameplay logic は Rust native component/system へ promote
- promotion は手動または AI 補助による Rust 実装への移植とする
- ボタン一発の `.rhai` → Rust component/system 完全保証変換器は非目標
- 将来 `engine-cli promote-script` のような Rust 雛形生成コマンドは検討してよいが、挙動保証つき完全変換は非目標

---

## Cautions（注意点・落とし穴）

**無限ループ防止**:
Rhai script に `while true {}` を書かれるとゲームがフリーズする。
Rhai `Engine::set_max_operations` と wall-clock timing を設定する。

**サンドボックス境界**:
filesystem / network / process / dynamic module loading に触れる関数や package は登録しない。
外部 module/package loading は engine-approved module table のみに限定するか、MVP では無効化する。

**private state の lifetime**:
`ScriptComponent` の state は Play session lifetime に紐づく。重要状態は Rust native component
または将来の editor-defined data component に置き、MVP では scene file に runtime state を保存しない。

**複数 script の実行順序**:
同一 entity に複数 `ScriptComponent` がある場合は `order` で実行する。
未指定順序に依存する挙動は禁止し、同値の場合は deterministic な asset id 順にする。

**profiling overhead**:
detailed operation profiling は overhead がある。release runtime では wall-clock timing と
max operation limit を優先する。

---

## Prohibited（禁止事項）

- ADR 0037 の Accept 前にコードを書くことを禁止
- Rhai から直接 `unsafe` Rust を呼べる binding を禁止
- Rhai から raw ECS `World` / raw `AssetStorage` を触る API を禁止
- filesystem / network / process / arbitrary module/package loading を Rhai に公開することを禁止
- script から任意 `require` / `import` を許可することを禁止
- heavy ECS query を script 内で自由に実行させることを禁止
- Rhai で新しい ECS component type を直接定義することを禁止

---

## Completion Criteria（完了基準）

- `assets/scripts/enemy_ai.rhai` の Rhai script で敵 AI の移動が動く
- Rust native component / system が引き続き first-class として動く
- Rhai script は `ScriptComponent` から attach される
- missing hook は skip される
- `.rhai` asset の AST cache が frame 中 compile を避ける
- Rhai script の parse/runtime/max-operation error が Console に表示される
- slow script warning と script profiler metrics が確認できる
- raw `World` / filesystem / network / process へ Rhai からアクセスできない
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 43: Gamepad — `ScriptContext` に input action mapping を接続
- Phase 44: NavMesh — `ScriptContext` に高レベル navigation command を追加
- Phase 46: WASM — Rhai / `rhai` の wasm32 build behavior を ADR 0041 で確認
