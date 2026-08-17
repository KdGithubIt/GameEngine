# ADR 0134: Portable Prefab Instance Source

Status: Accepted
Date: 2026-08-17
Relates to: ADR 0004, ADR 0021, ADR 0023, ADR 0058, ADR 0121, ADR 0132

## Context

Instantiating a prefab adds an editor-only `editor.prefab_instance` component
to the new root (ADR 0058 §4). Its `source` field recorded whatever path the
adapter happened to hold, and every adapter held a resolved absolute path:

- `PrefabAssetService::load` returned `LoadedPrefab { source, .. }`, where
  `source` came from `ProjectRoot::resolve_asset` and is therefore
  canonicalized (`\\?\C:\Users\...\assets\prefabs\hero.prefab.json` on
  Windows);
- `crates/cli/src/prefab_cli.rs` and `crates/mcp/src/prefab.rs` forwarded that
  value straight into `PrefabInstantiationRequest`;
- the Editor passed `project.assets_root().join(relative)` or
  `project.path().join(".engine/imported/<asset id>.prefab.json")`.

`PrefabAuthoringService` then wrote that string into the Scene, so
`engine-cli prefab instantiate <project> scenes/main.scene.json
prefabs/hero.prefab.json` persisted a machine-specific absolute path into
`*.scene.json`.

`*.scene.json` is canonical project data. ADR 0021 fixes the asset reference
model as project-relative manifest paths and rename-safe `asset_ref` values
precisely so canonical documents stay portable. An absolute path breaks as soon
as the project is moved, shared, or checked out elsewhere, produces churn in
version control, and leaks the authoring machine's directory layout. It also
makes CLI and MCP results non-comparable without stripping the project prefix,
which is how the defect surfaced while writing the ADR 0132 §5
adapter-equivalence tests.

## Decision

### 1. The persisted source is project-root-relative

`editor.prefab_instance.source` stores a path relative to the **project root**,
forward-slash separated, with no `..` component and no drive, UNC, or verbatim
prefix. A prefab under the assets root is recorded as
`assets/prefabs/hero.prefab.json`.

The project root is the base rather than the assets root because prefabs that
may be instantiated also live outside `assets/`: ADR 0075 generated model
prefabs are written to `.engine/imported/`. `asset_manifest.json` already
stores that location as a project-relative string in
`ImportSettings::generated_prefab`, so one base covers both cases without
introducing `..`.

An `AssetId` was considered instead (see Alternatives) and rejected for this
marker.

### 2. One shared type owns the invariant

`engine-authoring` owns `PrefabSourcePath`, a validated project-relative path,
and `PrefabInstantiationRequest::source` has that type. Adapters can no longer
pass a resolved filesystem path by mistake; they must convert one, and the
conversion is the single place that can fail.

`PrefabAssetService::load` returns both halves explicitly:
`LoadedPrefab::source` (portable, for the marker) and `LoadedPrefab::path`
(resolved, for opening the file). The absolute path exists only where a file is
actually read or written.

Marker construction and parsing are shared functions
(`prefab_instance_marker`, `prefab_instance_source`) rather than per-adapter
code, so Editor, CLI, and MCP cannot drift (ADR 0121, ADR 0132 §5).

### 3. Existing scenes keep opening; rewrites migrate them

Readers accept both forms. `PrefabInstanceSource` classifies a stored string as
`Portable` or `Legacy`:

- `Legacy` is any absolute value, including Windows drive-qualified, UNC, and
  verbatim paths, which are lexically relative on Unix and must not be
  misclassified as portable project data.
- `Legacy` resolves to itself, so a Scene authored before this ADR keeps
  working on the machine that authored it.
- A value that is neither is unresolvable and reads as no marker at all, the
  same outcome as a malformed component today.

Writers always emit `Portable`. Any operation that rewrites the marker
converts, so reverting or re-instantiating an instance migrates it. The Editor
Inspector labels a legacy source and names that recovery step.

No automatic scene-wide rewrite pass runs on load. Silently rewriting canonical
documents the author did not edit would produce unexplained version-control
diffs, and the legacy value is still functional on its original machine. A
legacy absolute path that points outside the current project cannot be made
portable; converting it fails with `prefab.source_outside_project` rather than
guessing.

The Scene `schema_version` does not change. The marker is an editor-only
component whose field type is unchanged (still a string); only the value
convention is narrowed, and both conventions parse.

### 4. Nested prefab traversal resolves against the project root

`prefab_dependencies` previously joined a marker source onto the containing
prefab's directory, which only ever produced a correct result because the
stored value was absolute. It now resolves through `PrefabInstanceSource`
against the project root, so it takes the project root as a parameter. Legacy
absolute markers keep resolving to themselves and traversal behavior for them
is unchanged.

## Consequences

- Scenes containing prefab instances are portable across machines and
  checkouts, and their diffs no longer depend on where the project is stored.
- CLI and MCP prefab results are directly comparable, which is what ADR 0132 §5
  adapter-equivalence tests require.
- `PrefabInstantiationRequest::new`, `LoadedPrefab`, and the Editor
  `prefab_workflow` entry points change signature. This is a breaking change
  across `engine-authoring`, `engine-assets`, `engine-cli`, `engine-mcp`, and
  `engine-editor`; all call sites are updated in the same change.
- Workflows that open the referenced prefab now need the project root. The
  Editor Inspector prefab section and Scene View placement therefore require an
  open project, which they already did in practice.
- Legacy scenes carry an absolute path until an author reverts or
  re-instantiates the instance. The stale value is visible in the Inspector, so
  the state is diagnosable rather than silent.

## Alternatives Considered

### Store an `AssetId` instead of a path

Rejected for this marker, though it is the ADR 0021 form for asset references
inside component values. Prefabs that can be instantiated are not all
registered: ADR 0075 generated model prefabs under `.engine/imported/` are
deliberately absent from `asset_manifest.json`, and the Editor instantiates
them directly. An `AssetId` marker would either exclude that path or force
manifest entries for import artifacts the asset browser must not show. A
follow-up ADR may add an optional `asset_id` field beside `source` once every
instantiable prefab is registered.

### Store an assets-root-relative path

Rejected. It cannot name `.engine/imported/` without a `..` component, which
`ProjectRoot` path validation rejects for good reason (ADR 0023).

### Keep the absolute path and strip it in adapters and tests

Rejected. The defect is in the persisted document, not in how it is reported.
Stripping at the boundary leaves version-controlled data machine-specific and
requires every future reader to know the workaround.

### Rewrite legacy markers automatically when a scene loads

Rejected as the default. It mutates canonical documents the author did not
edit, dirties scenes on open, and cannot succeed for a path outside the current
project. Migration on rewrite gives the same end state under author control.

## Compatibility and Migration

Scene `schema_version` is unchanged and both source forms load. Prefab, asset
manifest, project settings, and graph formats are untouched. Stable identifier
formats are unchanged.

The MCP `prefab.preview` / `prefab.instantiate` input schema is unchanged: its
`source` argument is still the asset-root-relative path used to locate the
prefab. Only the value persisted on the instantiated root changes.

Existing scenes require no action to keep working. To migrate one, revert or
re-instantiate the affected prefab instance and save the scene.
