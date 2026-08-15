# Phase 48: Skinned Mesh & Skeletal Animation

Status: 実装完了（2026-07-04）。自動テスト・4 ゲートすべてパス。
`cargo run --example skinned_mesh` による GPU 描画の目視確認のみ未実施。

## Goal

glTF からスキン付きメッシュとスケルタルアニメーションをインポートし、
GPU スキニングで描画・再生できるようにする。

## Why

- アニメーションランタイム（Phase 37）とアニメーショングラフ（Phase 38）は
  完成しているが、駆動できるのは単一エンティティの `Transform` のみで、
  キャラクターアニメーションが表現できない。
- glTF パイプライン（Phase 36 / ADR 0032)は static mesh のみ。スキンと
  アニメーショントラックは黙って捨てられており、既存資産が最も活きる
  拡張ポイントになっている。
- 親子 Transform 伝播（2026-07-04 実装）により、ジョイント階層を
  ECS エンティティとして表現する土台が整った。

## Scope

ADR 0043 の決定に従う。

- In: `SkinningVertexData`（slot 2）/ `SkinnedMesh` コンポーネント /
  ジョイント＝ECS エンティティ / uniform joint palette（MAX_JOINTS=128）/
  `mesh_skinned.wgsl` パイプライン / glTF skin + animation インポート /
  `AnimChannel.target_joint`
- Out: モーフターゲット / スキンドメッシュのインスタンシング /
  authoring スキーマ・エディタ Inspector 統合 / 外部バッファ URI /
  CUBICSPLINE 補間（linear に降格）

## Design Decisions

ADR 0043 を正とする。実装中に判明した詳細はこのセクションに追記する。

- `Vertex` は変更しない（静的メッシュに 24 バイト/頂点の死んだ属性を
  載せないため。破壊的変更そのものは許容されている）。
- ジョイント行列は `transform_propagation_system` の結果
  （`GlobalTransform`）から取得し、階層計算を重複実装しない。
- 不正データ（ジョイント数不一致・範囲外 index・消えたジョイント）は
  diagnostics / スキップで縮退し、panic しない。

### 実装時の追記（2026-07-04）

- skinned パイプラインの頂点バッファは slot 0 `Vertex` / slot 1
  `InstanceData`（1 インスタンス分のモデル行列+色）/ slot 2 skinning。
  モデル行列を uniform に分けず、静的パスと同じ `InstanceInput` 構造を
  シェーダで共有する。
- skinned エンティティの識別は `JointPalette` コンポーネントの有無。
  静的バッチクエリは `Without<JointPalette>` で除外する。
- `SkinnedMesh` を持つが skinning データのないメッシュは、静的
  パイプラインの 1 インスタンス描画に縮退する（非表示にしない）。
- `animation_system` は「animator 走査で書き込み予定をマップに収集 →
  全 `Transform` 走査で適用」の 2 段構成。`Query<(&mut Animator,
  Option<&SkinnedMesh>)>` と `Query<&mut Transform>` はアクセスが
  重ならないため単一システムで完結する。
- glTF アニメーションは「対象ノードを joint に含む最初の skin」に
  バインドする。skin 外ノードを対象とするチャネルは
  `gltf.animation_target_not_joint` 警告でスキップ。
- shadow パス（Phase 41）は skinned メッシュを未対応のまま
  （バインドポーズの影にはならず、影から除外される訳でもない —
  現状 shadow パス自体が contracts のみのため実影響なし）。

## Implementation Plan

- **48-A: Skinning vertex path**
  `SkinningVertexData` 型、`Mesh.skinning: Option<Vec<_>>`、GPU アップロード
  （slot 2）、長さ不一致の `MeshValidationError`。
- **48-B: SkinnedMesh + joint palette**
  `SkinnedMesh` コンポーネント、palette 計算システム
  （`GlobalTransform × inverse_bind`）、uniform buffer 更新。
- **48-C: Skinned render pipeline**
  `mesh_skinned.wgsl`、専用 pipeline / bind group layout、`render.rs` の
  skinned draw パス（インスタンシング batcher をバイパス）。
- **48-D: glTF skin import**
  `JOINTS_0` / `WEIGHTS_0` / `inverseBindMatrices` 読み込み、ジョイント
  エンティティ生成（`Parent`/`Children` 接続）、ADR 0043 §5 の診断。
- **48-E: glTF animation import + animator 拡張**
  アニメーションサンプラー → `AnimationClip`（`target_joint` 付き）、
  animator システムのジョイント解決。
- **48-F: Example + tests**
  スキン付き glTF を再生する example、インポート・palette・縮退の
  ユニットテスト。

依存順序: 48-A → 48-B → 48-C は直列。48-D / 48-E は 48-A 完了後に並行可。
48-F は全体の後。

## Cautions

- slot 1 は `InstanceData`（ADR 0042）が使用中。skinning は必ず slot 2。
- WGSL の uniform 配列は 8 KiB（128 × mat4x4<f32>）。WebGL2 の最小
  uniform buffer サイズ 16 KiB を超えない。
- `Mesh` に struct literal で構築している既存箇所があれば、`skinning`
  フィールド追加は同一 PR で全箇所更新する。
- glTF の joint index accessor は u8 / u16 の両方が来る。u16 に正規化。

## Prohibited

- `Vertex` のフィールド追加・変更
- シリアライズ済みフォーマット（scene / manifest / prefab / graph）の変更
- authoring クレートへの依存追加・スキーマ変更
- ライブラリコードでの `unwrap()` / `panic!()`（縮退 + diagnostics で処理）

## Completion Criteria

- スキン付き glTF（embedded buffer）をインポートすると、ジョイント
  エンティティ階層と `SkinnedMesh` が生成される。
- `Animator` でスキンクリップを再生すると、ジョイントの `Transform` が
  更新され、GPU スキニングで変形が描画される。
- 128 ジョイント超・weights 非正規化・欠落 inverseBindMatrices が
  ADR 0043 §5 どおりの diagnostics になる。
- 静的メッシュの描画パス・インスタンシングに回帰がない。
- `cargo fmt --check` / `clippy -D warnings` / `test --workspace` /
  `doc --no-deps` がすべてパスする。

## Feeds Into

- スキンドメッシュの authoring / エディタ統合（将来 Phase・要 ADR）
- アニメーショングラフ（Phase 38）からのスキンクリップ再生
- スキンドインスタンシング（将来の性能 Phase）
