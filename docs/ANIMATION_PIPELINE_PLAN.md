# Animation Retarget Pipeline — Implementation Plan (AP-1 … AP-9)

Status: Historical record (AP-1 … AP-9 implemented; the legacy authoring
shapes it references were removed by ADR 0091)
Date: 2026-07-21 (AP-6 … AP-9 added 2026-07-22)
Design authority: ADR 0077, 0078, 0079, 0080, 0081 (read the ADR for the
phase before implementing; this document adds sequencing and acceptance
detail, it never overrides an ADR).

Execution model: each phase is implemented by an implementation agent,
reviewed against its gates, and must leave the workspace green before the
next phase starts. Phases are strictly ordered — later phases assume
earlier data models.

## Global rules (every phase)

- Read `docs/RUST_CODE_STYLE.md` first. English only in `.rs` files.
- Gates before a phase is called done:
  `cargo fmt --all --check` / `cargo clippy --workspace --all-targets --
  -D warnings` / `cargo test --workspace` / `cargo doc --workspace
  --no-deps` — all clean.
- No `unwrap()`/`panic!()` for recoverable failures in library code;
  diagnostics over panics at import/spawn time (existing codes style:
  `anim.*`).
- Do not change `Vertex`, `StableId` format, scene/prefab
  `schema_version`, or manifest schema version. Additive
  `ImportSettings` fields only, with `skip_serializing_if`.
- AP-6 onward: commit at the end of the phase once every gate is green
  (one commit per phase; AP-1 … AP-5 predate this rule and landed as a
  single commit).
- Cross-crate call-site updates land in the same phase (breaking-change
  protocol, `docs/AGENTS.md` §3).

## AP-1 — Skeleton assets, BoneId, clip re-binding (ADR 0077)

Scope:
- `crates/engine/src/skeleton_asset.rs` (new): `BoneId`,
  `SkeletonIdentity` + canonical hash, `SkeletonAsset`, `BoneDef`,
  identity quantization (1e-4, w>=0 canonicalization), FNV-1a 64.
- `skinning.rs`: `Skeleton` gains `bone_ids: Vec<BoneId>`, `asset:
  Option<AssetId>`; `spawn_skin` fills them.
- `animation.rs` / `anim_graph.rs`: `AnimChannel::target_joint` →
  `target_bone: Option<BoneId>`; `AnimationClip` gains `skeleton`,
  `skeleton_identity`, `root_bone`; `animation_system` resolves BoneId
  via the skeleton's id→index map; validation diagnostic
  `anim.clip_missing_skeleton`.
- `gltf_import.rs`: build `SkeletonAsset` per skin source, compute
  identity, dedupe against manifest `skeleton_records` (adopt AssetId +
  BoneIds on identity match), reimport name-matching (keep / add fresh /
  retire; `next_bone_id` monotonic), emit `anim.skeleton_rebind`
  diagnostic; resolve clip channels to BoneIds; auto-detect `root_bone`
  (topmost bone with a translation channel).
- `asset.rs`: `ImportSettings::skeleton_records` (+ record types),
  additive serde.
- `scene_bridge` spawn paths + editor import result handling updated.
- Examples (`skinned_mesh`, `spire_lite`, `sample_game`) compile with
  `skeleton: None` / `target_bone` forms.

Required tests:
- identity hash: jittered rest (<1e-4) → equal; renamed bone → different.
- dedupe: two synthetic imports, same rig → one skeleton id adopted.
- reimport: rename = one retired + one added, kept bones keep ids,
  `next_bone_id` never decreases; ids never reused.
- clip validation: `target_bone` without skeleton → diagnostic.
- animation_system drives joints via BoneId incl. out-of-range skip.
- serde: manifest with/without `skeleton_records` round-trips.

## AP-2 — Model IR and parser boundary (ADR 0078)

Scope:
- `crates/engine/src/model_ir.rs` (new): `ModelDocument` + Ir* types,
  normalization contract rustdoc.
- `gltf_import.rs` → `parse_gltf(...) -> ModelDocument` (parsing only).
- `crates/engine/src/model_import.rs` (new): `build_import_result(...)`
  — sub-asset IDs, skeleton identity/dedupe/rebind (moved from AP-1
  location if needed), clip BoneId resolution, prefab-generation input.
  `GltfImportResult` keeps name/shape; import cache (ADR 0071) behavior
  unchanged.

Required tests:
- builder tests on hand-written `ModelDocument` (no .glb): skeleton
  dedupe, clip binding, mesh/material ID assignment parity with a real
  glTF fixture (import an existing fixture both ways, assert equal
  catalogs).
