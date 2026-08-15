# Phase 12: Runtime Foundation — Time / Input / Fixed Update

## Goal

ゲームを安定して動かすための時間・入力・固定更新の基盤を整える。  
Time と Input の大半は既に実装済みなので、このフェーズの主な作業は
Fixed Timestep と Virtual Input Layer（12-F、2026-06-11 追加・ADR 0026）。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**Fixed Timestep が Phase 22（物理、旧 18）の前提**:  
物理計算（velocity + gravity + push-out）はフレームレートに依存してはいけない。
フレームが落ちた時に物体が壁を突き抜けたり、逆に高フレームレートで物理が速くなったりする。
Fixed Timestep を先に作ることで、Phase 22 の物理が安定する。

**Time と Input はほぼ実装済み**:  
`Time { delta_seconds, elapsed_seconds, frame_count }` / `Input<KeyCode>` / `Input<MouseButton>` /
`MouseInput` はすべて `crates/engine/src/` に実装済み。
Phase 12 で新規実装が必要なのは Fixed Timestep のみ。

**Action Mapping はこのフェーズで実装しない**:  
Action Mapping（キーコードをゲームアクションに変換する仕組み）は便利だが、
Phase 25（フル sample game、旧 20）で実際に必要になるまで実装を後回しにする。
早すぎる抽象化より、直接 `Input<KeyCode>` を使う具体的なシステムの方が今は適切。

---

## Scope

### 作るもの

- `Time` に `frame_count: u64` を追加（完了 2026-06-11）
- `FixedTime` リソース（fixed_delta + accumulator）
- `EcsApp::add_fixed_system()` API
- `EngineRunner` での fixed update ループ
- **12-F（追加）**: `InputCommand` / `InputSource` / `VirtualInputQueue` と、
  キューを既存の `Input<KeyCode>` / `Input<MouseButton>` / `MouseInput` に
  反映する drain 処理（ADR 0026）

### 作らないもの

- Action Mapping（Phase 25 まで不要）
- 既存 `Input<T>` / `MouseInput` の構造変更（仮想入力は既存リソースへの
  「書き込み元の追加」であり、読み手の API は変えない）
- Runtime Debug Overlay の egui 統合（Phase 24〔旧 16〕と一緒にやる）
- ゲームパッドの実デバイス対応（`gilrs` 等の導入は将来。12-F では
  `GamepadId` / `GamepadButton` / `GamepadAxis` を**型として定義するだけ**）
- OS レベルの入力合成（enigo / SendInput 等）— 恒久的に禁止（ADR 0026）
- AI API 通信・スクリーンショット送信（AI Agent Bridge〔旧称 Phase 26〕で engine 外に作る）
- Replay ファイル形式の凍結（`InputCommand` のシリアライズは別 ADR が必要）

---

## Design Decisions

### なぜ Fixed Update を別のシステムリストにするか

Fixed Update のシステムは `delta_seconds` ではなく `fixed_delta` を使う必要がある。
通常の `update()` の中に混ぜると「このシステムは fixed か可変か」が分かりにくくなる。
`EcsApp` に `systems`（可変）と `fixed_systems`（固定）の 2 つのリストを持つ。

### なぜ accumulator の上限を設けるか

PC が長時間止まった後（スリープから復帰、デバッガで一時停止等）に再開すると、
`delta` が数秒〜数分になる。その分だけ fixed update を回そうとすると何百回も実行されて
フリーズしたように見える。
`accumulator = accumulator.min(fixed_delta * 5.0)` で最大 5 ステップに制限する。

### なぜ `fixed_delta` のデフォルトを `1.0 / 60.0` にするか

一般的なゲームの物理更新頻度。60fps よりも高い固定更新は CPU 負荷が増えるだけで
体感差が少ない。より精密な物理が必要になった時点でユーザーが変更できる。

### なぜ仮想入力を OS レベルではなく engine 内に作るか（12-F / ADR 0026）

