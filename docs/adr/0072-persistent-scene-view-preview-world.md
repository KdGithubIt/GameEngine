# ADR 0072: Persistent Scene View Preview World

- Status: Accepted (Stage A implemented; Stage B proposed)
- Date: 2026-07-21
- Depends on: ADR 0068 (best-effort conversion), ADR 0071 (shared glTF cache)

## Implementation status

- **Stage A — implemented.** `SceneView` retains a `PreviewWorld` gated by
  `PreviewKey` (`crates/editor/src/scene_view.rs`). The scene input is the
  session's `document_revision` (`crates/editor/src/session.rs`, bumped in
  `sync_scene_document_from_session` and every document-switch site); the
  manifest input is a per-frame content hash (`manifest_content_hash`). Idle
  frames reuse the world with no conversion, mesh copy, or GPU upload.
  Editor-camera recreation, `DebugLines` clearing, particle re-simulation, and
  animation sampling run as per-frame maintenance on the reused world. The
  transform fast path (`apply_transform_overrides`) moves a gizmo/Inspector
  transform drag without a rebuild; non-transform Inspector previews force a
  per-frame rebuild.
  - Deviation from the design below: the manifest uses a per-frame content
    hash rather than a revision counter. The manifest is mutated from ~12
    scattered editor sites, so a counter would be fragile (a missed bump is a
    silent stale preview). Hashing the small manifest each frame is O(entries),
    not O(asset bytes), and makes the invalidation total by construction. The
    scene keeps the revision counter because it has a single clean mutation
    funnel.
  - The scene revision is drawn from a **process-wide** sequence, not a
    per-session counter. Each document tab owns its own `EditorSession`
    (`crates/editor/src/workspace.rs`), so a per-session counter handed two
    different scenes the same value — both reach 1 on their first open — and
    the shared Scene View reused the previous scene's world when switching
    tabs or opening another scene (it only redrew after an edit bumped the
    counter). A global sequence makes every revision unique across all
    sessions, so any document switch invalidates the preview. Regression:
    `distinct_sessions_never_share_a_document_revision`.
- **Stage B — proposed, not implemented.** Entity-granular incremental update
  remains future work as described below.

## Context

The Scene View rebuilds its entire preview world every frame:
`SceneView::show` calls `build_preview_app_with_sky`, which creates a fresh
`engine::App`, runs a full best-effort scene conversion, and drops the world
at the end of the frame (`crates/editor/src/scene_view.rs`).

ADR 0071 removed the dominant per-frame cost (glTF re-parse and image
re-decode), but the rebuild-per-frame architecture still pays, every frame,
for every entity in the scene:

- Full authoring-to-runtime conversion (validation, planning, spawning).
- A deep copy of every referenced mesh (`mesh.mesh.clone()` in
  `scene_bridge/asset_load.rs` copies the whole `Vec<Vertex>` into a fresh
  `Assets<Mesh>` store).
- Re-upload of every mesh to the GPU: `GpuMeshCache` is a world resource
  inserted by `App::new` (`crates/engine/src/app.rs`), so a fresh world means
  an empty cache and `GpuMesh::upload` runs again for every mesh
  (`upload_pending_meshes`, `crates/engine/src/render.rs`).
- ECS churn: every entity, component, and resource is reallocated.

Commercial editors (Unity, Godot, Unreal) do not have this problem because
their editor scene world is persistent: edits are applied as deltas to a
retained world, and GPU resources are uploaded once. Rebuilding a world per
frame is the anomaly, and it caps how large a scene the editor can preview
interactively.

ADR 0068 explicitly relies on the per-frame rebuild in one place: a skipped
component may leave partial per-component state in the preview world, and the
rebuild guarantees it "cannot outlive one frame". Any persistence design must
replace that guarantee with an equivalent one.

## Goals

1. An idle Scene View frame (no document change) performs zero conversion
   work: no validation, no spawning, no mesh copies, no GPU uploads.
2. A transform gizmo / Inspector drag updates the preview at interactive
   rates for scenes with hundreds of entities.
3. Behavior visible to the user is unchanged: same preview content, same
   best-effort skip notices, same Play/player/packaging behavior (untouched).
4. Every invalidation is provably routed: a stale preview is a bug class this
   design must make structurally hard, not just unlikely.

## Non-goals

- Baked import artifacts on disk (Unity `Library/` model). That is the
  long-term complement to this ADR and needs its own ADR (content-hash keyed
  artifact store, importer versioning, migration). ADR 0071's in-memory cache
  is sufficient until project sizes demand it.
- Incremental preview for Play mode. Play keeps strict, atomic, one-shot
  conversion.

## Decision

Adopt a staged design. Stage A alone already meets goals 1 and 3; Stage B
meets goal 2 for large scenes. Stages are independently shippable.

### Stage A — Revision-gated persistent world

`SceneView` retains the preview app across frames and rebuilds it only when
one of its inputs actually changed.