- existing glTF tests keep passing unchanged (behavior parity).

## AP-3 — RetargetMap, retarget function, derived cache (ADR 0079)

Scope:
- `crates/authoring` or `engine` persisted `*.retarget.json`
  (schema_version 1) + serde + validation (`anim.retarget_map_stale`);
  follow existing persisted-format patterns (ADR 0022 style separation,
  migration test).
- `crates/engine/src/retarget.rs` (new): `retarget_clip` pure function
  per ADR 0079 math §2 (model-space delta transfer, translation policy,
  chain nearest-position mapping, key-union resampling with 4096/60 Hz
  cap), `RETARGET_ALGORITHM_VERSION`.
- `crates/engine/src/derived_cache.rs` (new): generic get/put under
  `.engine/cache/`; single audited `cache_key` fn.
- Editor: map-creation flow on cross-skeleton clip assignment
  (name-heuristic prefill, unresolved list as diagnostics for now), bake
  on resolution miss, `anim.retarget_map_missing` diagnostic.
- Packaging: bake walk over reachable (clip, skeleton) pairs; missing
  map = blocking diagnostic (ADR 0045 policy).

Required tests:
- pure-function golden tests: identity retarget (same skeleton) is
  lossless within 1e-5; T-pose→A-pose synthetic rig transfers rotation
  correctly (hand-computed case); unequal chain counts map by nearest
  normalized position.
- translation policy: HipHeightRatio scales root translation, other
  bones keep target rest translation.
- cache key test: each key component (algo version, fingerprint, clip
  id, both identities, map json) independently changes the key.
- cache round-trip; deleting `.engine/cache` recovers by re-bake.

## AP-4 — Contact detection + runtime foot IK (ADR 0080)

Scope:
- Detection in the import builder + on baked retarget output
  (re-detected, not copied): candidate name patterns +
  `ImportSettings::contact_bones` override; thresholds as documented
  constants; `AnimationClip::contacts`.
- `crates/engine/src/foot_ik.rs` (new): `FootIk` component (registered
  in builtin component registry per ADR 0027 conventions),
  `foot_ik_system` on the fixed schedule after `animation_system`:
  contact weights from active clips (crossfade-aware, 0.1 s easing),
  vertical ray vs static colliders (Phase 58 slab test reuse), two-bone
  analytic IK with knee-plane preservation, pelvis lowering, clamped by
  `max_correction`, skip-with-diagnostic on degenerate chains.

Required tests:
- detection: synthetic walk (sine foot) yields expected intervals; gap
  bridging and min-duration rules; moonwalk-style always-slow foot does
  not produce a full-clip interval when height gate fails.
- two-bone solver: reachable target exact within 1e-4; unreachable →
  skip; knee plane preserved.
- system: planted interval + ground offset → ankle lands on ground
  within tolerance; no ground hit → pose untouched; blend of two clips
  eases weight.

## AP-5 — Editor UX for observability (design-review §9)

Scope:
- Import/bind report UI: per-skeleton bone match table (kept / added /
  retired; matched-by-name fallbacks flagged) surfaced from
  `anim.skeleton_rebind` data — Problems panel entry opens a detail
  view.
- RetargetMap inspector: pair list with unresolved-bone list, re-run
  heuristic button, stale-identity banner.
- Contact interval display on clips (read-only timeline list v1 +
  manual add/remove/edit writing the override metadata).
- No new persisted formats beyond what AP-3/AP-4 defined.

Required tests: UI-side unit tests per existing `crates/editor/src/ui`
test patterns (state → widget model), not pixel tests.

## AP-6 — Fingerprint strictness + multi-skin map creation

Two independent hardenings; no new persisted formats.

Scope (a) — fingerprint blocking (`TODO(anim-pipeline)` at
`crates/engine/src/scene_bridge.rs` in `resolve_cross_skeleton_clip`):
- A source without a recorded `source_fingerprint` must **block**
  cross-skeleton resolution with a new diagnostic
  `anim.retarget_source_unfingerprinted` (error; entity's `Animator`
  removed, same guarantee as the map-missing path) instead of falling
  back to the sub-asset ID as the cache-key fingerprint. The fallback
  is a same-session-only uniqueness guarantee and poisons the on-disk
  cache across sessions.
- Audit `crates/editor/src/build.rs`'s bake walk for the same fallback
  shape; packaging must emit a blocking `BuildDiagnostic` for an
  unfingerprinted source rather than bake under a weak key.