AI に OS のマウス・キーボードを操作させると、ウィンドウフォーカスと OS の
タイミングに依存し、開発者の実入力を乗っ取り、CI でテストできない。
engine 内の `VirtualInputQueue` に `InputCommand` を積み、既存の入力リソースに
反映する方式なら、AI / Replay / Test が同じ注入経路を共有でき、ゲームシステム側は
`Input<KeyCode>` 等を読むだけで入力元を意識しなくてよい。

### なぜ `InputSource` を持つか

入力コマンドに Human / AiAgent / Replay / Test のタグを付けることで、
将来「Replay 再生中は人間入力を無視する」「AI 入力だけログする」といった
ポリシーをキュー側で実装できる。ゲームシステムには伝播させない
（システムが入力元で分岐し始めると再現性が壊れるため）。

### なぜ drain を ECS システムではなく明示的な関数呼び出しにするか

現在の ECS にはシステム間の順序保証がない。drain がユーザーシステムの後に
実行されると入力が 1 フレーム遅れる。`EngineRunner` と editor の
`RuntimePlayState::tick` が「`clear_transitions()` の後・schedule 実行の前」に
明示的に呼ぶことで、winit イベントと同じタイミング・同じ
`just_pressed` / `just_released` セマンティクスを保証する。

### なぜゲームパッドを「型だけ」先に定義するか

`GamepadAxis { value: f32 }` は最初のアナログ入力であり、`InputCommand` が
ボタン状態だけのモデルにならないよう設計段階で形に影響する。一方で実デバイス
対応（`gilrs` 等）は現時点で顧客がいない。型を `#[non_exhaustive]` な enum の
バリアントとして予約しておけば、後からバックエンドを足しても破壊的変更にならない。

---

## Implementation Plan

> **スケジュール注記**: `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` により、
> Phase 12-B（Fixed Timestep: 12-B-1〜12-B-3）は Phase 21（collision、旧 17）の先頭タスクとして実施する。
> Phase 12-A（frame_count）は 2026-06-11 に完了。

### 12-A: Time に frame_count を追加（完了 2026-06-11）

```rust
// crates/engine/src/time.rs
pub struct Time {
    pub delta_seconds: f32,
    pub elapsed_seconds: f32,
    pub frame_count: u64,
}
```

`EngineRunner` の `RedrawRequested` と editor の `RuntimePlayState::tick` で
schedule 実行前に `time.frame_count` を進める。

### 12-B-1: FixedTime リソース

```rust
// crates/engine/src/time.rs に追加
pub struct FixedTime {
    pub fixed_delta: f32,
    pub(crate) accumulator: f32,
}

impl Default for FixedTime {
    fn default() -> Self {
        Self { fixed_delta: 1.0 / 60.0, accumulator: 0.0 }
    }
}
```

### 12-B-2: EcsApp の fixed_systems

```rust
// crates/ecs/src/app.rs
pub struct App {
    world: World,
    systems: Vec<Box<dyn System>>,
    fixed_systems: Vec<Box<dyn System>>,   // 追加
}

impl App {
    pub fn add_fixed_system<P, M>(&mut self, system: impl IntoSystem<P, M>) -> &mut Self;
    pub fn run_fixed_update(&mut self) -> Result<(), AppError>;
    // fixed_systems を 1 回実行する
}
```

### 12-B-3: EngineRunner での fixed update ループ

```rust
// crates/engine/src/app.rs の RedrawRequested 処理内
let delta = /* フレーム時間 */;

// fixed update を必要回数実行
{
    let fixed = world.get_resource_mut::<FixedTime>().unwrap();
    fixed.accumulator += delta;
    // ヒッチ（長い停止後）でも最大 5 ステップに制限
    fixed.accumulator = fixed.accumulator.min(fixed.fixed_delta * 5.0);
}

loop {
    let (should_step, fixed_delta) = {
        let fixed = world.get_resource::<FixedTime>().unwrap();
        (fixed.accumulator >= fixed.fixed_delta, fixed.fixed_delta)
    };
    if !should_step { break; }

    self.app.ecs.run_fixed_update()?;

    let fixed = world.get_resource_mut::<FixedTime>().unwrap();
    fixed.accumulator -= fixed_delta;
}

// その後に通常 update を実行
self.app.ecs.update()?;
```

