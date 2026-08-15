# ADR 0027: Component Definition and Registry Boundary

Status: Accepted
Date: 2026-06-13

## Context

Adding one authorable component currently requires edits in at least four
places across three crates:

1. The component struct and its systems in `crates/engine`.
2. The component type constant and the spawn dispatch branch in
   `crates/engine/src/scene_bridge.rs`.
3. A `ComponentSchema` (fields, defaults, display metadata) registered either
   in `engine_authoring::ComponentSchemaRegistry::builtin()` or in the
   editor's `addable_component_registry()` in `crates/editor/src/ui/mod.rs`.
4. Editor inspector special cases such as `builtin_asset_choices()`, which
   hard-codes which component types are asset-backed and which asset IDs they
   may reference.

The cost of this scatter is already visible: `PlayerController`,
`OrbitCamera`, and `FollowCamera` were implemented in Phase 13 but were never
added to any schema registry, so they cannot be added or edited from the
editor. `Camera3D` and the light resources have no authorable component type
at all, which forces Play mode to inject a temporary default camera.

ADR 0025 staged inspector capabilities but did not decide where a unified
component description should live. The dependency rules constrain the
options: `engine-authoring` must not depend on `engine` (or any GUI crate),
while `engine` already depends on `engine-authoring`. CLI and MCP depend on
`engine-authoring` only and must keep working without linking the engine.

Reflection-based automatic component discovery remains an open decision and
is explicitly out of scope here.

## Decision

1. **`engine-authoring` keeps `ComponentSchemaRegistry` as a schema-only
   contract.** It continues to describe component shape, defaults, and
   display metadata without referencing runtime types. CLI and MCP keep
   consuming it unchanged.
2. **`crates/engine` gains a `ComponentRegistry` of `ComponentDefinition`
   entries.** A definition bundles everything the engine and its tools need
   to know about one authorable component:

   ```rust
   pub struct ComponentDefinition {
       /// Authoring schema: type id, fields, defaults, display metadata.
       pub schema: ComponentSchema,
       /// Spawns or applies the component on a runtime entity from its
       /// authoring value. Receives the bridge spawn context so
       /// asset-backed components can resolve `Value::AssetRef` through
       /// the manifest and emit asset diagnostics.
       pub spawn: SpawnFn,
       /// Editor presentation hint (for example: which asset kind an
       /// `asset_ref` value picker should offer).
       pub inspector: InspectorHint,
   }

   pub enum InspectorHint {
       /// Generic field editors derived from the schema.
       Default,
       /// The component value is a bare `asset_ref`; the editor offers a
       /// picker over built-in assets plus manifest entries of this kind.
       AssetRef { kind: AssetKind },
   }
   ```

   `SpawnFn` is a plain function pointer or boxed closure with a signature
   owned by `scene_bridge` (entity, authoring value, and a `SpawnContext`
   giving access to the world, asset server, manifest, and diagnostics
   sink). The exact signature is an implementation detail of `engine` and
   not part of this ADR's contract.
3. **One registration site.** `engine::components::builtin_registry()` (final
   module path decided in implementation) constructs the registry containing
   every built-in authorable component. Both consumers read from it:
   - `scene_bridge::spawn_from_authoring_scene` dispatches component values
     through the registry instead of an inline match over type-id constants.
   - The editor builds its Add Component picker, default values, and
     inspector behavior from `ComponentDefinition` entries, deleting the
     hand-written schemas and `builtin_asset_choices()` special cases.
4. **Dependency directions are unchanged.** `engine-authoring` never names
   engine types. The editor consumes the engine registry through its existing
   `engine` dependency. CLI and MCP continue to link `engine-authoring` only;
   they see schemas, not definitions.
5. **Registration stays explicit and hand-written.** No reflection, derive
   macro, or inventory-style auto-discovery is introduced by this ADR. The
   open decision on automatic discovery is unaffected; this ADR only reduces
   the number of registration sites from four to one.

## Consequences

- Adding an authorable component becomes: implement the component, write one
  `ComponentDefinition`, add it to `builtin_registry()`, and add tests. The
  schema, default value, spawn logic, and inspector behavior cannot drift
  apart because they live in one value.
- `engine` becomes the single source of truth for which components are
  authorable at runtime. The authoring crate's `builtin()` registry remains
  for components meaningful without the engine (for example
  `engine.transform` as pure data), and the engine registry supersedes it
  for editor use.
- The editor loses its private schema definitions; editor behavior for
  asset-backed components is driven by `InspectorHint` instead of matching
  on component type-id strings.
- `scene_bridge` keeps its public entry point and error/diagnostic behavior;
  the dispatch refactor is internal. Existing bridge tests must pass
  unchanged before any new component is added (migration gate).
- A second registry consumer (a future runtime player binary, headless
  scene tooling) gets spawn dispatch for free.

## Alternatives Considered

### Extend `ComponentSchemaRegistry` in `engine-authoring` with callbacks

Rejected. Spawn functions need `engine` types (`World`, `AssetServer`,
meshes), which the authoring crate must not name. Registering engine
callbacks into an authoring-owned registry at startup would invert the
dependency in spirit and create hidden global state that CLI/MCP would link
but never use.

### A new shared crate between authoring and engine

Rejected for now. A `component-registry` crate would still need engine types
for spawn functions, so it would sit at the same layer as `engine` and add a
crate boundary without removing any coupling. Revisit only if a non-engine
consumer needs definitions (not just schemas).

### Derive macro / reflection for automatic discovery

Rejected here. It changes how components are declared, not just where they
are registered, and the project has deliberately kept this an open decision.
The one-site registry is compatible with adding discovery later: a macro
would generate `ComponentDefinition` values feeding the same registry.

### Keep the status quo and document the four sites

Rejected. The Phase 13 components already fell through the gaps under the
documented process; the cost grows with every component planned for Phase 15
(camera, lights, controllers).

## Compatibility and Migration

- No serialized format changes. Scene files, `asset_manifest.json`,
  component type ids, and `Value::AssetRef` semantics are untouched.
- Registering new component types is additive for scene files; scenes
  containing unknown component types keep their current validation behavior.
- Migration order (Phase 15): first move the four existing components
  (`engine.transform`, `engine.player_marker`, `engine.mesh`,
  `engine.material`) into the registry with zero behavior change, gated on
  the existing editor and bridge test suites; then add new components.
- `engine_authoring::ComponentSchemaRegistry` keeps its public API; the
  editor stops calling its `builtin()` directly once the engine registry
  provides schemas, but CLI/MCP usage is unchanged.
