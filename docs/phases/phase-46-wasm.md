# Phase 46: WASM / Web Build Target

> **着手前に ADR 0041 必須**（wasm32 build strategy、asset loading、web runtime boundary）。

## Goal

エンジンの最小 runtime を WebGPU / wasm32 ターゲットでビルドし、ブラウザ上で
sample scene を実行できるようにする。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

Phase 41-45 で runtime rendering path が拡張された後に web target を確認する。
desktop 依存（filesystem、native dialogs、`gilrs`、threading、Rhai / `rhai` の wasm32 build behavior）を
ADR 0041 で整理してから実装する必要がある。

---

## Scope

### 作るもの

- `wasm32-unknown-unknown` build gate と target-specific dependency 整理（46-A）
- Web asset loading path（HTTP / embedded manifest、ADR 0041 で決定）（46-B）
- WebGPU canvas bootstrap（46-C）
- Minimal sample scene の browser smoke test（46-D）

### 作らないもの

- Web editor
- Full packaging / hosting pipeline
- Native filesystem access
- Browser-specific gameplay API の一般公開
- Phase 42 scripting の wasm32 対応保証（ADR 0041 で可否を決める）

---

## Design Decisions

### Web target strategy は ADR 0041 で決定

`wasm-bindgen` / `trunk` / custom build script のどれを採用するか、asset の配置、
WebGPU feature fallback、panic logging を ADR 0041 で固定する。

### Desktop-only dependencies are target-gated

`gilrs`、native dialogs、process/filesystem 前提の機能は `cfg(not(target_arch = "wasm32"))`
で分離する。web gamepad は Phase 43-C の Web Gamepad API path を使う。

### Rhai / `rhai` wasm32 behavior is explicitly confirmed here

Phase 42 では Rhai scripting を MVP baseline とする。ADR 0041 で
Rhai を web build でも有効にするための feature gate、制限事項、無効化条件を確認する。

---

## Implementation Plan

### 46-A: Build Target Gate（ADR 0041 ゲート）

1. workspace の target-specific dependencies を整理
2. desktop-only code に `cfg` boundary を追加
3. `cargo build --target wasm32-unknown-unknown` の最小 path を確立

### 46-B: Asset Loading

- sample project assets を browser から取得できる layout にする
- blocking filesystem API を runtime path から除外

### 46-C: WebGPU Bootstrap

- canvas から `wgpu::Surface` を作成
- resize / device lost / panic hook を最低限処理

### 46-D: Browser Smoke Test

- sample scene を描画
- keyboard input と optional gamepad input を確認
- screenshot または pixel check で nonblank を確認

---

## Prohibited（禁止事項）

- ADR 0041 の Accept 前に wasm target 固有 code / dependency を追加することを禁止
- desktop-only dependency を wasm32 に漏らすことを禁止
- Web editor をこの Phase に含めることを禁止

---

## Completion Criteria（完了基準）

- `wasm32-unknown-unknown` target の build が通る
- browser canvas に sample scene が描画される
- keyboard input が browser 上で動く
- desktop target の既存 tests が通り続ける

---

## Feeds Into（次フェーズへの依存）

- Phase 47: GPU Instancing & LOD — WebGPU 上の instance draw compatibility を確認する
