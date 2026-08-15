# Phase 59: Animation Blending & Events

Status: 実装完了（2026-07-18）。クロスフェード、固定ステップイベント、
Animation Graph 実行、オーサリング、Edit Mode プレビュー、Play 中デバッグ、
ルートモーションの character motor 接続を実装し、対象テストを通過。

## Goal

(1) クリップ間クロスフェード、(2) アニメーションイベント（攻撃判定
フレーム等の発火 → Rhai/BT）、(3) Phase 38 Animation Graph（ADR 0033
`CompiledAnimGraph`）の実行時評価、を提供する。

## Why

- 現状の `Animator` は単一クリップの即時切替のみで、アクションゲームの
  モーション遷移（待機→攻撃→待機）がポップする。
- 「攻撃のこのフレームで hitbox を出す」仕組みがなく、Phase 57 の
  trigger と組み合わせられない。
- Phase 38 でグラフのオーサリングとコンパイルはできるが、実行器がない。

## Scope

- In:
  - `Animator` のクロスフェード（`crossfade_to(clip, duration)`）
  - `AnimationClip.events`（`AnimEvent { time, name }`）+ 通過検出 +
    `AnimationEvents` リソース + 対象エンティティのスクリプトへの
    `on_event` 配送
  - `AnimGraphPlayer` コンポーネント + `anim_graph_system`
    （condition フラグで遷移 → crossfade 起動）+
    `load_animation_graph(path)` ヘルパー
  - テスト
- Out:
  - authoring コンポーネント化（`engine.animator` 等。クリップ handle の
    アセット解決設計が必要なため将来 Phase）
  - クリップアセットエディタでのイベント編集（events はコード/インポート
    後付け。文書化）
  - ブレンドツリー（2D ブレンド等）・上半身マスク・IK（M1 非目標）

## Design Decisions

### 1. クロスフェードは「2 クリップサンプル + 出力補間」

- `Animator` に private な fade 状態（前クリップの handle・時刻・
  looping・残り時間・総時間）を追加。`crossfade_to(clip, duration)` は
  現在の再生を fade 元として保存し、新クリップを time 0 から開始。
  `duration <= 0` は即時切替。fade 中の再 crossfade は「現在の合成結果を
  静止ポーズとして扱わず」、単純に fade 元を差し替える（v1 簡素化。
  rustdoc に明記）。
- `animation_system` は fade 中、両クリップをサンプルし weight =
  経過/総時間 で補間する。**両クリップに存在するプロパティは lerp、
  片方にしかないプロパティはそのまま適用**（v1 規則。rustdoc に明記）。
- 回転プロパティ（quaternion `[f32; 4]`）は正規化 lerp（nlerp）+
  符号補正（dot < 0 で片方を負向きに）。`lerp_channel` は変更せず、
  合成層を追加する。
- fade 元クリップの時刻も fade 中は進める（walk→run で足が止まらない）。

### 2. アニメーションイベント

- `AnimationClip` に `events: Vec<AnimEvent>`（`time` 秒・`name`）を追加
  （struct literal 構築箇所は同一 PR で修正 = 破壊的変更プロトコル §3。
  glTF インポートは空 Vec で埋める）。
- `animation_system` が各 tick で「前回時刻 < event.time <= 今回時刻」を
  発火（ループ折返しは (prev, duration] と (0, new] の 2 区間）。fade 元
  クリップのイベントは発火しない（現行モーションのみ。明記）。
- `AnimationEvents` リソース: `Vec<AnimationEventRecord { entity, name }>`。
  `animation_system` の先頭でクリア → 発火分を積む（fixed step 内有効。
  `CollisionEvents` と同じライフサイクル）。
- **配送は対象エンティティ限定**: `scripting_update_system` が
  `AnimationEvents` を読み、record の entity 自身が enabled な
  `ScriptComponent` を持つ場合のみ `run_on_event(entity, name)` を呼ぶ
  （UI イベントの全員ブロードキャストと違う点。rustdoc に明記）。
  登録順は animation → scripting（同 tick 配送）。BT は リソース読みの
  既存パターン。

- `engine.animator.events` の各行は任意の `clip` 名を持てる。`clip` がある
  行は解決済み glTF/GLB クリップだけで発火し、省略した行は互換用の
  Animator 共通イベントとして発火する。これにより攻撃判定イベントが
  idle や move クリップで誤発火しない。

### 3. Animation Graph 実行器

