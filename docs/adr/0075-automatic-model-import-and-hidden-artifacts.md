# ADR 0075: Automatic Model Import and Hidden Import Artifacts

Status: Accepted
Date: 2026-07-21

## Context

ADR 0074 made an imported glTF/GLB source placeable by generating a prefab
from it. Using that on a real model exposed two mistakes in how the result
reaches the author.

**Getting a model into the project has three entry points that behave
differently.** `start_gltf` is called from exactly two places — the Asset
Browser's `Register Asset` and `Reimport` items — so:

| How the file arrives | Registered | glTF imported | Prefab generated |
| --- | --- | --- | --- |
| Copied in with the file manager | no | no | no |
| Dragged onto the Asset Browser | yes | no | no |
| Right-click → Register Asset | yes | yes | yes |

Only the third path produces a usable model. The first two leave a file that
looks present but resolves to nothing, and nothing on screen explains which
step is missing. A `git pull` or a branch switch that brings in a model
behaves like the first row.

**The generated prefab is written next to its source**, so the Asset Browser
shows two entries for one model and the author has to know that the second
one — not the file they added — is the thing to place. Unity and Godot both
show only the source file and instantiate *it*; their import artifacts live
in `Library/` and `.godot/imported/`, outside the asset tree. Placing a
build product beside hand-authored assets also puts it in version control and
in the author's way, for a file that is rewritten on every reimport and is
never meant to be edited.

## Decision

### 1. Any glTF/GLB under `assets/` is registered and imported automatically

The existing `ProjectFileWatcher` already reports `Created` and `Modified`
events for `assets/`. Those events now drive import:

- an unregistered `.gltf`/`.glb` is registered, then imported;
- a registered one is reimported when its content stamp changes;
- external drops (`import_external_asset_files`) queue the same job instead
  of registering without importing.

`Register Asset` remains for the non-model formats it already covers.
`Reimport` remains as a manual override for cases the stamp cannot see, such
as an edited texture sidecar reached through a path the watcher does not
cover.

Because `AssetImportManager` runs one job at a time, the editor keeps a queue
of pending sources and starts the next when the current job finishes. Queue
entries are deduplicated by `AssetId`, so a burst of writes from a branch
switch imports each source once.

### 2. The generated prefab lives in `.engine/imported/`

The prefab is written to `.engine/imported/<source asset id>.prefab.json`
and is **not registered in the asset manifest**. `.engine/` is already
established as non-authoring editor data (ADR 0060's asset trash; the
authoring spec excludes it from authoring assets), and the Asset Browser
scans only `assets/`, so the artifact disappears from the author's view
without any filtering rule.

`ImportSettings::generated_prefab` therefore stores a **project-relative
path**, not an asset ID. Nothing in a scene or package references the prefab:
instantiating it emits ordinary entities whose components reference the mesh,
material, and skin sub-assets that *are* in the manifest, so packaging and
build reachability are unaffected.

### 3. The source entry is what gets placed

A registered `.gltf`/`.glb` in the Asset Browser becomes draggable into the
Scene View and gains `Instantiate in Scene`, both of which apply the prefab
generated for it. One model is one row in the browser, and that row is the
thing you place — the arrangement Unity and Godot both use.

A source whose import has not finished, or which produced no prefab (a
document that draws nothing), reports why instead of silently doing nothing.

## Consequences

- Adding a model is one action: put the file in `assets/`. The path taken to
  get it there no longer changes the outcome.
- The Asset Browser shows one row per model instead of two, and the row is
  the source file the author recognizes.
- Generated prefabs leave version control. Projects that committed one from
  ADR 0074 can delete it; nothing references it by ID.
- Import work now starts without an explicit user action, so a large model
  can occupy the import worker right after a branch switch. The existing
  progress and cancel UI covers this, and the queue keeps bursts serialized
  rather than dropped.
- An author who wants a customized hierarchy still instantiates the model and
  saves their own prefab, which lives in `assets/` and is never rewritten.

## Alternatives Considered

- **Keep manual registration and only fix the drag-and-drop path.** Rejected:
  the file-manager and version-control paths are how models actually arrive
  in a project, and they would still be silently broken.
- **Register the generated prefab in the manifest but hide it with a browser
  filter.** Rejected: the entry would still be in `asset_manifest.json` and in
  asset pickers, and every consumer would need the same filtering rule. Not
  registering it removes the problem instead of masking it.
- **Write the prefab into `assets/.imported/`.** Rejected: a dot-directory
  inside the asset tree still ships with the project's assets and would need
  excluding from packaging, path validation, and folder operations that
  already treat everything under `assets/` as authoring data.
- **Import into the manifest but instantiate straight from the parsed
  document, with no prefab file.** Rejected: instantiation would re-parse the
  source and could not be inspected or diffed, and the prefab file is what
  lets `Instantiate in Scene` reuse the existing Phase 33 path unchanged.

## Compatibility and Migration

- This replaces ADR 0074 §4. Sections 1, 2, 3, and 5 of ADR 0074 are
  unchanged.
- `ImportSettings::generated_prefab` changes meaning from an asset ID to a
  project-relative path. The field is additive and no in-tree project stores
  it yet, so no manifest migration is required; a stale value is overwritten
  by the next import.
- A `<name>.prefab.json` generated beside a source by ADR 0074 is left on
  disk. It is a normal prefab and keeps working if instantiated; it is simply
  no longer produced or updated, and can be deleted.
- No scene, prefab, or manifest schema version changes.
