# ADR 0025: Editor Component Inspector Staging

Status: Accepted
Date: 2026-06-11

## Context

Phase 9-E adds Scene Hierarchy and Entity Inspector to the editor. The editor
can now select scene entities, edit component values, and add components via
`ComponentSchemaRegistry`.

Three related capabilities are intentionally not complete in Phase 9-E:

- The component registry is hand-written and starts with a small set of
  built-in components.
- Asset-backed components such as `engine.mesh` and `engine.material` require
  an `AssetId` chosen from project assets; they cannot be given a valid generic
  default value.
- Field-level component editing through `PropertyPath` / `SetProperty` is
  specified but not implemented. Phase 9-E replaces whole component values via
  `AuthoringCommand::SetComponentValue`.

Without a recorded staging decision, future work could accidentally add
fabricated asset IDs, path-shaped component values, or a partial `SetProperty`
contract in the wrong phase.

## Decision

Phase 9-E scope is limited to editor entity selection, entity creation and
deletion, whole-component value editing, and adding components whose default
value is self-contained. A self-contained default value is one that can be
constructed without choosing another entity, choosing an asset, reading a
manifest, or inspecting runtime resources.

The Phase 9-E built-in registry MUST include only components that satisfy that
rule. `engine.transform` and `engine.player_marker` are valid Phase 9-E
entries. Additional self-contained components may be added in later phases
when the component implementation exists.

The remaining capabilities are assigned to these phases:

1. **Phase 13: gameplay/controller component schemas.** Components introduced
   for the minimal playable vertical slice, such as `PlayerController`,
   `OrbitCamera`, or `FollowCamera`, may be added to
   `ComponentSchemaRegistry` in Phase 13 if they have deterministic defaults
   and do not require asset selection.
2. **Phase 14: asset-backed component assignment.** `engine.mesh`,
   `engine.material`, texture assignment, and any other component whose value
   is `Value::AssetRef` are implemented with the Phase 14 asset pipeline.
   The editor must choose an existing `AssetId` from `asset_manifest.json` or
   a reserved built-in asset ID. It MUST NOT fabricate an `AssetId` as a
   default and MUST NOT introduce path-shaped component values. ADR 0021
   remains the reference model.
3. **Phase 15: field-level property commands.** `PropertyPath` and
   `AuthoringCommand::SetProperty` are implemented in Phase 15, before prefab
   or multi-scene editing depends on precise nested-field diffs. Until Phase
   15, editor inspectors, CLI tools, and MCP tools must use
   `SetComponentValue` when changing scene component data.

## Consequences

- Phase 9-E remains a usable scene editor without inventing incomplete asset
  selection or property path semantics.
- Phase 13 can extend the component palette for gameplay components without
  waiting for the asset pipeline.
- Phase 14 owns asset-backed editor UX because it also owns manifest loading,
  asset validation, and asset diagnostics.
- Phase 15 owns field-level commands because they change the shared authoring
  command contract and need stable behavior before prefab and multi-scene
  workflows multiply edit surfaces.
- Scene files remain valid under the current `Value` model. No temporary
  `asset_path` or placeholder asset-reference format is introduced.

## Alternatives Considered

### Add `engine.mesh` and `engine.material` to Phase 9-E with null defaults

Rejected. Runtime bridge code expects meaningful `AssetRef` values for these
components. A null default would make newly added components invalid by
default and would push error handling into every save or play flow.

### Generate fresh `AssetId` defaults for asset-backed components

Rejected. An `AssetId` is only useful when it resolves through the project
manifest or a reserved built-in ID. Generating one in the inspector would
create broken references and contradict ADR 0021.

### Store asset paths directly in component values

Rejected. ADR 0021 explicitly rejects path-shaped references. Scene files must
continue to use `Value::AssetRef`.

### Implement `SetProperty` in Phase 9-E

Rejected. Phase 9-E does not need nested-field command semantics to provide a
working inspector. Whole-component replacement is already command-backed,
undoable through `AuthoringSession`, and sufficient for Phase 10 play testing.

### Defer all Add Component support until Phase 14

Rejected. Scene Hierarchy and Entity Inspector are much less useful without
adding simple components. Self-contained components are safe to add in Phase
9-E and provide the editing loop needed by Phase 10 and Phase 13.

## Compatibility and Migration

This ADR does not change serialized scene, graph, or project file formats.

Phase 9-E scenes edited through the inspector continue to serialize existing
`Value` data. Phase 14 will add asset manifest editing and asset-backed
component assignment without changing the `asset_ref` representation. Phase 15
will add `SetProperty` as an authoring command API extension; existing
`SetComponentValue` commands remain valid for whole-component replacement.

## Note: Phase Renumbering (2026-06-13)

The roadmap restructure of 2026-06-13 (see
`docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md`) maps this ADR's phase
assignments as follows. The staging intent is unchanged.

- "Phase 13 gameplay/controller component schemas" was not completed in
  Phase 13; those registrations now land in the new Phase 15 (registry,
  ADR 0027) and Phase 19 (camera controllers).
- "Phase 14 asset-backed component assignment" lands in the new Phase 16
  (asset / OBJ / render editor integration); the built-in-only picker
  shipped early during Phase 11-D remains the interim state.
- "Phase 15 field-level property commands" remains Phase 15 under the new
  numbering (`docs/phases/phase-15-component-registry.md`, task 15-D).
