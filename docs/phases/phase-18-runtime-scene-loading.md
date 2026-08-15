# Phase 18: ProjectRoot ベースの Runtime Scene Loading / Reload

> **2026-06-13 再構成**: 旧 Phase 15「Scene Management — Multiple Scenes /
> Prefab / Dynamic Spawn」を縮小移設した。Prefab / `Commands` による動的
> スポーン / `DontDestroyOnLoad` / scene transition サンプルは Phase 21 以降
> （フル sample game は Phase 25）へ後ろ倒し。旧スコープの設計スケッチは
> 本ファイルの git 履歴（`phase-15-scene-management.md` 時代）を参照。
> 旧→新対応表は `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md`。

## Goal

editor で開いている project の scene を、Play 中に確実に load / reload
できるようにする。複雑な scene transition より「ファイル上の scene を
確実にロードして Play できる」ことを優先する。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

**Phase 16〜17 の後にこのフェーズが来る理由**:
Phase 16 で `RuntimePlayState` が `ProjectRoot` / `AssetServer` /
`AssetManifest` を持つようになり、Phase 17 で編集ループが安定する。
その土台の上で「ファイルからの scene ロード」を足すのが最小差分。

**なぜ transition / Prefab を含めないか**:
タイトル→ゲームの切り替えや動的スポーンの本来の顧客はフル sample game
（Phase 25）。minimal sample project（Phase 20）は単一 scene で成立する。
消費者なしの機能を先行実装しない。

---

## Scope

### 作るもの

- `SceneLoader`（`crates/engine/src/scene_loader.rs` 新規）—
  `ProjectRoot` 経由のパス解決 + `load_scene_from_json()` ラッパー
- editor からの Reload — Play 中にファイル上の scene を再ロード
  （まず「同一 scene の reload」のみ）
- （任意・最小）`SceneManager` の pending_load 方式による単純な scene 切替

### 作らないもの（Phase 21 以降へ後ろ倒し）

- `Prefab` / `.prefab.json` / `spawn_prefab_at`
- `Commands` による deferred spawn / despawn
- `DontDestroyOnLoad` と persistent entity
- scene の additive load / preload / streaming
- タイトル → ゲーム → リザルトの transition サンプル

---

## Design Decisions

### なぜ scene 切り替えをフレーム末尾で行うか（18-C を実装する場合）

フレーム途中で world を丸ごと入れ替えると、処理中の system が無効な
entity 参照を持つ。`load_scene` 要求は `SceneManager.pending_load` に
溜め、フレーム末尾（全システム完了後）にメインループ側で実行する。

### なぜ reload を transition より先にやるか

reload は「同じ scene を破棄して読み直す」だけで、persistent entity の
扱いを決めずに済む。エディタの編集→確認ループに直接効く最小機能。

---

## Implementation Plan

### 18-A: SceneLoader

```rust
// crates/engine/src/scene_loader.rs (新規)
pub struct SceneLoader { /* ProjectRoot 経由のパス解決 */ }

impl SceneLoader {
    pub fn load(&self, relative_path: &str) -> Result<AuthoringScene, SceneLoadError>;
}

pub enum SceneLoadError {
    Io(std::io::Error),
    JsonParse(String),
    Validation(Vec<Diagnostic>),
}
```

`engine-authoring` の `load_scene_from_json()` + `fs::read_to_string()` の
ラッパー。パスは `ProjectRoot::resolve_asset()` で assets root 内に限定する。

### 18-B: editor からの Reload

Play 中にファイル上の scene を再ロードする。runtime world を作り直し、
`AssetServer` のキャッシュは維持する（同一 `AssetId` は再読込しない）。
失敗（I/O・パース・バリデーション）はすべて Diagnostics に出し、
クラッシュも Play 強制終了もしない（reload 失敗時は旧 world を維持）。

### 18-C:（任意・最小）単純な scene 切替

```rust
// crates/engine/src/scene_manager.rs (新規・最小)
pub struct SceneManager {
    current_scene: Option<String>,
    pending_load: Option<String>,
}
```

ECS システムは pending を立てるだけ。実切替はメインループが
フレーム末尾に行う。persistent entity は扱わない（全 entity 破棄 →
新 scene スポーン）。

---

## Cautions（注意点・落とし穴）

**reload 失敗時のロールバック**:
新 scene のロードに失敗した場合、旧 runtime world を破棄してはいけない。
「読み込み成功を確認してから差し替える」順序を守る。

**`AssetServer` キャッシュの寿命**:
reload で mesh キャッシュを捨てると毎回ディスク I/O が走る。
`AssetId` キーのキャッシュは Play セッション中は維持し、
Stop で破棄する（spec §5.2 の `AssetId → RuntimeAssetId` 対応と同じ寿命）。

**system 実行中の world 破壊禁止**:
切替・reload は必ず全システム完了後に行う。

---

## Prohibited（禁止事項）

- Prefab / `DontDestroyOnLoad` / transition サンプルをこのフェーズで
  実装することを禁止（Phase 21 以降）
- scene 切り替えをフレーム途中（system の実行中）に行うことを禁止
- ロード失敗でクラッシュ・パニックすることを禁止（診断を出して継続）

---

## Completion Criteria（完了基準）

- editor で開いている project の scene を Play 中に reload できる
- reload 失敗が Diagnostics に出て、旧 world が維持される
- `cargo test --workspace` が通る（load / reload の成功・失敗テストを含む）

---

## Feeds Into（次フェーズへの依存）

- Phase 19: minimal runtime features の検証で reload を使った高速反復
- Phase 21 以降: `SceneManager` を transition / persistent entity へ拡張
- Phase 25: フル sample game のタイトル → ゲーム切り替え
