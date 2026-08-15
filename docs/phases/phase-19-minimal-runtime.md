# Phase 19: Minimal Runtime Features

> **2026-06-13 再構成**で新設。旧 Phase 19（Audio）は
> `phase-23-audio.md` へ後ろ倒し。

## Goal

PlayerController / Camera controllers / BehaviorTree / input / time /
debug draw を、minimal sample project（Phase 20）で使える程度に
editor 経由で安定させる。新規 runtime 機能は原則追加しない安定化フェーズ。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

13-A/B/D で実装された runtime 機能（PlayerController、camera controllers、
BT 接続）は example コード経由でしか検証されていない。Phase 15 で
authorable になった後、editor で組んだ scene から実際に動くことを
sample project の直前に確認する。Collision / Physics / Audio は
前提（12-B Fixed Timestep、collider 編集 UI）が揃っていないため
Phase 21〜23 へ後ろ倒し済み。

---

## Scope

### 作るもの

- PlayerController の editor 導線検証: 追加・編集 → Play → Game View 内で
  WASD 移動（19-A）
- `OrbitCamera` / `FollowCamera` の authorable 化（registry 登録）と
  安定化（19-B）
- BehaviorTree 接続の editor 検証（19-C）
- debug draw の Play 中トグル、time / input の安定確認（19-D）

### 作らないもの（Phase 21〜24）

- Collision / Physics / Audio（着手禁止）
- Action Mapping（12-D。引き続き KeyCode 直接参照）
- Runtime Debug Overlay の本格実装（12-E。egui runtime 統合は Phase 24）

---

## Design Decisions

### Game View focus 時のみゲーム入力を流す

editor のキーボードショートカット（Ctrl+S 等）とゲームの WASD が
衝突しないよう、Game View パネルが focus を持つ間だけ runtime へ
入力を転送する。既存の入力転送実装を検証し、足りなければ修正する。

### FollowCamera の target 参照は entity_ref

`FollowCamera.target` は entity 参照を要する。authorable 化では
`Value::EntityRef`（spec §7.4 の既存タグ）を使い、spawn 時に
`AuthoringToRuntimeMap` で runtime Entity に解決する。
解決失敗は診断 + camera 無効化（クラッシュしない）。

---

## Implementation Plan

### 19-A: PlayerController の editor 導線

- editor で `engine.player_controller` を追加 → Play → WASD 移動を確認
- `player_controller_system` の Play 時登録を確認
  （`App::new()` は自動登録しない方針のまま、`RuntimePlayState::start` が
  登録する）
- 12-F の `InputCommand` 注入による自動テスト

### 19-B: Camera controllers の authorable 化

- `engine.orbit_camera` / `engine.follow_camera` を registry に登録
  （Phase 15 の基盤により登録 1 箇所 + テスト）
- `orbit_camera_system` / `follow_camera_system` の Play 時登録
- `FollowCamera.target` の entity_ref 解決と失敗診断

### 19-C: BehaviorTree 接続の editor 検証

- editor で BT graph を持つ scene を組み、Play で
  `register_behavior_tree_system` 経由の挙動を確認（13-D の editor 導線検証）

### 19-D: debug draw / time / input

- `DebugLines` の Play 中トグル（キー or UI ボタン）
- `Time` の経過・delta が editor Play で正しいことの確認
- 既知の入力取りこぼし（focus 切替時の stuck key 等）の修正

---

## Cautions（注意点・落とし穴）

**stuck key**: Game View の focus が外れた瞬間に押下中のキーが
released にならないと、移動し続ける entity が発生する。focus 喪失時に
`Input` の全キーを release する。

**バグ修正にはテスト必須**（RUST_CODE_STYLE §11）。
入力系は 12-F の仮想入力でテスト可能。

---

## Prohibited（禁止事項）

- Collision / Physics / Audio の実装着手を禁止（Phase 21〜23）
- 新規 runtime 機能の追加を原則禁止（安定化に必要な最小修正のみ）

---

## Completion Criteria（完了基準）

- editor で組んだ scene の PlayerController が Game View 内で WASD で動く
- `OrbitCamera` / `FollowCamera` を editor から追加・編集でき、Play で効く
- BT を持つ scene が editor から Play できる
- 仮想入力による PlayerController の自動テストが通る
- `cargo test --workspace` が通る

---

## Feeds Into（次フェーズへの依存）

- Phase 20: minimal sample project がこれらの機能だけで成立する