```text
struct SceneView {
    ...
    preview: Option<PreviewWorld>,
}

struct PreviewWorld {
    app: engine::App,
    /// Inputs the current world was built from.
    key: PreviewKey,
    /// Conversion output retained for Stage B and for notices.
    bridge_map: engine::scene_bridge::AuthoringToRuntimeMap,
}

#[derive(PartialEq)]
struct PreviewKey {
    scene_revision: u64,          // new: EditorSession document revision
    manifest_revision: u64,       // new: bumped on every manifest mutation
    game_module: Option<usize>,   // Arc::as_ptr identity
    project_root: Option<PathBuf>,
    sky_enabled: bool,
    animation_preview_enabled: bool,
    particle_preview_enabled: bool,
}
```

Per frame:

1. Compute `PreviewKey` from current inputs.
2. If `preview` is `None` or `key` differs → full rebuild (exactly today's
   `build_preview_app_with_sky`), store the new `PreviewWorld`.
3. Otherwise reuse `preview.app` and run only per-frame maintenance:
   - clear `DebugLines`, then redraw grid/gizmo/icon overlays (today this
     works implicitly because the world is new; with persistence the clear
     must be explicit or lines accumulate),
   - update `ViewportSize` on resize,
   - `update_editor_camera` (already per-frame),
   - preview simulations (see below).

#### Revision sources (the invalidation contract)

- `EditorSession` gains `document_revision(&self) -> u64`, incremented in
  exactly the places that change the authored document: command commit /
  checkpoint push (`session.rs` push sites), `undo`, `redo`, and every
  document load/replace/clear site that currently calls
  `undo_stack.clear()`. These are already the sole mutation funnels for the
  scene document; the revision is bumped next to the existing bookkeeping,
  not sprinkled at call sites.
- The editor state that owns `AssetManifest` gains the same counter, bumped
  wherever the manifest is mutated or reloaded (register asset, reimport,
  external drop, manifest reload from disk).
- Defense in depth (debug builds only): every N frames, hash the serialized
  scene and compare against the hash captured at build time; on mismatch,
  log a "missed invalidation" error. This turns a silent stale preview into
  a diagnosable defect during development without per-frame release cost.

#### Transient gesture previews (gizmo / Inspector drags)

Today a drag builds a cloned scene (`apply_component_preview`) and the world
is rebuilt from it every frame. With Stage A:

- Transform-shaped previews (the overwhelmingly common case: gizmo drag,
  Inspector transform drag) bypass conversion entirely. The preview value is
  written directly to the runtime entity's `Transform` (resolved through the
  retained `bridge_map`), and `GlobalTransform` propagation runs as usual.
  On gesture end (commit or cancel), `scene_revision` changes (commit) or
  the authored value is rewritten to the entity (cancel), so the world
  converges with the document without a rebuild.
- Non-transform component previews fall back to rebuild-per-frame during the
  gesture only (bounded by ADR 0071 caches; identical to today's cost, and
  Stage B upgrades this path to a single-entity respawn).

#### Preview simulations on a persistent world

Both preview simulations must produce state that is a pure function of
`(authored scene, elapsed)` so that reuse cannot drift:

- Animation preview already samples clips at absolute `elapsed`; sampling
  fully overwrites targeted channels each frame, so re-sampling on a
  persistent world is idempotent for sampled targets. Toggling the preview
  off (or Restart) must restore the authored pose — that is why
  `animation_preview_enabled` is part of `PreviewKey`: the toggle triggers
  one rebuild, which restores the rest pose exactly as today.
- Particle preview currently re-simulates from `elapsed` on fresh emitters.
  On a persistent world, emitter pools must be reset to their authored state
  before each `simulate_preview(elapsed, ...)` call (or `simulate_preview`
  documented and enforced as elapsed-deterministic). Same toggle rule via
  `PreviewKey`.

#### ADR 0068 partial-state guarantee, restated

ADR 0068's consequence "partial state cannot outlive one frame" becomes:
partial state cannot outlive the *next rebuild or respawn of its entity*.
Stage A: any document change rebuilds the world, so partial state lives at
most until the user's next edit — and it was produced by an invalid
component that converts to nothing anyway. Stage B keeps the stronger form:
a changed entity is always despawned and respawned whole, never patched.
ADR 0068 must be amended with a pointer to this ADR when Stage A lands.

### Stage B — Entity-granular incremental update

Stage A still rebuilds the whole world on every committed edit (typing in
the Inspector produces a commit per keystroke-debounce). Stage B reduces the
unit of invalidation from "world" to "entity".

- Retain, alongside the world, the last-converted `AuthoringScene` snapshot
  and the `AuthoringToRuntimeMap`.
- On `scene_revision` change, diff old vs new scene at entity granularity
  (`EntityId → AuthoringEntity` value equality; `AuthoringEntity` already
  has stable IDs and comparable component `Value`s).
