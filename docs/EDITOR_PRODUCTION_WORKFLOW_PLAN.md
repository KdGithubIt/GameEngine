# Editor Production Workflow Improvement Plan

Status: Implemented  
Date: 2026-07-19  
Scope: Unity / Unreal Engine / Godot-like production workflows for the current Rust game editor

## 1. Purpose

This plan records the editor improvements identified while building and testing
the `coin_collision_loop` proving project through the normal editor workflow.
The goal is not to copy another editor's appearance. The goal is to provide the
same practical production guarantees: repeated authoring is fast, project state
is durable, source code is discoverable, assets can be organized visually, and
Play behavior can be tested deterministically.

This document is a delivery plan. Canonical serialized formats, command
semantics, crate boundaries, and runtime contracts remain owned by:

- `docs/AI_FRIENDLY_AUTHORING_SPEC.md`
- `docs/RUST_CODE_STYLE.md`
- accepted records under `docs/adr/`
- `docs/RUST_GAME_EDITOR_READINESS_PLAN.md`

An implementation that changes one of those contracts MUST update the
specification or add an accepted ADR before relying on the change.

## 2. Audit baseline

The proving-project exercise exposed the following production blockers.

1. Repeated entities, coins, and UI elements are slow to create and arrange.
2. Prefab operations exist, but are not sufficiently visible or integrated with
   bulk scene authoring.
3. UI documents support basic authoring, but fixed-size layouts do not preview or
   scale reliably across different aspect ratios.
4. Creating a Rust script immediately asks Windows to choose an application for
   `.rs` files when no association exists.
5. Every individual Rust script creation can trigger a build, multiplying wait
   time during a short authoring session.
6. External asset changes are not observed automatically and can require an
   editor restart.
7. Restarting the editor does not reliably restore the last project, document,
   selection, and useful panel state.
8. Running ordinary Cargo commands for a generated game project depends on a
   hidden editor Cargo configuration, making validation outside the editor
   difficult to discover.
9. Automated WASD injection does not provide a reproducible end-to-end Game View
   test path through the existing Input Debugger.
10. Inspector component headers cannot navigate to the Rust definition that
    produced the component.
11. Built-in components do not expose their implementation in a safe,
    read-only form.
12. The Assets area shows a flat recursive file list. It cannot create folders,
    select a working folder, or move assets into folders by drag and drop.

## 3. Product rules

### 3.1 One normal workflow

Every feature in this plan MUST be reachable from the normal project editor.
A hidden JSON edit, a workspace-only Rust example, or a test-only helper does
not complete an editor feature.

### 3.2 Stable identity over paths and display names

Entities, components, and registered assets continue to use stable IDs.
Renaming a file, moving an asset, or changing a display label MUST NOT silently
create a different logical object.

### 3.3 Editor conveniences do not bypass authoring commands

Multi-edit, drag and drop, repeated placement, prefab operations, and folder
moves MUST invoke validated authoring or asset-management operations. The UI
must not directly mutate serialized documents behind the session's back.

### 3.4 Recoverable filesystem operations

Asset and folder operations MUST preflight their entire target set. A failure
must either leave the project unchanged or restore the original files and
manifest. Deletion uses project-local trash where practical.

### 3.5 Explicit source ownership

Project Rust is editable. Engine Rust is read-only from a game project. The
Inspector MUST make this distinction visible and must never create a second
component with a colliding stable ID.

### 3.6 Observable background work

Builds, imports, file synchronization, source indexing, and replay execution
MUST expose their current state, latest result, and recovery action in the
editor instead of failing silently.

## 4. Workstream A: hierarchy and repeated authoring

### 4.1 Multi-selection model

The current hierarchy selection set becomes a first-class editing selection.
Click selects one entity, Ctrl-click toggles an entity, and Shift-click selects
a visible range. Scene View selection and Hierarchy selection share the same
ordered selection model.

The Inspector shows:

- a common-value editor when all selected entities share the same value;
- a mixed-value indicator when values differ;
- an add-component action applied to every selected entity after preflight;
- a remove-component action that reports how many entities are affected;
- transform operations supporting absolute values and relative offsets.

