# Phase 52: Particle Emitter Authoring Integration

Status: 実装完了（2026-07-11）。4 ゲートすべてパス。
`engine.particle_emitter` が builtin registry の 11 番目のコンポーネント
として登録され、spawn / デフォルト値一致 / 診断のテスト済み。

## Goal

Phase 49 の `ParticleEmitter`（ランタイム専用）をオーサリングコンポーネント
`engine.particle_emitter` として公開し、エディタの Add Component・シーン
ファイル・パッケージ済みゲームからパーティクルを使えるようにする。

## Why

- Phase 49 のパーティクルは `App` に直接 Rust コードで組み込む場合しか
  使えない。エディタで作ったシーン／Phase 51 のパッケージ出力からは
  一切利用できず、機能として「エンジンにあるのにゲームに置けない」状態。
- ADR 0027 の component registry がまさにこのための拡張点であり、
  Phase 15-C（Camera / Light / Controller の registry 追加）と同じ
  確立済みパターンで追加できる。新 ADR は不要（ADR 0027 + 0044 に従う）。

## Scope

- In:
  - `PARTICLE_EMITTER_COMPONENT`（`"engine.particle_emitter"`）定数の追加
  - `particle_emitter_schema()`（`crates/engine/src/components.rs`）
  - `spawn_particle_emitter_component()`（`crates/engine/src/scene_bridge.rs`）
  - `builtin_registry()` への登録（エディタの Add Component picker と
    generic inspector は registry 経由で自動追従）
  - mesh asset 解決の共通ヘルパー抽出（`spawn_mesh_component` と共有）
  - スキーマ・spawn・診断・デフォルト値のテスト
  - `docs/AGENTS.md` / `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` の表更新
- Out:
  - エディタ専用のエミッタープレビュー UI / gizmo
  - ネストした AssetRef フィールド用のアセットピッカー UX 改善
    （generic property editor の表示で v1 は可とする）
  - `LodGroup` / `engine.script` のオーサリング化（将来フェーズ）
  - authoring フォーマットの変更（コンポーネント追加は加算的変更のみ）

## Design Decisions

- **フラットフィールド規約に従う**: 既存の `engine.directional_light` が
  `direction_x` / `color_r` のようにベクトル・色をフラットな数値
  フィールドで表現しているため、emitter も同じ規約を使う。ネストした
  object フィールドを導入しない（generic inspector がそのまま使える）。
- **mesh は `FieldType::AssetRef` フィールド**とし、デフォルトは
  built-in quad（`BUILTIN_QUAD_ASSET_ID`）。`engine.mesh` のような
  whole-value asset ref にしないのは、emitter が多数の数値フィールドを
  持つため。spawn 時の解決は `spawn_mesh_component` と同じキャッシュ
  （`mesh_handles` / `added_mesh_handles`）を通す。ヘルパーとして抽出し
  ロジックを複製しない。
- **スキーマのデフォルト値は `ParticleEmitter::new()` と一致させる**。
  乖離すると「エディタで置いた見た目」と「コードで作った見た目」が
  食い違うため、一致をテストで固定する。
- **`seed` フィールド設定後は `ParticleEmitter::reset()` を呼ぶ**。
  `seed` フィールドへの代入だけでは rng が再シードされないため。
- **spawn 時のバリデーションは既存の `invalid_component` 診断パターン**
  に従う（非有限値・`lifetime_min > lifetime_max` 等は
  `SceneBridgeError::InvalidComponent` として blocking）。

## Component Schema（`engine.particle_emitter` v1）

デフォルトは `ParticleEmitter::new()` と同値。

| field | type | default | 制約 |
|-------|------|---------|------|
| `mesh` | AssetRef | built-in quad | manifest 解決失敗は既存 mesh fallback + 診断 |
| `spawn_rate` | F64 | 32.0 | 有限・`>= 0.0` |
| `lifetime_min` / `lifetime_max` | F64 | 0.8 / 1.6 | 有限・`0 < min <= max` |
| `initial_speed_min` / `initial_speed_max` | F64 | 2.0 / 4.0 | 有限・`min <= max` |
| `direction_x/y/z` | F64 | 0.0 / 1.0 / 0.0 | 有限（ゼロベクトルはランタイム側が Y にフォールバック済み） |
| `spread` | F64 | 0.5 | 有限・`>= 0.0` |
| `gravity_x/y/z` | F64 | 0.0 / -9.8 / 0.0 | 有限 |
| `start_color_r/g/b/a` | F64 | 1.0 / 0.9 / 0.4 / 1.0 | 有限 |
| `end_color_r/g/b/a` | F64 | 1.0 / 0.2 / 0.05 / 1.0 | 有限 |
| `start_size` / `end_size` | F64 | 0.12 / 0.02 | 有限 |
| `max_particles` | I64 | 1024 | `>= 0` |
| `seed` | I64 | `ParticleEmitter::new()` の seed | `0 <= seed <= u32::MAX` |

## Implementation Plan

- 52-A: `scene_bridge.rs` に定数 + mesh 解決ヘルパー抽出 +
  `spawn_particle_emitter_component`（バリデーション込み）
- 52-B: `components.rs` に `particle_emitter_schema()` + registry 登録
- 52-C: テスト（下記）+ 既存の registry 件数を前提にしたテストの更新
- 52-D: docs 表更新（AGENTS.md / IMPLEMENTATION_PLAN の Phase 48+ 表・依存グラフ）

## Tests

- スキーマの `default_value()` が `ParticleEmitter::new()` の全設定値と
  一致する（mesh は built-in quad の AssetRef）
- authoring scene に `engine.particle_emitter` を置いて
  `spawn_from_authoring_scene` → 実体に `ParticleEmitter` が付き、
  各フィールドが反映される
- デフォルト値（フィールド省略）でも spawn が成功する
- 不正値（`lifetime_min > lifetime_max`・非有限・型不一致・負の
  `spawn_rate`）が `InvalidComponent` 診断で失敗する
- mesh の manifest 解決失敗が既存の fallback + 診断挙動になる

## Cautions

- `.rs` ファイルに日本語を書かない（rustdoc は英語）
- `particles` の `rng` / `spawn_accumulator` / `particles` は非公開。
  構築は `ParticleEmitter::new(mesh)` → pub フィールド上書き → `reset()`
- `Vertex` / 既存シリアライズフォーマットに触れない（今回の変更は
  コンポーネント型の追加のみで加算的）
- editor クレートに registry 件数や定義順を前提にしたテストがあれば
  同一 PR で更新する（破壊的変更プロトコル §3）

## Prohibited

- `"$type": "asset_path"` の導入（ADR 0021。永久却下）
- `ParticleEmitter` の既存 pub API の変更・削除
- authoring クレートから engine 型への依存追加（単方向依存の維持）

## Completion Criteria

- エディタの Add Component に Particle Emitter が現れ、Play とパッケージ
  済みプレイヤーの両方でシーン配置したエミッターが動く（自動テストは
  spawn 経路まで。目視確認は既存 Phase 48-50 と同様に任意）
- 上記テストがすべてパスする
- 4 ゲート（fmt / clippy / test / doc）がすべてパスする

## Feeds Into

- Phase 53 候補: `LodGroup` / `engine.script` のオーサリング化
- ネスト AssetRef フィールドのアセットピッカー UX
- エディタ UI の「Package」ボタン配線（Phase 51 Feeds Into 継続）