- `AnimGraphPlayer` コンポーネント:
  - `graph: CompiledAnimGraph`（値保持。graph はコンパイル済み・不変）
  - `clips: BTreeMap<String, Handle<AnimationClip>>`（state の `clip_id`
    → handle。ゲーム側が構築時に供給）
  - `current_state: usize`・`fade_duration: f32`（default 0.2）
  - `set_condition(name, bool)` / `condition(name)`（BT レジストリと
    同じ感覚のフラグ表）
- `anim_graph_system`（fixed・opt-in・**animation_system より前**に登録）:
  現 state の out 遷移を `CompiledAnimGraph.transitions` の順で走査し、
  最初に「condition が空（無条件）or フラグ true」の遷移で state 変更 →
  遷移先 clip_id を `clips` で解決して `Animator::crossfade_to`。
  解決失敗（clip_id なし / map 未登録）は `log::warn!` して遷移だけ行う。
  無条件遷移の連鎖は 1 tick に 1 遷移まで（無限ループ防止）。
- `load_animation_graph(path) -> Result<CompiledAnimGraph, ...>`:
  authoring の graph 読み込み（persist モジュールの既存 API を調査して
  使う）+ `compile_animation_graph`。エラーは typed error。

## Implementation Plan

- 59-A: crossfade（Animator 拡張 + animation_system の合成層）
- 59-B: events（clip 拡張 + 発火 + `AnimationEvents` + scripting 配送）
- 59-C: graph 実行器 + `load_animation_graph`
- 59-D: テスト + 4 ゲート

## Tests

- crossfade: fade 中間で値が両クリップの補間になる（translation で数値
  検証）・fade 完了後は新クリップのみ・`duration <= 0` は即時・回転の
  符号補正（dot < 0 ケースで最短経路）・片方にしかないプロパティは
  素通し
- events: 通過で 1 回だけ発火・同 tick 複数イベント順序（time 昇順）・
  ループ折返しで両区間発火・停止中は発火しない・`AnimationEvents` が
  tick 先頭でクリアされる
- 配送: イベント entity のスクリプトだけが `on_event` を受ける（隣の
  entity のスクリプトは受けない）
- graph: 無条件遷移が entry から 1 tick で 1 つだけ進む・condition
  フラグで遷移・遷移時に crossfade が起動（Animator の fade 状態を確認）・
  clip 未解決で warn + state だけ遷移・`load_animation_graph` の成功/
  パース失敗/コンパイル失敗
- 既存 animation テストが無変更で通る（fade なし経路の挙動不変）

## Cautions

- `.rs` に日本語禁止・英語 rustdoc・回復可能エラーに unwrap 禁止
- `AnimationClip` / `Animator` の struct literal 構築箇所（テスト・
  examples・glTF インポート）を同一 PR で全部直す
- スキンドメッシュ（`target_joint` チャンネル・Phase 48）の合成も
  同じ規則で通ること（joint palette への反映は既存経路のまま）
- fixed step が大きいとイベントを飛ばさない（区間判定であって
  サンプリングではない）ことをテストで固定

## Prohibited

- ブレンドツリー・レイヤー/マスクの先行実装
- `lerp_channel` の挙動変更
- graph の実行時再コンパイル（コンパイルはロード時 1 回）

## Completion Criteria

- 待機↔移動をフラグで遷移するグラフが crossfade 付きで動く（テスト）
- クリップ上のイベントが正確に 1 回、対象エンティティのスクリプトに
  届く（テスト）
- 4 ゲートパス

## Editor Ready Integration (2026-07-18)

- `engine.animator` は clip source/name、loop/autoplay、playback speed、
  completion event、clip 別 timeline event、root-motion mode を Inspector
  から編集できる。
- `engine.animation_graph_player` は graph/clip source、既定 crossfade、
  boolean parameter map を Inspector から編集できる。
- Scene View の Animation Preview は一時 world だけをサンプルし、停止または
  Restart で authored/rest pose に戻る。scene document と Play world は変更しない。
- Play 中の選択 entity には現在 clip/time/state、crossfade、root motion、
  graph state/transition/parameters を read-only 表示する。
- `engine.root_motion_motor` は共有 runtime host profile に登録され、
  `engine.animation` の後、`engine.character_controller` の前に実行される。

## Feeds Into

- Phase 60: Script API v2（`ctx.play_anim` / `ctx.set_anim_condition`）
- Phase 62: busters_lite（攻撃モーション + 判定フレーム）
