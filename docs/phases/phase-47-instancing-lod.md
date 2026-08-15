# Phase 47: GPU Instancing & LOD

## Goal

同一 mesh / material の大量描画を GPU instancing で効率化し、距離に応じて LOD を切り替える。

---

## Why（なぜこのフェーズが必要か・なぜこの順序か）

Shadow pass、post-process、WASM target の基本互換性を確認した後で、描画数の多い scene を
高速化する。Phase 44 の NavMesh 上に多数の agent を置くケースでも、instance draw と
LOD が効く必要がある。

---

## Scope

### 作るもの

- `InstanceBuffer` と per-instance transform / color data（47-A）
- Main pass と Shadow pass の instanced draw 対応（47-A）
- Static mesh renderer の batch key（mesh + material + render flags）（47-B）
- LOD component / asset reference（47-C）
- Editor: LOD distance visualization / preview（47-D）

### 作らないもの

- GPU driven rendering / indirect draw
- Occlusion culling
- Impostor generation
- Hierarchical LOD

---

## Design Decisions

### Batch key は mesh + material + render flags

同一 mesh でも material が異なる場合は別 batch とする。Shadow-only flags など pass 差分も
batch key に含める。

### LOD は distance thresholds のみ

MVP は camera distance による LOD 切替だけを扱う。screen-space error や hysteresis の
詳細は必要になった時点で追加する。

### Shadow pass も instancing 対応する

Phase 41 の Shadow pass が instance draw に対応しないと、main pass だけ高速化しても
shadow draw が bottleneck になる。

---

## Implementation Plan

### 47-A: Instance Buffer

1. per-instance transform matrix / optional color を buffer に詰める
2. WGSL vertex input を instance step に対応
3. Main pass と Shadow pass の draw call を instance count 付きにする

### 47-B: Batching

- visible renderables を batch key で group 化
- material / mesh bind group の切替回数を削減

### 47-C: LOD

```rust
pub struct LodGroup {
    pub levels: Vec<LodLevel>,
}

pub struct LodLevel {
    pub max_distance: f32,
    pub mesh: AssetId,
}
```

- camera distance で active mesh を選択
- missing LOD asset は diagnostic に出して最高詳細 mesh に fallback

### 47-D: Editor Integration

- Inspector で LOD levels を編集
- Scene View に LOD distance ring / sphere を表示
- Game View debug overlay に instance batch count を表示

---

## Prohibited（禁止事項）

- indirect draw / GPU culling をこの Phase に含めることを禁止
- material system の大規模再設計をこの Phase に含めることを禁止
- Shadow pass の instancing 対応を後回しにして完了扱いにすることを禁止

---

## Completion Criteria（完了基準）

- 同一 mesh / material の大量 entity が instanced draw にまとまる
- Shadow pass でも instanced draw が使われる
- LOD threshold に応じて mesh が切り替わる
- debug overlay で batch count / instance count を確認できる
- `cargo fmt --all --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` / `cargo doc --workspace --no-deps` が通る
