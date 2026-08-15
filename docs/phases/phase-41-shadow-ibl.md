# Phase 41: Shadow Mapping & Environment Lighting

> **ADR 0036 Accepted**（シャドウマップフォーマット・解像度・カスケード数ポリシー）。

## Goal

Directional light のカスケードシャドウマップ（CSM）と skybox / IBL（Image-Based Lighting）を
実装し、レンダラーをインディーゲーム向け最低限のビジュアル品質に引き上げる。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

現在のレンダラーはシャドウがなく、ライティングが平板。
Phase 42 以降の新機能（Scripting・NavMesh 等）の見栄えに直結し、
Phase 45（Post-Processing）の HDR パイプラインも Shadow pass の存在を前提とする。

## Implementation Status

2026-06-14 の Phase 41 初期実装では、ADR 0036 に沿って runtime contract を追加した:

- `ShadowSettings` / `ShadowMapDescriptor` / `ShadowCascade` / `ShadowMapFormat`
- `EnvironmentLighting`
- `App::new()` での既定 resource 登録
- diffuse environment lighting を既存 ambient light contract に反映する renderer hook

GPU shadow pass、skybox drawing、environment texture sampling はこの初期実装の後続タスクとして残る。
ADR 0032（glTF）・ADR 0031（Material/Lighting）が確立した render resource 構造の上に
追加するため、Phase 40 完了後が最小差分で実装できる最初のタイミング。

---

## Scope

### 作るもの

- Directional Light Cascaded Shadow Maps — 2 カスケード（41-A、ADR 0036 ゲート）
- Skybox / Equirectangular Environment Map 読み込み（41-B）
- Diffuse IBL（irradiance map）— offline bake 前提（41-C）
- Editor: Game View の Shadow ON/OFF トグル + Scene View でのカスケード可視化（41-D）

### 作らないもの

- Point light / Spot light shadow（後送り）
- Specular split-sum IBL（Phase 45 以降）
- Runtime HDRI 生成（offline bake のみ）
- Soft shadow（PCSS 等）

---

## Design Decisions

### 2 カスケード固定（ADR 0036 で決定）

ADR 0036 で解像度・フォーマット・カスケード数を固定する。
実装前に ADR を Accept してからコードに触る。

### シャドウマップは `Depth32Float`

`wgpu::TextureFormat::Depth32Float` + PCF 2x2 最小実装。
精度と互換性のバランスが最良。ハードウェア PCF は `compare_function` で行う。

### Light は entity component からの mirror 方式（Phase 15-C）

`engine.directional_light` component が `DirectionalLight` resource へ
ミラーする既存の仕組みに乗る。Shadow pass はその resource を読む。

### IBL は diffuse のみ v1

`AmbientLight` を irradiance_map から差し替える形で導入。
specular split-sum（BRDF LUT + prefiltered env map）は Phase 45 以降。

---

## Implementation Plan

### 41-A: Shadow Pass 基盤（ADR 0036 ゲート）

1. `ShadowMapResource { texture: Texture, view: TextureView, sampler: Sampler }` を
   `render.rs` に追加
2. Shadow pass（depth-only draw call）を main pass の前に挿入
3. WGSL に `cascade_matrices: array<mat4x4<f32>, 2>` を uniform として追加
4. `sample_shadow_map(pos, cascade)` 関数で PCF 2x2 サンプリング
5. Main pass の fragment shader で shadow factor を乗算

### 41-B: Skybox / Equirectangular Environment

- `TextureCube` 対応（6 面 PNG または equirect PNG → 手動 cubemap 変換ユーティリティ）
- `engine.skybox` コンポーネント（cubemap AssetId を保持）
- フルスクリーン skybox draw pass（depth write off、depth test equal）

### 41-C: Diffuse IBL

- `IrradianceMap` resource — 低解像度 cubemap（8x8 〜 32x32 per face）
- `AmbientLight.use_ibl: bool` フラグ — true なら resource から ambient を読む
- サンプルプロジェクト用のデフォルト irradiance map（手作業 bake）を
  `assets/environment/default_irradiance/` に配置

### 41-D: Editor Integration

- Game View toolbar に「Shadow」チェックボックス
- Scene View でカスケード分割距離を赤・橙のフラスタムワイヤーフレームで表示
- Project Settings Panel に「Shadow Resolution: 512 / 1024 / 2048」ドロップダウン

---

## Cautions（注意点・落とし穴）

**Shadow acne**:
bias 値が小さいと surface が自身を遮蔽する。`bias = 0.002` を初期値にし、
インスペクターでチューニングできるようにする。

**Cascade split の可視境界**:
カスケード切替点でシャドウが突然変わって目立つ。ブレンドゾーン（0.1 world unit）を設ける。

**wgpu のデプステクスチャサンプラー**:
`SamplerBindingType::Comparison` は通常のサンプラーと binding layout が別。
Bind group / layout を分離して管理する。

---

## Prohibited（禁止事項）

- Point light shadow をこのフェーズで実装することを禁止
- ADR 0036 の Accept 前にコードを書くことを禁止
- specular IBL（split-sum）の先取り実装を禁止（Phase 45 以降）

---

## Completion Criteria（完了基準）

- ground + character + `engine.directional_light` の構成でリアルタイムシャドウが
  Game View に表示される
- Skybox が Game View の背景に描画される
- IBL ambient が `AmbientLight` を置換できる
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 45: Post-Processing — HDR render target（Phase 41 の 2 パス構成を前提）
- Phase 46: WASM — WebGPU での shadow 対応確認が必要
- Phase 47: Instancing — shadow pass への instance draw 対応