All multi-entity changes execute as one undoable transaction. If any target is
invalid, the operation fails before changing the first entity.

### 4.2 Duplicate and repeated placement

Duplicate operates on the complete selected set, preserves parent-child
relationships within that set, generates new stable entity IDs, and returns the
new entities as the active selection.

The editor also provides:

- duplicate with configurable offset;
- repeat-last-duplicate;
- linear distribution along X, Y, or Z;
- equal spacing between first and last selected entities;
- align minimum, center, or maximum per axis;
- a placement mode that repeatedly instantiates a selected prefab until Escape.

These operations cover common coin, enemy, prop, and UI repetition without
requiring manual JSON generation.

### 4.3 Completion criteria

- Five coins can be created and evenly arranged from one authored coin without
  editing JSON.
- Multi-delete, duplicate, add-component, and transform edits undo in one step.
- A failed target validation leaves every selected entity unchanged.

## 5. Workstream B: prefab production workflow

Existing prefab create, instantiate, inspect, apply, revert, and unpack logic is
made consistently reachable from the Hierarchy, Inspector, Asset Browser, and
Scene View.

Prefab instances display a visible badge and their source asset. The Inspector
provides `Open Prefab`, `Apply`, `Revert`, and `Unpack` actions with disabled
reasons when an operation is unavailable.

The first delivery keeps the current whole-instance apply/revert behavior and
documents it clearly. True property-level overrides require a separate ADR that
defines persisted override identity, conflict handling, nested prefabs, and
migration behavior.

Completion requires repeated prefab placement, stable instance identity,
undo/redo, and a restart round trip through the ordinary editor.

## 6. Workstream C: UI Builder and responsive presentation

### 6.1 Editing productivity

The UI Builder gains multi-selection, duplicate, reorder, reparent, alignment,
distribution, and repeat-placement operations using the same transaction rules
as scene authoring. Drag and drop provides an explicit insertion indicator and
rejects parent-child cycles before mutation.

### 6.2 Responsive model

UI documents define a reference resolution and scale policy. Supported policy
choices are:

- constant pixels;
- scale with viewport using width, height, or a width-height blend;
- constant physical size when platform information is available.

Anchors determine how the element rectangle follows its parent. Min/max size,
aspect constraints, and safe-area padding are explicit properties rather than
preview-only behavior.

Changing the persisted UI schema requires a versioned schema update and an ADR.
Older UI documents receive deterministic defaults that preserve their current
appearance as closely as possible.

### 6.3 Preview and diagnostics

The preview offers named presets for common desktop, ultrawide, handheld, and
portrait resolutions plus a freely resizable viewport. It can draw reference
bounds, anchors, safe area, overflow, and clipped content.

Completion requires the same HUD to remain legible at 16:9 and ultrawide sizes,
with the preview and Game View producing the same layout result.

## 7. Workstream D: Inspector component source navigation

### 7.1 Project components

Every project component header gains a script icon and context menu with:

- `Open Script`;
- `Reveal in Game Code`;
- `Copy Component ID`.

Double-clicking the component header performs `Open Script`. The external editor
is launched only after an explicit user action.

The editor builds a source index from Rust files below `game/src/`. It extracts
the stable ID from declarations such as:

```rust
#[game_component(id = "game.player_controller")]
```

The index maps the stable component ID to the source-relative path, Rust type
name, and definition line. It does not infer identity from a file name or type
name. This index is editor-local and MUST NOT add machine-specific absolute
paths to scene documents or the native GameModule ABI.

The index refreshes after internal script generation and filesystem watcher
events. Duplicate stable IDs, malformed attributes, and missing source files
appear in Problems. When an ID is ambiguous, `Open Script` is disabled instead
of choosing an arbitrary file.

Opening a source uses the configured code editor and line-navigation template.
If no editor is configured, the editor shows a chooser with `Configure Editor`,
`Use OS Association Once`, and `Cancel`; it never invokes an association dialog
automatically during script creation.

### 7.2 Built-in components

Engine-owned components provide:

