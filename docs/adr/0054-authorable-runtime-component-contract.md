# ADR 0054: Authorable Runtime Component and Inspector Contract

Status: Accepted
Date: 2026-07-13

## Context

Editor Ready v1 requires reusable runtime behavior to be reachable from the
component registry and Inspector. The current registry covers transforms,
basic rendering, cameras, lights, particles, UI documents, and collision, but
animation, graph players, behavior trees, navigation agents, spatial audio,
and gameplay identity metadata remain runtime-only. Inspector metadata can
only distinguish three asset categories and otherwise falls back to generic
primitive controls.

Adding these paths introduces persisted component identifiers, asset and
entity references, enum controls, layer-mask meaning, and loader failure
semantics shared by authoring, engine, editor, package analysis, and project
Rust queries.

## Decision

1. Every reusable runtime component required by ER-4 has one stable
   `engine.*` component type ID, one schema, one registry definition, and one
   authoring-to-runtime spawn callback. Scene files store only authoring
   values; runtime handles, entity indices, and compiled graph pointers are
   never serialized.
2. Asset-backed component fields use `Value::AssetRef`. Their schemas and
   Inspector metadata declare a concrete asset category: mesh, material,
   texture, animation clip, animation graph, behavior tree, audio, NavMesh,
   UI document, or prefab. The editor filters manifest choices by category
   using deterministic path/source metadata; an unrecognized or mismatched
   file remains visible as a diagnostic, not as a silently accepted choice.
3. Entity relationships use `Value::EntityRef` and resolve through the
   conversion pass's complete authoring-to-runtime map. Plain strings and
   persisted runtime entities are invalid.
4. Schema-driven editor hints describe enum choices, project collision-layer
   masks, inclusive or exclusive numeric bounds, and conditional field
   visibility. Numeric bounds are also consumed by pre-Play validation so the
   Inspector and Problems panel report the same accepted range. Hints affect
   presentation only; the shared schema and spawn callback remain the final
   validation authority for editor, CLI, MCP, direct JSON edits, Editor Play,
   and packaging.
5. Asset-backed runtime components load or compile their referenced artifact
   during the normal conversion/build boundary and cache the resulting
   runtime asset. Missing files, unsupported formats, invalid graphs, and
   incompatible sub-assets produce stable, source-linked diagnostics. A
   component is not attached in a fabricated default state merely to make
   conversion appear successful.
6. `engine.runtime_metadata` stores game-visible name, tags, and team as
   explicit authored data. The bridge-owned `RuntimeEntityIdentity` continues
   to carry the stable authoring identity and editor name for every mapped
   entity; metadata augments it and does not replace stable identity.
7. Components with dependencies declare and validate them. In particular,
   animation graph players require an Animator and clip bindings, character
   motors require the kinematic controller path, and spatial audio emitters
   require a Transform. Missing dependencies produce actionable diagnostics
   during conversion or project validation rather than a silent runtime
   no-op.
8. Adding, removing, and editing these components continues through existing
   `AuthoringCommand` transactions. No component-specific editor mutation
   bypass is introduced.
9. `engine.animator` version 2 stores clip selection, playback speed, looping,
   autoplay, completion event, root-motion mode, and animation event rows.
   Each event row contains `time` and `name` plus an optional imported `clip`
   name. A named row fires only for that resolved clip; an omitted `clip`
   preserves the earlier Animator-wide event behavior. Runtime asset handles
   remain transient and are rebuilt from the registered glTF/GLB source.

## Stable Component IDs

The initial coverage uses these IDs:

- `engine.animator`
- `engine.animation_graph_player`
- `engine.behavior_tree_runner`
- `engine.nav_mesh_agent`
- `engine.audio_emitter`
- `engine.audio_listener`
- `engine.music_controller`
- `engine.runtime_metadata`

These identifiers must not be renamed after scene files use them. New fields
remain backward compatible through schema defaults or an explicit migration.

## Consequences

- Inspector controls can be generated without engine-specific mutation code.
- Scene conversion becomes the single loader/validation boundary for both
  Editor Play and the packaged player.
- ER-5 may extend animation-clip asset resolution with deterministic glTF
  sub-assets without changing the component IDs.
- ER-9 may extend the spatial audio mixer implementation while retaining the
  emitter/listener authoring shape accepted here.

## Verification

For every registered component, parameterized tests must cover schema default
creation, command-based add/edit/remove and undo, canonical save/reopen,
runtime conversion, malformed-value diagnostics, and execution of the owning
runtime system. Asset-backed components additionally require wrong-category,
missing-file, and package-dependency tests.
