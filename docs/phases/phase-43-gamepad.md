# Phase 43: Gamepad / Controller Input

> **着手前に ADR 0038 必須**（`gilrs` 依存の追加とプラットフォーム別ビルドゲート戦略）。

## Goal

Phase 12-F で定義した `GamepadButton` / `GamepadAxis` / `GamepadId` 型に
実デバイスを接続する。`gilrs`（デスクトップ）と Web Gamepad API（WASM）の両方で
同一の `InputCommand` インターフェースを通す。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

Phase 12-F で gamepad の型と `VirtualInputQueue` への注入パスは実装済みだが、
実デバイスの読み取りがない。インディーゲームでは gamepad 対応がほぼ必須。
Phase 42（Scripting）の後に置くことで、スクリプトから gamepad 入力を読む
APIを同時に整備できる。

---

## Scope

### 作るもの

- `gilrs` 統合（デスクトップ、ADR 0038 ゲート）（43-A）
- `EngineRunner` 内の gamepad イベントポーリングループ（43-A）
- `InputAction` への gamepad バインディング拡張（`project_settings.json`）（43-B）
- Project Settings Panel の gamepad remapping UI（43-B）
- WASM ターゲット向け Web Gamepad API バインディング（43-C、任意）

### 作らないもの

- 振動（rumble / force feedback）
- デバイス固有ボタンレイアウト（Xbox / DualSense の差異吸収）の完全なマッピング
  （Phase 43 ではシステム提供のボタン番号をそのまま使う）
- 複数コントローラーの同時サポートを超えたロビー管理

---

## Design Decisions

### `gilrs` は `target_arch != "wasm32"` でのみ依存（ADR 0038 で決定）

```toml
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
gilrs = "0.11"
```

WASM では別のバインディング（43-C）を使う。ADR 0038 でこの分岐を確定する。

### `VirtualInputQueue` へ注入する形を維持

既存の `InputCommand::GamepadButton` / `GamepadAxis` を使う。
`gilrs` イベントを直接 `VirtualInputQueue::push()` で注入するだけで、
ゲームコードは変更不要。

### 接続 / 切断イベントは `Diagnostic::info` で出力

```
"gamepad.connected" / "gamepad.disconnected" — Severity::Info
```

デバイス名を message に含め、Console に流す。

### `InputAction` の gamepad バインディングは Phase 34 の `ProjectSettings` を拡張

```json
{
  "name": "jump",
  "keys": ["Space"],
  "gamepad_buttons": [0]
}
```

`InputAction` に `gamepad_buttons: Vec<u32>` / `gamepad_axes: Vec<AxisBinding>` を追加する。
既存の `keys` フィールドとは独立して動作し、どちらか一方が押されれば Action が発火する。

---

## Implementation Plan

### 43-A: gilrs 統合（ADR 0038 ゲート）

1. `crates/engine/src/gamepad.rs`（新規）— `GilrsContext` 型
2. `EngineRunner::update()` 内で `gilrs.next_event()` をポーリング
3. `GamepadButton`・`GamepadAxis` を `VirtualInputQueue::push()` へ変換
4. 接続 / 切断を `Diagnostic::info` として `session.push_diagnostic()` に流す

### 43-B: Input Mapping Editor

- `ProjectSettings::input_actions` の `InputAction` に `gamepad_buttons` / `gamepad_axes` を追加
- Project Settings Panel に「Gamepad Bindings」テーブル UI
- `keys_for_action()` に相当する `gamepad_buttons_for_action()` を追加

### 43-C: WASM Web Gamepad API（任意）

- `wasm-bindgen` の `GamepadButton` API を `InputCommand` に変換
- `requestAnimationFrame` コールバック内でポーリング
- デスクトップと同一テストが通ることを確認

---

## Cautions（注意点・落とし穴）

**デッドゾーン**:
スティックのゼロ付近は物理的なノイズが乗る。`abs(value) < 0.1` をデッドゾーンとして
デフォルト設定し、Project Settings でチューニングできるようにする。

**プラットフォームドライバの差異**:
Windows（XInput）・Linux（evdev）・macOS で `gilrs` のボタン番号が異なる場合がある。
Phase 43 では番号をそのまま公開し、抽象化は後送りとする（ADR 0038 に明記）。

**`gilrs` の初期化失敗**:
ゲームパッドがない環境では `Gilrs::new()` が空の状態を返すが、エラーにはならない。
パニックしないことを確認する。

---

## Prohibited（禁止事項）

- ADR 0038 の Accept 前に `gilrs` を `Cargo.toml` に追加することを禁止
- 振動（rumble）API をこのフェーズで実装することを禁止
- `wasm32` ターゲットで `gilrs` を有効化することを禁止

---

## Completion Criteria（完了基準）

- Xbox / DualSense コントローラーで `PlayerController` が動く
- 接続 / 切断が Console に出る
- `VirtualInputQueue::push(GamepadButton)` の既存テストが通り続ける
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 46: WASM — 43-C の Web Gamepad API バインディングを WASM ビルドに組み込む
- Phase 42: Scripting — `ScriptContext` に `ctx.is_gamepad_pressed(button)` API を追加