- `View Built-in Source (Read-only)`;
- `Copy Source`;
- `Show Documentation`;
- `Copy Component ID`.

The packaged editor SDK contains a source bundle matching the engine version.
Built-in component metadata resolves a stable component ID to its owning crate,
module path, SDK-relative source path, and definition location. The Source
Viewer displays that content in an internal read-only tab and clearly labels it
as engine-owned.

If the matching source bundle is missing, component editing continues to work.
The viewer displays the expected SDK version and a repair action instead of an
empty editor.

The editor MUST NOT automatically copy a built-in component into project code.
Such a copy could collide with the built-in stable ID and would not replace the
runtime spawn implementation safely. A future `Create Custom Component from
Template` action may generate a separate project component with a new ID, but
it is outside the first delivery.

### 7.3 Completion criteria

- An Inspector project component opens its exact `.rs` declaration by stable ID.
- A built-in component opens matching read-only SDK source without exposing an
  editable engine-workspace path.
- Missing, stale, and ambiguous mappings produce actionable diagnostics.
- No source-navigation metadata is persisted in scenes or asset documents.

## 8. Workstream E: Asset Browser folders and bulk organization

### 8.1 Browser model

The Assets area becomes a folder-aware browser rooted at the physical
`assets/` directory. At sufficient width it uses a Unity-like two-pane layout:
folder tree on the left and contents of the selected folder on the right. At
narrow widths the same data appears as one collapsible tree.

Folders are organizational filesystem entries. They do not receive `AssetId`
values and are not registered in `asset_manifest.json`. Registered files retain
their existing stable `AssetId` when moved or renamed.

The browser supports:

- create a folder in the selected folder;
- create a supported asset in the selected folder;
- nested folders;
- inline folder rename;
- single and multi-asset selection;
- drag selected assets onto a folder;
- drag a folder onto another folder;
- breadcrumb navigation and reveal-current-selection;
- recoverable folder trash;
- restoration of the selected folder after refresh and restart.

Hidden editor directories and project metadata outside `assets/` are never
presented as movable asset folders.

### 8.2 Move transaction

An asset or folder drop creates one batch move request. Before touching the
filesystem, the editor:

1. normalizes every source and destination relative to `assets/`;
2. rejects absolute paths, parent traversal, and symlink escapes;
3. rejects moving a folder into itself or one of its descendants;
4. detects filename and folder collisions;
5. collects every affected manifest entry and supported serialized reference;
6. presents a conflict or unsupported-reference summary when needed.

After successful preflight, the operation moves all files, updates manifest
paths while preserving IDs, persists affected documents, and refreshes the
browser. If any persistence step fails, filesystem changes and in-memory
manifests are rolled back. Rollback failure is treated as a high-severity
diagnostic containing the exact recovery paths.

Deletion of a non-empty folder uses project-local trash after showing the
number of files and registered assets. Empty-folder deletion may use the same
recoverable path for consistent behavior.

Initial scope covers drag and drop inside the Asset Browser. Importing files by
dropping them from Windows Explorer is a later extension and is not required to
complete this workstream.

### 8.3 Completion criteria

- A user can create `assets/gameplay/coins`, create or move five coin-related
  assets into it, and reopen the project with the same organization.
- Multi-file drag is one undoable or recoverable editor operation.
- Asset IDs remain stable and registered assets still load after folder moves.
- Collisions and invalid folder cycles change no files.

## 9. Workstream F: Rust authoring and build scheduling

Creating a Rust component, system, or resource writes the source, refreshes the
Game Code browser and source index, and selects the new file. It does not open
an external program unless the user explicitly chooses `Open Script`.

Editor Preferences stores an external-editor executable and argument template,
including path, line, and column placeholders. Known editor presets may be
offered, but a custom command remains supported.

Game-code edits set a dirty generation counter. Builds are coalesced:

- automatic build waits for a short quiet period;
- another edit resets that delay;
- at most one build runs at a time;
- edits during a build queue one newer generation, not one build per file;
- `Build Now` bypasses the delay;
- Play either builds the newest generation first or presents an explicit stale
  build choice according to project policy.

