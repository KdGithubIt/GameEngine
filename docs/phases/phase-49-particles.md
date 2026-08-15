# Phase 49: Particle System

Status: 実装完了（2026-07-05）。自動テスト・4 ゲートすべてパス。
`cargo run --example particles` は 12 秒間の起動確認済み
（wgpu バリデーション・描画パスとも安定）。見た目の最終確認のみ目視待ち。

## Goal

CPU シミュレーション + 既存インスタンシング描画によるパーティクル
システムを追加する。

## Why

- GPU インスタンシング（Phase 47）がそのまま描画基盤として使え、
  レンダラ変更ほぼゼロで視覚効果の表現力が大きく上がる。
- サンプルゲーム（ヒット演出・ゴール演出など）の見栄えに直結する。

## Scope

ADR 0044 の決定に従う。

- In: `ParticleEmitter` コンポーネント / `particle_update_system`（frame
  schedule・App::new 自動登録）/ render.rs のパーティクル収集パス /
  xorshift RNG（依存追加なし）/ example / テスト
- Out: ビルボード / GPU シミュレーション / authoring・エディタ統合 /
  テクスチャアトラスアニメーション / ソート（半透明の厳密な奥行き順）

## Design Decisions

ADR 0044 を正とする。

- パーティクルはエンティティではなく emitter 所有の Vec。
- world 空間シミュレーション（emitter 移動でトレイルが出る）。
- 描画は既存 instanced パイプラインへの合流のみ。emitter 自身の
  エンティティは `Handle<Mesh>` コンポーネントを持たない
  （batcher に二重描画されないため。mesh はフィールドで参照）。

## Implementation Plan

- 49-A: `particles.rs`（Particle pool / ParticleEmitter / xorshift /
  update system）+ App::new 登録
- 49-B: render.rs 収集パス（live particle → InstanceData、Material 合成）
- 49-C: example（噴水 or ヒットバースト）+ テスト
  （spawn/寿命/上限/決定性/ゼロ divide 安全）

## Cautions

- `spawn_rate` の端数は accumulator で持ち越す（低レートで一切
  湧かなくなるのを防ぐ）。
- `Time.delta_seconds` が大きいフレーム（ブレークポイント等）で
  一斉大量スポーンしないよう 1 フレームのスポーン数を上限で刻む。
- 半透明はソートしないので、v1 のデフォルトは不透明寄りの色を推奨。

## Prohibited

- 新規第三者依存（`rand` 等）の追加
- 既存パイプライン・`InstanceData`・`Vertex` の変更
- パーティクルを ECS エンティティとして生成すること

## Completion Criteria

- emitter を置くとパーティクルが湧き、寿命・色・サイズ補間・重力が働く。
- 同一 seed・同一設定で決定的に同じ軌跡になる（テストで保証）。
- `InstanceStats` にパーティクルバッチが反映される。
- 4 ゲート（fmt / clippy / test / doc）すべてパス。

## Feeds Into

- ビルボード・ソート・テクスチャアニメーション（将来拡張）
- サンプルゲームの演出強化
