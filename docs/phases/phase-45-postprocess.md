# Phase 45: Post-Processing Pipeline

> **着手前に ADR 0040 必須**（HDR render target、tonemapping、post-process pass 構成）。

## Goal

HDR render target と post-processing pass を追加し、tonemapping / bloom /
color grading などを段階的に載せられる描画パイプラインにする。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

Phase 41 の Shadow Mapping & Environment Lighting で 2-pass rendering と
environment lighting の基礎が入った後に、HDR と画面全体の後処理を導入する。
Shadow / IBL の結果を LDR backbuffer に直接書く前提のまま post-process を足すと
render target contract が崩れるため、ADR 0040 で pass 構成を固定してから実装する。

---

## Scope

### 作るもの

- HDR offscreen render target（45-A、ADR 0040 ゲート）
- Tonemapping pass（Reinhard または ACES、ADR 0040 で決定）（45-B）
- Bloom 最小実装（threshold + blur + composite）（45-C）
- Editor: exposure / bloom toggle / intensity の設定 UI（45-D）

### 作らないもの

- Full screen effect graph / node editor
- TAA / motion blur / depth of field
- Per-camera post-process volume
- Specular split-sum IBL の本格実装（必要なら別タスク）

---

## Design Decisions

### HDR target format は ADR 0040 で決定

候補は `Rgba16Float` と `Rgba32Float`。メモリ、WebGPU 互換性、banding の少なさを
ADR 0040 で比較して Accept する。

### Post-process は explicit pass chain

MVP では graph 化せず、`scene_hdr -> tonemap -> backbuffer` の固定 chain から始める。
Bloom を有効にした場合だけ downsample / blur / composite pass を挿入する。

### Editor 設定は Project Settings に保存

Exposure / bloom enabled / bloom intensity は project setting として扱い、
runtime component にはしない。per-scene / per-camera override は後送り。

---

## Implementation Plan

### 45-A: HDR Render Target（ADR 0040 ゲート）

1. `RenderTargets` に HDR color texture / view を追加
2. Main scene pass の出力先を swapchain から HDR target に変更
3. Resize 時に HDR target を再作成

### 45-B: Tonemapping Pass

- Fullscreen triangle pass を追加
- exposure uniform を渡す
- output は swapchain format に変換

### 45-C: Bloom

- Bright-pass extraction
- 低解像度 blur
- Tonemap 前または tonemap pass 内で composite

### 45-D: Editor Integration

- Project Settings Panel に exposure / bloom controls を追加
- Game View toolbar に post-process bypass toggle を追加

---

## Prohibited（禁止事項）

- ADR 0040 の Accept 前に code / dependency を追加することを禁止
- post-process graph をこの Phase で実装することを禁止
- TAA / motion blur / DOF をこの Phase で実装することを禁止

---

## Completion Criteria（完了基準）

- HDR target 経由で scene が描画され、tonemapping 後に Game View に表示される
- exposure を変更すると表示が変わる
- bloom toggle が動作する
- `cargo fmt --all --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` / `cargo doc --workspace --no-deps` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 46: WASM — HDR / post-process pass の WebGPU 互換性確認
- Phase 47: GPU Instancing & LOD — instanced draw が post-process 前の scene pass に統合される