Build status shows dirty, queued, building, succeeded, and failed states with
the generation involved. Diagnostics link to the responsible source where
Cargo provides file and line information.

## 10. Workstream G: filesystem synchronization and continuity

### 10.1 File watcher

The editor watches project assets, authoring documents, game source, and the
manifest. Events are debounced and normalized. Internal editor writes carry a
suppression token so they do not cause duplicate imports or reload prompts.

External changes to an unmodified open document reload automatically. If the
document has unsaved editor changes, the editor offers `Keep Editor Version`,
`Reload Disk Version`, and a comparison summary. Rename and removal events
preserve stable selection when the move can be identified.

### 10.2 Project restoration

Preferences record the last successfully opened project and editor-local state:

- open document;
- selected entities and asset folder;
- expanded hierarchy and folder nodes;
- panel layout and Game/UI preview preset.

Normal startup reopens the last valid project. Holding a documented safe-start
modifier or using `Open Project Hub` bypasses restoration. A missing or invalid
project opens the hub with a diagnostic and repair choices rather than looping
on startup.

## 11. Workstream H: generated-project SDK and Cargo discoverability

Generated game projects become self-describing. Their README and editor UI show
the supported validation command. The editor provides `Open Project Terminal`
with the required SDK environment configured and `Copy Cargo Command` for use
in another terminal.

The SDK locator has one documented resolution order and reports the selected
engine version and source bundle. Generated Cargo configuration must be either
standard and visible to Cargo or invoked through a clearly documented wrapper;
it must not depend on an undisclosed editor-only path.

Package and editor builds use the same dependency-resolution contract. A clean
generated project must pass its documented check without manually discovering
the engine workspace layout.

## 12. Workstream I: deterministic input and replay testing

The editor provides engine-level virtual input, not operating-system key
injection. Input Debugger actions feed the same action-state boundary consumed
by Play systems.

The first deterministic replay format records:

- format and engine version;
- fixed simulation step;
- ordered frame or tick number;
- action press, release, axis, and pointer events;
- optional named checkpoints;
- expected final scene or game-flow state.

Game View offers Record, Stop, Replay, and Save Replay. Headless/editor test code
can load the same replay and assert authoring-visible results. Replay execution
uses a fixed step and rejects an incompatible format with a clear diagnostic.

The coin proving project supplies at least two replays:

- collect five coins, touch the enemy, and reach the clear result;
- collect fewer than five coins, touch the enemy, and reach the failure result.

Persisting a replay file is a serialized-format decision and requires an ADR
before implementation.

## 13. Delivery slices

### Slice 1: remove immediate authoring interruptions

- stop automatic OS opening after Rust script creation;
- add external editor preferences and explicit source opening;
- coalesce game-code builds;
- add project component source indexing;
- add built-in read-only source metadata and viewer skeleton.

### Slice 2: high-frequency scene and asset operations

- hierarchy multi-edit and batch duplicate;
- alignment, distribution, and repeat placement;
- folder-aware Asset Browser;
- create, rename, trash, and drag-to-folder transactions;
- manual refresh remains available as a recovery action.

### Slice 3: automatic continuity

- project file watcher;
- conflict prompts and internal-write suppression;
- last-project and last-document restoration;
- source index and Asset Browser refresh integration.

### Slice 4: UI production workflow

- UI multi-selection and repeated operations;
- responsive schema ADR and migration;
- preview presets, safe-area, and scaling diagnostics.

### Slice 5: reproducible validation

- generated-project SDK/Cargo workflow;
- deterministic virtual input queue;
- replay schema ADR, record/replay UI, and proving-project replays.

True property-level prefab overrides are scheduled only after their independent
ADR is accepted. Existing prefab discoverability and repeated instantiation can
ship in earlier slices.

## 14. Anticipated implementation surface

The exact file list is revalidated before each slice. The expected main changes
are:

- `crates/editor/src/ui/mod.rs`: Inspector actions, project browser UI, build
  controls, multi-edit entry points, and replay controls.
