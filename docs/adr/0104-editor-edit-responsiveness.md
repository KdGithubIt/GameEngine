# ADR 0104: Editor Edit Responsiveness

Status: Accepted
Date: 2026-08-13

## Context

Adding a component or editing one Inspector field visibly stalls the editor,
and the editor feels heavy even while idle. Reading the paths involved, one
committed scene edit synchronously performs all of the following before the
next frame is drawn:

| Step | Where |
|---|---|
| Clone the whole scene into a transaction working copy | `Transaction::begin` |
| Validate the whole scene | `Transaction::commit` |
| Clone the whole scene again for the authoring undo entry | `AuthoringSession::commit` |
| Clone the whole scene again into `CurrentDocument` | `EditorSession::sync_scene_document_from_session` |
| Serialize the whole scene to canonical JSON to decide the dirty flag | `EditorSession::mark_dirty` → `document_snapshot` |
| Validate the whole scene again, **with filesystem I/O** | `EditorApp::refresh_scene_problems` |
| Rebuild the entire Scene View preview world on the next frame | `SceneView::show` (`PreviewKey` mismatch) |

Two of those dominate.

**Whole-file hashing during validation.**
`refresh_scene_problems` calls `engine::validate_builtin_component_assets`,
which for each imported asset reference reaches
`validate_import_source_files` → `fingerprint_source` →
`gltf_import::fingerprint_gltf_source`. That function does
`std::fs::read(path)` of the complete model file and hashes it with a
byte-at-a-time FNV-1a loop. A scene referencing a 50 MB FBX or PMX therefore
reads and hashes 50 MB on the UI thread for **every** committed edit.
`refresh_scene_problems` has 45 call sites, including
`apply_component_edit`, which runs it on every non-draft component edit.

**Full preview-world rebuild.**
Any committed edit bumps `EditorSession::document_revision`, which changes
`PreviewKey`, which discards the preview world and calls
`build_preview_app_with_sky`: a fresh `engine::App`, a clone of the manifest,
and a complete re-spawn of the scene. Parsed glTF data survives through the
shared import cache (ADR 0071), but `GpuMeshCache` is a **world resource**, so
every mesh is uploaded to the GPU again on each rebuild.

Three further costs run continuously rather than per edit:

- `ProjectFileWatcher::poll` walks the whole `assets/` tree with a recursive
  `read_dir` + `metadata` scan into a `BTreeMap<PathBuf, FileStamp>` every
  500 ms, on the UI thread, and requests a repaint each time.
- `EditorApp::bone_choices_for_selected_entity` calls
  `engine::import_gltf_path`, which parses the model file with no cache, once
  per Inspector frame while an entity with `engine.bone_attachment` is
  selected.
- The Inspector rebuilds `entity_choices` (label plus hierarchy-path string
  for every scene entity) and clones every candidate `ComponentSchema` for the
  Add Component picker on every frame, and `SceneView` recomputes
  `manifest_content_hash` and `collect_entity_positions` on every frame.

`ImportSettings::source_fingerprint` is persisted project data, so changing
what it stores is a format decision, and `GpuMeshCache`'s ownership is an
engine API decision. Both cross crate boundaries, which is why this is an ADR
rather than a set of local fixes.

## Decision

1. **Staleness is detected by a cheap stamp; content hashing becomes
   explicit.** `ImportSettings` gains an additive
   `source_stamp: Option<SourceStamp>` recording modified-time and length for
   the source and each declared dependency. Editor-side validation compares
   stamps only. The content fingerprint remains the authority for deciding
   whether an import must re-run, and is computed where that decision is made
   — import, reimport, and packaging — never on a per-edit validation pass.

2. **Validation splits by cost, and only the cheap half is synchronous.**
   `refresh_scene_problems` keeps running schema, reference, and
   component-value validation inline, because those are pure in-memory passes
   over the scene. Every check that touches the filesystem moves behind a
   debounced pass that runs after edits stop, and publishes into the same
   Problems set. Diagnostic codes and targets are unchanged; only *when* an
   I/O-backed problem appears changes.

3. **Uploaded GPU meshes outlive the preview world.** `GpuMeshCache` moves
   from a per-world resource to a shared handle owned alongside the existing
   shared glTF import cache, so rebuilding the preview world no longer
   re-uploads geometry that has not changed.

4. **The project file watcher leaves the UI thread.** Snapshot scanning runs
   on a worker; the UI thread only drains the resulting events. The 500 ms
   cadence is a property of that worker, not a per-frame repaint driver.

5. **Model parsing is never per frame.** Inspector data derived from a model
   source — the bone list is the current case — is resolved through a cache
   keyed by the asset it came from, and recomputed only when that asset or the
   selection changes.