### 12-F: Virtual Input Layer（2026-06-11 追加・同日実装済み・ADR 0026 Accepted）

**目的**: 人間の winit 入力だけでなく、AI エージェント・Replay・テストが
engine 内の仮想入力としてキーボード / マウス /（将来）ゲームパッド入力を
注入できるようにする。

```rust
// crates/engine/src/input.rs（または兄弟モジュール）
#[non_exhaustive]
pub enum InputSource { Human, AiAgent, Replay, Test }

#[non_exhaustive]
pub enum InputCommand {
    Key { key: KeyCode, pressed: bool },
    MouseButton { button: MouseButton, pressed: bool },
    MouseMove { position: (f32, f32) },   // レンダーターゲットの物理ピクセル座標
    MouseDelta { delta: (f64, f64) },
    MouseScroll { amount: f32 },
    // gamepad は型の予約のみ。実デバイス対応は将来
    GamepadButton { gamepad: GamepadId, button: GamepadButton, pressed: bool },
    GamepadAxis { gamepad: GamepadId, axis: GamepadAxis, value: f32 },
}

/// ECS リソース。push された (InputSource, InputCommand) を
/// フレーム境界で既存の入力リソースへ反映する
pub struct VirtualInputQueue { /* Vec<(InputSource, InputCommand)> */ }
```

**反映タイミング（重要）**: drain は「前フレームの `clear_transitions()` の後・
schedule 実行の前」。winit イベントが反映されるのと同じ位置に置くことで、
注入入力でも `just_pressed` / `just_released` が 1 フレームだけ立つ。

**MouseMove の座標系**: 10-E の `FrameCapture` と同じ物理ピクセル座標。
AI はスクリーンショット上のピクセル座標をそのまま `MouseMove` に渡してクリックできる。

**タスク分解（各 30 分〜1 時間）** — 12-F-1〜12-F-7 すべて完了（2026-06-11）:

| # | タスク | 場所 |
|---|--------|------|
| 12-F-1 | `InputSource` / `InputCommand` 型定義 + rustdoc + ユニットテスト | `crates/engine/src/input.rs` |
| 12-F-2 | `GamepadId` / `GamepadButton` / `GamepadAxis` 型定義（データのみ、バックエンドなし） | 同上 |
| 12-F-3 | `VirtualInputQueue` リソース（push / drain、`App::new()` で挿入） | 同上 + `app.rs` |
| 12-F-4 | drain 関数: キューを `Input<KeyCode>` / `Input<MouseButton>` / `MouseInput` に反映（gamepad コマンドは受理するが現状は no-op、transition テスト付き） | `crates/engine/src/input.rs` |
| 12-F-5 | `EngineRunner` の RedrawRequested に drain 呼び出しを追加 | `crates/engine/src/app.rs` |
| 12-F-6 | editor: `RuntimePlayState::tick` に `clear_transitions` / `prepare_frame` / drain を追加（editor tick が transition をクリアしていなかった点も同時に修正済み） | `crates/editor/src/runtime.rs` |
| 12-F-7 | 注入の統合テスト: `Key { pressed: true }` 注入 → tick → システムから `just_pressed` が見える → 次 tick で消える | `crates/engine` tests + editor `runtime.rs` tests |

**今すぐ実装するもの / 将来の拡張**:

- 今すぐ（12-F として実装可能。GPU 不要・engine 単体で完結）: 12-F-1〜12-F-7
- 将来:
  - winit 人間入力を `InputCommand`（`InputSource::Human`）経由に一本化する
    `EngineRunner` リファクタ（公開 API 影響なし・ADR 0026 Decision 3）
  - `gilrs` 等によるゲームパッド実デバイス対応 + `Input<GamepadButton>` リソース
  - Replay の記録・再生（`InputCommand` のシリアライズ形式は別 ADR で凍結）
  - MCP/CLI への入力注入ツール公開（AI Agent Bridge〔旧称 Phase 26〕）

---

## Cautions（注意点・落とし穴）

**`run_fixed_update` の中で `Time.delta_seconds` を使わない**:  
Fixed Update のシステムは `FixedTime.fixed_delta` を使う。
`Time.delta_seconds` は可変フレームの値なので fixed システムで使うと不安定になる。

**Fixed システムと通常システムの実行順序**:  
Fixed Update → 通常 Update の順で実行する。
逆にすると物理の結果が 1 フレーム遅れて表示される。

**accumulator の取り扱い**:  
`accumulator` は `pub(crate)` にして外部から直接変更できないようにする。
直接変更されると固定更新のタイミングが崩れる。

**drain のタイミングを固定する（12-F）**:  
drain を schedule 実行の後ろや途中に置くと、注入入力の `just_pressed` が
システムの実行順に依存して見えたり見えなかったりする。必ず
「`clear_transitions()` の後・schedule 前」の 1 箇所に置く。

**editor tick の transition クリア漏れ（12-F-6）**:  
editor の `RuntimePlayState::tick` は現状 `clear_transitions()` /
`prepare_frame()` を呼んでいない。仮想入力を editor Play に注入できるように
する際、これを先に直さないと `just_pressed` が立ちっぱなしになる。

---

## Prohibited（禁止事項）

- Fixed システムで `Res<Time>` の `delta_seconds` を使うことを禁止（`Res<FixedTime>` を使う）
- accumulator の上限チェックを省略することを禁止（フリーズの原因になる）
- Action Mapping をこのフェーズで実装することを禁止（スコープ外・Phase 25 まで待つ）
- OS レベルの入力合成（enigo / SendInput / ウィンドウメッセージ注入）を禁止（ADR 0026）
- engine に AI API 通信・画像エンコード・ネットワーク依存を追加することを禁止
  （AI ブリッジは AI Agent Bridge フェーズで engine 外に作る）
- ゲームシステムが `InputSource` で分岐することを禁止（システムは既存の
  `Input<T>` を読むだけ。入力元ポリシーはキュー側で扱う）
- `InputCommand` にシリアライズを足して Replay 形式を黙って凍結することを禁止（別 ADR が必要）

---

## Completion Criteria（完了基準）

- `app.add_fixed_system(my_system)` が動作する
- fixed system が 60Hz 相当で実行される
- フレームレートが 30fps に落ちても fixed update は 60Hz で実行される
- 長時間停止後の再開で 5 ステップ以上の fixed update が実行されない

**12-F の完了基準（追補分）**:

- テストコードから `VirtualInputQueue` に `InputCommand` を push し、tick 後に
  システムから `pressed` / `just_pressed` / `just_released` が正しく観測できる
- editor Play 中の runtime world にも同じ経路で注入できる
- `GamepadButton` / `GamepadAxis` コマンドがコンパイル・受理される
  （反映先リソースは未実装でよい）
- 既存の winit 入力経路が壊れていない（実機で WASD / マウスが従来どおり動く）

---

## Feeds Into（次フェーズへの依存）

- Phase 22（旧 18）: 物理システム（velocity + gravity）を fixed update システムとして登録する
- Phase 13: PlayerController を仮想入力でテストできる（WASD 注入 → Transform 変化を assert）
- AI Agent Bridge（先送り・旧称 Phase 26）: `VirtualInputQueue` への注入と
  10-E の `FrameCapture` を MCP/CLI ツールとして公開する（ADR 0026）