- `crates/editor/src/asset_browser.rs`: folder tree, selected folder, and
  multi-selection data model.
- `crates/editor/src/asset_management.rs`: folder operations and transactional
  batch moves.
- `crates/editor/src/preferences.rs`: external editor, restoration, and preview
  preferences.
- `crates/editor/src/ui_builder.rs`: multi-edit and responsive preview tools.
- `crates/editor/src/session.rs`: atomic multi-entity commands and undo grouping.
- `crates/editor/src/game_build.rs`: debounce and build-generation coalescing.
- `crates/editor/src/drag_drop.rs`: typed multi-asset and folder payloads.
- `crates/engine/src/components.rs`: built-in component source references.
- new `crates/editor/src/component_source_index.rs`: stable-ID project source
  index.
- new `crates/editor/src/component_source_viewer.rs`: read-only SDK source view.
- new filesystem synchronization module under `crates/editor/src/`.
- authoring UI schema and migration files if the responsive schema ADR is
  accepted.
- input/replay engine and editor modules after the replay ADR is accepted.
- `docs/AI_FRIENDLY_AUTHORING_SPEC.md`, relevant ADRs, user documentation, and
  `docs/editor_feature_reachability.json` as contracts become accepted.

This list is planning information, not authorization to modify every listed
file in one implementation. Each slice receives a focused design and approval
under the repository's implementation rules.

## 15. Required ADRs and specification updates

Before the relevant implementation depends on them, create or amend records
for:

1. component source discovery and the versioned read-only SDK source package;
2. batch asset/folder transaction boundaries and rollback behavior if they
   alter command semantics across crates;
3. responsive UI schema version and deterministic migration;
4. persisted input replay format and compatibility policy;
5. property-level prefab overrides, only if that work begins.

Editor-local preferences and a source index that do not affect persisted game
data should remain small, reversible editor changes and do not require an ABI
change.

## 16. Verification policy

Every slice includes unit tests for its non-GUI model and integration tests for
persistence and failure rollback. UI reachability is verified through the
normal editor surface, not by invoking private helpers alone.

Before Rust work is considered complete, run:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Risk-focused manual checks include:

- Windows with no `.rs` file association;
- source opening in a configured editor and missing-source recovery;
- a packaged editor with matching and missing SDK source bundles;
- multi-asset moves, collisions, rollback, and restart persistence;
- external file changes while documents are clean and dirty;
- ultrawide UI preview and Game View equivalence;
- clear and failure replays through the coin proving project;
- clean generated-project Cargo validation outside the engine workspace.

## 17. Overall completion criteria

This plan is complete only when a developer can use the editor to:

1. create and arrange repeated gameplay objects without manual JSON edits;
2. create folders and organize assets by drag and drop without losing IDs or
   references;
3. open project component code from the Inspector and inspect built-in source
   safely in read-only form;
4. create several Rust types without repeated application dialogs or redundant
   builds;
5. observe external changes and resume the last project after restart;
6. author UI that behaves consistently at standard and ultrawide resolutions;
7. validate a generated game with a documented Cargo/SDK workflow;
8. replay both success and failure paths deterministically through the same
   engine input boundary used by normal Play.

The `coin_collision_loop` project remains the small end-to-end proving project
for these workflows. Larger samples may supplement it, but cannot replace this
focused authoring and regression check.

## 18. Implementation record

The workstreams in this plan are implemented through the normal editor path.
The principal persisted decisions are ADR 0061 through ADR 0064. Scene and UI
multi-edit use command transactions; prefab instance actions are visible in the
Inspector and Assets workflow; responsive UI schema version 3 is shared by
preview and runtime; source navigation distinguishes project-owned and built-in
code; the Assets dock owns folder and batch moves; Rust builds use coalesced
source generations; the polling watcher and preferences restore project state;
generated games contain a Cargo README and standard `.cargo/config.toml`; and
Game View records and plays the same replay JSON consumed by engine tests.

The `coin_collision_loop` proving project stores clear and failure recordings
under `assets/replays/`. Property-level prefab overrides remain deliberately
outside this delivery, as specified in Workstream B.