- Classification:
  - **Component-value change** (same ID, same parent, components differ):
    despawn the runtime entity plus its auxiliary runtime-only entities
    (skeleton entities from `spawn_skin`), then respawn that single entity
    through the existing spawn path. Whole-entity respawn preserves the
    ADR 0068 cleanup guarantee and reuses all component spawn code.
  - **Structural change** (entity added/removed, parent changed, scene-level
    singleton components changed, `enabled` flag changed on an entity with
    descendants): full rebuild. Hierarchy edits are rare relative to value
    edits; incrementalizing `Parent`/`Children` rewiring is not worth its
    correctness risk in the first iteration.
- Engine API: the scene bridge exposes a scoped conversion entry point,
  e.g. `respawn_authoring_entities(world, scene, &[EntityId], &mut map)`,
  reusing `BridgeAssetState` semantics but resolving asset handles through a
  persistent handle cache (see below). This is a new public engine API and
  is the crate-boundary decision this ADR records.
- Persistent asset handle cache: `BridgeAssetState.mesh_handles` /
  `material_handles` / `animation_clip_handles` / `audio_handles` are today
  conversion-local. For respawn-in-place they move to (or are seeded from) a
  world resource keyed by `AssetId`, so a respawned entity reuses the
  already-loaded `Handle<Mesh>` instead of cloning mesh data into the store
  again. Rollback semantics stay conversion-local: a failed respawn removes
  only what it added, exactly as `apply_conversion_plan` does today.

With Stage B, the idle path is zero work, a value edit costs one entity
respawn, and only structural edits pay full conversion — which ADR 0071
keeps bounded.

## Consequences

- Scene View frame cost becomes proportional to what changed, not to scene
  size. Large imported models (the ADR 0071 motivating case) render at
  editor-idle cost after the first frame.
- `GpuMeshCache` and `Assets<Mesh>` persist with the world, eliminating
  per-frame mesh deep copies and GPU re-uploads without moving those caches
  out of the world (the option considered and rejected: renderer-owned mesh
  cache — unnecessary once the world persists, and the world is the natural
  owner).
- Memory: the preview world and its GPU resources stay alive while the
  Scene View is open. Bounded by scene content; released on tab close,
  project switch, or `PreviewKey` rebuild.
- New invariant to maintain: every document/manifest mutation path must bump
  its revision. The debug-build hash check exists to catch violations early.
- `DebugLines` clearing becomes an explicit per-frame step; forgetting it
  produces visible line accumulation (self-announcing, low risk).
- ADR 0068 consequence text must be amended (see above) in the same PR as
  Stage A.
- Play mode, player, packaging, and all strict-conversion hosts are
  untouched.

## Implementation plan

Stage A (editor-only except one engine addition):

1. `crates/editor/src/session.rs`: `document_revision` counter + accessor;
   bumps at the enumerated mutation/undo/redo/load sites.
2. Manifest revision counter beside the editor's manifest owner (`ui.rs` /
   asset management state).
3. `crates/editor/src/scene_view.rs`: `PreviewWorld` retention, `PreviewKey`
   comparison, per-frame maintenance path (DebugLines clear, ViewportSize,
   camera), transform-preview fast path via `bridge_map`, rebuild fallback
   for non-transform gesture previews, debug-build hash check.
4. `crates/engine`: verify/ensure particle `simulate_preview` is
   elapsed-deterministic on a persistent emitter (reset-before-simulate);
   expose whatever minimal reset hook that requires.
5. Amend ADR 0068.

Stage B (engine + editor):

6. Scene diffing in the editor (entity-level classification).
7. Engine `respawn_authoring_entities` + persistent asset handle cache
   resource; rollback tests mirroring existing conversion rollback tests.
8. Editor wiring: value-edit → single respawn; structural → rebuild.

Tests (minimum):

- Idle frame reuses the world (world/entity identity stable across frames).
- Each revision source triggers exactly one rebuild (commit, undo, redo,
  load, manifest reimport, game module reload, each toggle).
- Gizmo drag updates `Transform` without rebuild; cancel restores authored
  value; commit converges.
- Animation preview toggle off restores rest pose byte-identically to a
  fresh conversion.
- Stage B: value edit respawns only the edited entity (other entity IDs
  unchanged); skinned entity respawn removes its old skeleton entities;
  failed respawn rolls back only its own additions.

## Open questions (resolve during implementation)

1. Does `ParticleEmitter::simulate_preview` accumulate state across calls,
   or is it already a pure function of `elapsed`? Determines item 4.
2. Does the inspector "component_preview" path carry non-transform
   components often enough in practice to justify pulling Stage B's
   single-entity respawn into Stage A?
3. `AuthoringToRuntimeMap` currently exposes lookup by `EntityId`; confirm
   it also survives partial respawn updates or needs a small mutable wrapper
   in the editor.
