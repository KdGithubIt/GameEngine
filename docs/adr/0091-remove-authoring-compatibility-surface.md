# ADR 0091: Remove the Authoring Compatibility Surface

- Status: Accepted
- Date: 2026-07-27
- Completes: ADR 0083 §"Compatibility and Migration", ADR 0087
  §"Delete the legacy `skeleton` fields now"
- Amends: ADR 0066, ADR 0067, ADR 0082, ADR 0085

## Context

Several accepted ADRs shipped a new authoring shape beside the old one and
deferred deleting the old one. Each deferral was recorded with the same
reasoning: readability of existing content is what let the new shape ship
without a forced migration, and the compatibility schemas would be removed
"in a later change once in-tree and project content has been converted"
(ADR 0087), or "until a future explicit migration removes them" (ADR 0083).

That condition now holds. `examples/` contains exactly one project,
`coin_collision_loop`, and its scenes, prefabs, UI documents, and component
sources have been converted to the current shapes. Nothing in the tree reads
the compatibility paths any more.

Keeping them has a real cost. Every compatibility branch is a second correct
answer that validation, conversion, packaging, the Inspector, and the
reachability walks all have to keep agreeing about. Two of the paths were
mutually exclusive with the new one and existed only to produce a diagnostic
saying so. The `hidden_from_add` registry mechanism existed solely so that
seven registered components would not appear in Add Component.

This ADR does not introduce a new format. It states that the current formats
are the only formats.

## Decision

### 1. Legacy authoring components are unregistered and deleted

`engine.mesh`, `engine.material`, `engine.material_slots`,
`engine.skinned_mesh`, `engine.skeleton`, `engine.animator`, and
`engine.animation_graph_player` no longer exist as authoring components. Their
schemas, spawn functions, Inspector hints, and the conflict diagnostics that
policed mixing them with the unified components are gone.

A document that still names one gets the ordinary unknown-component treatment;
it is not special-cased, and no diagnostic claims it is a legacy shape.

The runtime ECS decomposition is unchanged. ADR 0083's separation of authoring
components from the runtime mesh handle, material, material-slot, and skinning
components stands; only the authoring-side aliases are removed.

`ComponentRegistry::addable_definitions` and
`ComponentRegistry::hide_from_add` are removed with them. Every registered
component is now offerable, so `definitions()` is the only list.

### 2. Deprecated fields are removed, not merely hidden

- `engine.animation_controller.skeleton` (the version 3 Skin reference) is
  gone. A controller drives the rig its own `engine.skinned_model` owns, and a
  controller without one reports `scene.component_dependency_missing` with no
  escape hatch.
- `engine.skinned_mesh_renderer` no longer reads a version 1 `skeleton`
  `EntityRef` as a rig override.
- Animation Graph states carry `motion_slot` only. The load-only `clip_id`
  binding, the `anim.ambiguous_motion_binding` diagnostic that existed to
  reject carrying both, and the in-memory slot-list reconstruction for graphs
  written before graph-owned slots are all removed. A compiled `AnimState` has
  exactly one motion key.
- Animation State playback has only `loop` and `once`. The legacy
  `InheritController` mode is removed; a missing `playback_mode` uses `loop`.

### 3. Old document versions are rejected, not upgraded

A document whose `schema_version` is lower than the current version is an
error, not an input to migrate:

| Document | Accepted version |
| --- | --- |
| `*.ui.json` | 3 |
| `*.material.json` | 2 |
| `asset_manifest.json` | 2 |

`UiDocument::schema_version` is a required field; it no longer defaults to 1.
Documents whose current version is 1 — scene, prefab, project,
project settings, graph, graph view, animation set, component sidecar — are
unaffected, because for them "the only supported version" and "the version a
missing field would default to" are the same number.

### 4. Project component identity comes only from the sidecar

`#[game_component(id = "...")]` is not accepted. The `.rs.meta.json` sidecar
is the only identity source, so the backfill that wrote a sidecar from the
attribute, the editor's attribute fallback with its
`editor.component_source.sidecar_missing_legacy` warning, and the
`editor.component_source.id_mismatch` diagnostic comparing the two are all
removed.

The one-time migration that moved `game/src/{components,resources,systems}`
into `assets/scripts/rust/*` is removed with its
`LegacyMigrationCollision` error, and Rhai sources are recognized only under
`assets/scripts/rhai`.

### 5. Migration tooling built for these paths is removed

**Project → Convert Legacy Rigs to Skinned Models** and its planner
(`engine::plan_skinned_model_migration`) are gone: they existed to convert
content off the shapes §1 and §2 delete. **Resync Model Parts** (ADR 0087 §5)
is unrelated and stays.

Also removed: the probe that loaded `iroha_game_module_v1`/`v2` entry points to
turn a stale module into an ABI-mismatch message rather than a missing-entry
one, and `engine-cli behavior-tree commit` as an alias for `apply`.

### 6. "Legacy" stops being used for things that are not old

`SystemOrigin::Legacy` becomes `SystemOrigin::Unnamed` and
`SystemDescriptor::legacy` becomes `SystemDescriptor::unnamed`. Registering a
system without a stable ID through `add_system` is an ordinary convenience used
throughout engine examples and tests; it is not compatibility with an older
format, and naming it "legacy" made it read like a path to delete. The
generated IDs move from `legacy.<hash>.<index>` to `unnamed.<hash>.<index>`,
which is safe because such descriptors are `is_persistent: false` and never
appear in saved system order.

## Consequences

- One shape per authoring concept. Validation, conversion, packaging, and the
  Inspector each have one answer to give.
- Content authored before the current shapes will not load. That is the
  intended trade: this is a reset to a single baseline, not a migration step.
  There is no in-engine path back, and the removed conversion command means
  unconverted content must be converted by an earlier build or reauthored.
- The registry loses 7 of 40 components. `ComponentRegistry::len()` is 33.
- Diagnostics that no longer exist:
  `scene.mesh_renderer_legacy_conflict`,
  `scene.animation_controller_legacy_conflict`,
  `anim.ambiguous_motion_binding`,
  `editor.component_source.sidecar_missing_legacy`,
  `editor.component_source.id_mismatch`,
  `editor.rig_migration_*`.
- A stale game module now reports a missing entry point rather than naming the
  ABI version it was built against. The rebuild action is the same; the message
  is less specific.

## Alternatives Considered

### Keep the compatibility readers and only hide them

Rejected. Hiding is what the previous state already was, and it is the state
this ADR exists to end: a hidden reader is still a second code path that every
consumer must keep agreeing with, and `hidden_from_add` was itself
infrastructure that existed only to support the hiding.

### Write a converter instead of rejecting old documents

Rejected for this change. A converter is the right tool when content exists
that someone needs to keep, which was exactly ADR 0087's reasoning for
shipping the conversion command. In-tree content is already converted, so a
converter would be code with no input.

### Renumber or rewrite the superseded ADRs

Rejected. An ADR records what was decided when it was decided. The
compatibility clauses in ADR 0083 and ADR 0087 were correct decisions that
this ADR completes, and rewriting them would erase why the two-shape period
existed. They are annotated with a pointer here instead.

## Compatibility and Migration

Breaking, deliberately, for content authored against the removed shapes. No
automatic migration is provided and none is planned.

Unchanged: `Vertex` and `SkinningVertexData` field layout, `StableId` and
`AssetId` derivation, the scene/prefab document schemas and their version
numbers, `ComponentTypeId` values of every surviving component, the runtime
ECS component set, and the ABI v3 game-module transfer format.

`docs/AI_FRIENDLY_AUTHORING_SPEC.md` is updated in the same change.
