# Phase 50: Directional Shadow Pass

Status: 実装完了（2026-07-05）。自動テスト・4 ゲートすべてパス。
`particles` / `skinned_mesh` example とも 10 秒以上の起動確認済み
（シャドウパス含む全パイプラインが wgpu バリデーション通過）。
影の見た目の最終確認のみ目視待ち。

## Goal

ADR 0036 で契約済みのディレクショナルライトシャドウ（2 カスケード・
Depth32Float・2048px）を実際に GPU で描画する。

## Why

- Phase 41 は contracts（`ShadowSettings` 等の型）のみで、影は一切
  描画されていない。ライティング表現の完成度に最も効く未実装領域。
- 新しい設計判断は不要（ADR 0036 の実装であり、カスケード数・
  フォーマット・bias 契約は変更しない → 新 ADR 不要）。

## Scope

- In: カスケード light view-projection 計算（`shadow.rs`・テスト付き）/
  depth-only シャドウパス（`shadow_depth.wgsl`・2 レイヤー array texture）/
  `mesh.wgsl`・`mesh_skinned.wgsl` での comparison sampling（light bind
  group 拡張）/ `ShadowSettings.enabled` の尊重
- Out: 解像度のランタイム変更 / ポイント・スポットライト影 /
  specular IBL / カスケードブレンド
- 50-D/50-E 追加（2026-07-05）: スキンドメッシュのシャドウキャスト
  （`shadow_depth_skinned.wgsl`・メインパスと同一 palette で変形一致）と
  3x3 PCF フィルタ（`params.w` = シャドウテクセルサイズ）を実装済み

## Design Decisions

- シャドウ関連バインディングは既存の light bind group（group 2）に
  追加する（binding 1: light VP + params uniform、binding 2: depth array、
  binding 3: comparison sampler）。新しい bind group index を増やさない
  ので、静的・skinned 両パイプラインの group 構成が変わらない。
- カスケード選択はフラグメント側で「cascade 0 の光空間範囲内なら 0、
  外れたら 1、両方外なら影なし」の単純判定。
- 影はディフューズ項のみ減衰させ、アンビエント項には影響しない。
- シャドウマップ解像度は起動時の `ShadowSettings::default()`（2048）で
  固定。ランタイム変更は将来課題。
- スキンドメッシュは 50-D で専用 depth-only パイプラインによりキャスト
  対応（palette bind group を共有）。パーティクルは既存バッチに乗るため
  キャストする。

## Implementation Plan

- 50-A: `shadow.rs` に `cascade_view_projections()`（フラスタムスライスを
  光空間 AABB でフィットするオルソ行列）+ ユニットテスト
- 50-B: `render.rs` シャドウリソース（array texture / per-cascade uniform /
  depth-only pipeline）と light BGL 拡張、シャドウパス実行
- 50-C: `mesh.wgsl` / `mesh_skinned.wgsl` のサンプリング、example
  起動確認、4 ゲート、docs 更新

## Cautions

- wgpu の `DepthBiasState`（constant/slope）とシェーダ側 `depth_bias` の
  併用でアクネを抑える。`normal_bias` はワールド法線方向のオフセット。
- `textureSampleCompareLevel` を使う（非一様制御フロー内で合法なのは
  Level 版のみ）。
- ライト方向がゼロベクトルの場合は影を無効化して縮退する。

## Prohibited

- カスケード数・`ShadowSettings` のフィールド構成の変更（ADR 0036 契約）
- `Vertex` / `InstanceData` の変更
- 新規第三者依存

## Completion Criteria

- ディレクショナルライト下で静的メッシュ・パーティクルが影を落とす。
- `ShadowSettings.enabled = false` で影が消え、描画は従来どおり。
- カスケード行列のフィット（全フラスタム角が光クリップ空間に収まる）が
  テストで保証される。
- 4 ゲートすべてパス、example 起動確認。

## Feeds Into

- カスケードブレンド / 解像度設定のランタイム反映 / SSAO