6. **Per-frame work in the Inspector and Scene View is derived once per
   change, not once per frame.** Entity choice lists, the Add Component
   candidate list, and the manifest hash are recomputed when the scene
   revision or manifest changes.

7. **Dirty state is decided by revision, not by re-serializing the
   document.** `AuthoringScene` already carries a revision; `mark_dirty`
   compares that instead of producing canonical JSON on every edit.

Points 1–3 are the ones with cross-crate or persisted-format consequences.
Points 4–7 are stated here so the whole picture is recorded in one place;
they can land independently and in any order.

## Consequences

- The dominant per-edit cost disappears: no model file is read or hashed as a
  side effect of editing a component.
- An I/O-backed problem (a deleted texture, a stale import) appears shortly
  after an edit settles rather than in the same frame. The Problems panel is
  already an asynchronous surface with respect to background imports and
  builds, so this matches how the rest of it behaves — but it is a visible
  behavior change and is called out for that reason.
- A stamp can miss a content change that preserves modified-time and length.
  That is why §1 keeps the content fingerprint as the authority for import
  decisions: the cheap check gates editing feedback, not correctness of the
  import pipeline. Packaging keeps using the fingerprint.
- Sharing `GpuMeshCache` across preview worlds means its lifetime is no
  longer bounded by the world that filled it. It must be invalidated when its
  source asset changes, and released when the project closes.
- Moving the watcher off the UI thread introduces a thread boundary where
  there was none, and its events become ordered with respect to editor
  actions only through the existing `suppress_once` mechanism.
- Several of these are measurable rather than merely arguable, so this ADR is
  only worth accepting alongside numbers; see Verification.

## Alternatives Considered

**Keep hashing, but only for the assets referenced by the edited entity.**
Rejected: it reduces how often the stall happens without removing it, and one
entity referencing one large model still stalls on every edit to it.

**Cache the fingerprint in memory, keyed by path.** Rejected on its own: the
cache still has to be invalidated by something, and the only cheap signal
available for that is the modified-time and length stamp of §1. That makes
the stamp the real mechanism and the hash cache an extra layer over it.

**Make the whole of `refresh_scene_problems` asynchronous.** Rejected: schema
and reference errors are the feedback that tells an author their edit was
wrong, and delaying them would make the editor feel less correct, not more
responsive. The split in §2 exists precisely to keep those inline.

**Diff the preview world instead of rebuilding it.** This is the more complete
answer to §3 and is not rejected — it is deliberately out of scope here.
ADR 0072 already established a transform fast path; extending that to
arbitrary components is a larger design that should follow its own ADR. Making
GPU uploads survive a rebuild is the smaller, independent step and does not
constrain that later work.

**Raise the file-watcher interval instead of moving it off-thread.**
Rejected as the primary fix: it trades responsiveness to external edits for
frame time and leaves a full synchronous tree walk on the UI thread. Replacing
polling with OS change notifications would be better still, but it adds a
dependency and belongs in its own decision.

## Compatibility and Migration

- `ImportSettings::source_stamp` is additive and `#[serde(default,
  skip_serializing_if = "Option::is_none")]`, matching every other optional
  field in that struct. A manifest written before this change parses
  unchanged.
- A missing stamp means "unknown", which must be treated as *not stale* for
  editing feedback and resolved by the existing fingerprint path where import
  decisions are made. An absent stamp must never be reported to the author as
  a problem, or every pre-existing project would light up with errors on
  first open.
- The stamp is written on the next successful import; no migration pass over
  existing manifests is required, and none should be added, because a
  migration would have to read every source file — exactly the cost this ADR
  removes.
- `source_fingerprint` keeps its meaning and its place in the manifest. This
  ADR narrows *when* it is computed, not what it is.
- Modified-time is not comparable across machines or after a checkout, so the
  stamp is a local hint only. Nothing in packaging or CI may depend on it.
- `GpuMeshCache` is `pub` in `crates/engine`; changing its ownership is a
  public API change and must update every caller in the same change, per the
  breaking-change protocol in `docs/AGENTS.md` §3.
- No authoring command, scene format, `StableId` format, or diagnostic code
  changes.

## Verification

Because this ADR is entirely about cost, it is verified by measurement, not
only by tests:

- Time one `AddComponent` on a scene referencing a large imported model,
  before and after. The per-edit cost must no longer scale with the size of
  the source file.
- Confirm no `std::fs::read` of a model source occurs on the edit path, by
  instrumentation or by a test double for the import layer.
- Confirm the same set of diagnostic codes is produced for a scene with a
  missing texture, a stale import, and a broken material dependency, before
  and after the split in §2.
- Confirm a manifest without `source_stamp` loads, produces no new problems,
  and gains a stamp after one successful reimport.
- Confirm the preview world rebuild after a component edit performs no GPU
  mesh upload for meshes that were already resident.