- Remove the two TODO comments this phase resolves.

Scope (b) — multi-skin map creation
(`crates/editor/src/ui/assets.rs::create_retarget_map_from_browser`,
documented v1 simplification):
- When either side of the (source, target) pair records more than one
  skeleton (`ImportSettings::skeleton_records.len() > 1`), open a
  picker window (egui, existing editor window patterns) listing each
  side's skeletons as `<skin name> (<bone count> bones)`; the
  confirmed pair feeds the existing generation path. Exactly one
  record on both sides keeps today's one-click behavior.
- The picker is pure UI state on the editor app (no persistence);
  cancel = no file written.

Required tests:
- resolution without fingerprint → diagnostic, Animator removed, cache
  untouched (no entry written).
- packaging without fingerprint → blocking diagnostic, no `PackageCopy`
  staged.
- picker model: >1 records on one side → picker state with the right
  rows; selection → map file for the chosen pair (reuse the existing
  `create_retarget_map_writes_file_and_registers_manifest_entry` test
  pattern); single-record sources bypass the picker.

## AP-7 — Packaging reachability trace + `always_package`

Resolves the `TODO(anim-pipeline)` at `crates/editor/src/build.rs`
(`bake_registered_retarget_clips` bakes every registered map).

Scope:
- `RetargetMap` gains `#[serde(default, skip_serializing_if =
  "is_false")] pub always_package: bool` (schema_version stays 1 —
  additive field, absent = false, forward-compatible read of files
  written by this build is unaffected). Round-trip serde test.
- New reachability walk in `build.rs`: iterate manifest entries whose
  path ends in `.scene.json` / `.prefab.json`, load via the authoring
  crate, collect every entity carrying both `engine.animator` and
  `engine.skeleton`. For each, resolve:
  - target skeleton: the entity's `engine.skeleton` `skin` sub-asset →
    owning source's `skeleton_records` entry (via manifest, same
    lookup the bake walk already performs);
  - source skeleton: the animator's `clip_source` → that source's
    skeleton record for the clip's bound skin.
  The needed-pair set is `{(source_skeleton, target_skeleton) | source
  != target}`. Prefabs count as roots unconditionally (they may be
  spawned by script; consistent with `analyze_build`'s documented
  conservative policy).
- Bake set = maps matching a needed pair ∪ maps with `always_package`.
  A *needed* pair with no registered map stays a blocking diagnostic
  (unchanged). A registered map matching no needed pair and not
  `always_package` is skipped with an **info-level, non-blocking**
  diagnostic (`RetargetMapNotReached` kind) so the narrowing is
  observable in the build report.
- Editor RetargetMap inspector (`anim_ux.rs`): show an
  "Always package" checkbox writing the flag back to the file (same
  write path as re-run name matching).
- Update the ADR 0079 §4 scope note and delete the TODO comment.

Required tests:
- scene referencing a cross-skeleton pair → its map baked; an
  unreferenced map → skipped with the info diagnostic; the same map
  with `always_package: true` → baked.
- prefab-only reference (no scene) → baked.
- needed pair with no map → still blocking.
- serde: map without the field round-trips unchanged (byte-stable
  output for existing files).

## AP-8 — Player loads packaged baked clips (ADR 0079 §4 completion)

Resolves the `TODO(anim-pipeline)` at `crates/engine/src/scene_bridge.rs`
(`resolve_cross_skeleton_animator_clips`). Investigation facts this
design relies on: the shipped player (`crates/engine/src/bin/player.rs`)
runs the same `spawn_from_authoring_scene` path as editor Play, so today
it silently re-bakes retargets at runtime into
`<package_root>/.engine/cache/` and never reads the `baked_anim/` files
packaging staged; the baked file name is already deterministic
(`<cache_key file_stem>.<BAKED_CLIP_FILE_EXTENSION>`) on both sides.

Scope:
- New engine resource `PackagedBakedClips { root: PathBuf }` (in
  `retarget.rs` or a small module near it), inserted **only** by the
  player binary, pointing at `<package_root>/baked_anim`.
- `resolve_cross_skeleton_clip` branches on the resource:
  - resource present (shipped player): compute the cache key exactly as
    today (single audited key fn, unchanged), look up
    `baked_anim/<key>.clip.json`, `deserialize_baked_clip` on hit. On
    miss: diagnostic `anim.retarget_bake_missing_from_package` (error)
    + Animator removal. **No runtime bake and no cache write in the
    player** — an end-user install must never depend on a writable
    install directory, and a miss means AP-7's trace or the map's
    `always_package` flag is wrong, which must surface, not self-heal.
  - resource absent (editor Play, tests): today's bake-or-cache path,
    unchanged.
- `player.rs`: insert the resource unconditionally (pointing at
  `<package_root>/baked_anim` whether or not the directory exists) —
  a package without retargets never consults it, and a cross-skeleton
  entity in a package whose `baked_anim/` is absent *should* fail
  loudly rather than fall back to a runtime bake.
- Fingerprint source in the player: the packaged
  `asset_manifest.json` already carries `source_fingerprint` (AP-6
  guarantees bake refused without it), so key computation needs no new
  inputs.
- Delete the TODO comment.

Required tests:
- with the resource + staged baked file → Animator repointed to the
  deserialized baked clip; no `.engine/cache` write.
- with the resource + missing file → diagnostic + Animator removed.
- without the resource → existing behavior (existing tests unchanged).
- integration: a packaging bake's staged file round-trips through the
  player-path resolution (same key both sides — guards the key parity
  that makes the whole scheme work).

## AP-9 — FBX import via ufbx (ADR 0081)

Scope:
- `crates/engine/Cargo.toml`: feature `fbx-import` (default), `ufbx`
  under `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`,
  optional, gated per ADR 0081 §2.
- `crates/engine/src/fbx_import.rs` (new): `parse_fbx` /
  `parse_fbx_path` → `ModelDocument` honoring the full `model_ir.rs`
  normalization contract via ufbx load options (meters, +Y up
  right-handed, pivots/PreRotation baked, per-stack clips resampled to
  linear keys, `anim.fbx_curve_resampled` diagnostic), plus
  `import_fbx_bytes` / `import_fbx_path` /
  `import_fbx_path_with_contact_bones` /
  `fingerprint_fbx_source` / `fbx_source_dependencies` — exact glTF
  parity, composition through `model_import::build_import_result`.
- `model_import.rs`: `import_model_path` / `import_model_bytes` /
  `fingerprint_model_source` / `model_source_dependencies` dispatching
  on extension (ADR 0081 §4); `ModelImportError` wrapping both parser
  error types + `UnsupportedExtension`.
- Migrate production call sites to the dispatching form (same phase,
  breaking-change protocol): editor `asset_import.rs`, `build.rs`
  (bake walk + `locate_skeleton_source_id`), `scene_bridge`
  (`gltf_cache.rs` / `asset_load.rs`), `gltf_prefab.rs`. Test-only
  call sites may keep the glTF-specific entry points.
- Editor: `.fbx` joins the registerable-model extension list
  (`asset_browser.rs` + its extension-stripping logic + browser tests);
  no other editor change — sub-assets, skeleton records, retarget,
  contact detection all flow through the shared builder.
- Docs: rewrite `docs/FBX_IMPORT.md` (direct import is now the primary
  path; conversion remains the documented fallback; format-switch =
  re-registration note per ADR 0081 §5), update
  `docs/manual/quickstart-animation.md` and `docs/USER_MANUAL_JA.md`
  Mixamo sections, add 0081 to `docs/adr/README.md` index (done at
  design time if not already).
- Test fixture: a small binary FBX checked in under the engine test
  fixtures (a rigged two-bone box with one clip and one embedded or
  sidecar texture; generated once, kept tiny). If authoring a
  deterministic fixture in-repo proves impractical, gate the
  fixture-based tests behind `#[ignore]` with a doc comment naming the
  regeneration step — but exhaust the in-repo option first.

Required tests:
- `parse_fbx` on the fixture: node TRS normalized (no pivot residue),
  skin joints + inverse binds present, clip channels joint-bound and
  linear-sampled, duration correct.
- import parity shape: sub-asset IDs deterministic across two imports
  of the same bytes.
- dispatch: `.fbx` routes to the FBX parser, `.glb` unchanged, unknown
  extension → `UnsupportedExtension`; wasm32/feature-off arm compiles
  (cfg test or cfg-gated assertion).
- editor: `.fbx` appears registerable; registration produces the same
  sub-asset row kinds as a glTF source (reuse existing browser test
  patterns).
- All existing glTF tests unchanged.

## Explicitly out of scope (needs its own ADR)

- USD parser (dependency decision).
- FBX export (write side).
- Cross-machine shared derived cache.
- Dynamic runtime retargeting path (the `always_package` flag is the
  supported escape hatch for dynamically-swapped clips).
- Hand/weapon contacts, full-body IK.
