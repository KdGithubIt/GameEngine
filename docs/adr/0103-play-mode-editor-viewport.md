# ADR 0103: Play Mode Editor Viewport

Status: Accepted
Date: 2026-08-13

## Context

Edit Mode and Play Mode present two different worlds through two different
paths, and only one of them can be inspected freely.

Edit Mode draws the authoring scene into a throwaway preview world and points
an orbit/fly camera at it. `SceneView` does this by despawning every camera in
that world each frame and spawning its own from `EditorViewCamera`
(`crates/editor/src/scene_view.rs`).

Play Mode draws the live runtime world through
`RuntimePlayState::render_game_view`, which calls
`PreviewRenderer::render_to_view`. That function selects the camera by taking
the first `(Camera3D, Transform)` the query yields
(`PreviewRenderer::get_camera`), so the view is whatever camera the game
itself is driving. The editor central panel switches exclusively: while
`is_playing()` is true the Scene View is not drawn at all
(`crates/editor/src/ui/mod.rs`).

The consequence is that during Play an author can only see what the gameplay
camera sees. Debugging "where did the enemy go", "did the spawn fire behind
the wall", or "is the collider where I think it is" requires either stopping
Play, or adding a temporary camera to the game. `Pause` and single `Step`
already exist (`RuntimePlayState::set_paused`,
`request_single_step`), and the Runtime Debugger already lists live entities
with their component values, but none of that is spatial.

The Edit Mode technique cannot simply be reused. Despawning the running
game's cameras every frame would change gameplay state that scripts and
systems own, and the "first camera wins" rule makes the result depend on
spawn order.

Two shared resources also make a second view non-trivial:

- `ViewportSize` is a world resource. `camera_aspect_system` derives the
  active camera's aspect from it, `crate::game_host` lays out runtime UI
  against it, and screen-to-world spatial queries read it. A second view with
  a different size must not write it.
- Scene View picking uses `entity_pick_info`, built by
  `collect_entity_positions` from the **authoring** scene. During Play the
  authored transform is not where the entity is, so that path returns wrong
  answers for a moving world.

## Decision

1. `PreviewRenderer` gains an explicit-camera entry point:

   ```rust
   pub(crate) fn render_to_view_with_camera(
       &mut self,
       world: &mut World,
       camera: &Camera3D,
       camera_transform: &Transform,
       device: &wgpu::Device,
       queue: &wgpu::Queue,
       color_view: &wgpu::TextureView,
       depth_view: &wgpu::TextureView,
   ) -> Result<(), RenderFrameError>;
   ```

   `render_to_view` becomes a thin wrapper that resolves the world camera via
   `get_camera` and delegates. Rendering a world from a caller-supplied camera
   never mutates that world's entities.

2. The Play Mode editor viewport renders the **live runtime world** through
   that entry point, using the editor's own `EditorViewCamera`. No camera
   entity is spawned into, or removed from, the Play world.

3. The editor viewport computes its aspect from its own panel rectangle and
   **must not** write `ViewportSize`. `ViewportSize` continues to describe the
   Game View alone, so runtime UI layout, `camera_aspect_system`, and spatial
   queries keep observing exactly what the shipped game observes.

4. Play Mode presents Game View and Scene View as two selectable views of the
   central panel. Game View stays the default so starting Play is unchanged.

5. Selection in the Play Mode viewport picks against **runtime** transforms.
   Picking reads `GlobalTransform` from the Play world, and resolves the hit
   back to its authoring entity through the existing
   `AuthoringToRuntimeMap` so the Hierarchy and Inspector can follow, exactly
   as the Runtime Debugger's row selection already does. The authoring-scene
   `entity_pick_info` path stays Edit Mode only.

6. Edits made in the Inspector while Play is running remain edits to the
   authoring document, not to the running world. This ADR adds a way to look
   at the Play world, not a way to mutate it.

## Consequences

- Debugging a running game becomes spatial: fly the camera, pause, step, and
  look at where things actually are. Combined with the existing Pause/Step
  this replaces the "add a temporary debug camera to the game" workaround.
- The Play world is rendered twice in the frames where both views are
  visible. Because the two views are selectable rather than simultaneous, the
  common case renders once; when both are shown the GPU cost is the sum of
  the two viewports.
- `render_to_view`'s behavior is unchanged for every existing caller, so the
  Game View, the packaged `player`, and Edit Mode previews are unaffected.
- Two viewport sizes now exist during Play. Anything that needs "the size the
  game sees" must read `ViewportSize`; anything that needs "the size this
  panel is" must take it as an argument. This is a rule new render or UI code
  has to follow, and §3 is where it is stated.
- Runtime picking needs a runtime-side ray test. It cannot share the
  authoring implementation, so there will be two pick paths with a shared ray
  construction helper rather than one.

## Alternatives Considered

**Spawn an editor camera entity into the Play world, as Edit Mode does.**
Rejected: it mutates the world under test. Gameplay systems query cameras,
the "first camera wins" rule makes the result order-dependent, and a
`despawn`/`spawn` pair per frame changes entity IDs in a world whose IDs the
Runtime Debugger and replay recording both report.

**Add a `Camera3D` marker component that selects the render camera.**
Rejected for this decision: it is a larger, format-visible change to camera
semantics that gameplay code would also see, when the requirement is purely
"draw this world from this matrix, just once, for the editor".

**Run a second `App` that mirrors the Play world.** Rejected: keeping two
simulated worlds in sync is strictly harder than rendering one world twice,
and any divergence would make the debug view lie.

**Detach the Game View camera so the author can fly the gameplay camera
itself.** Rejected: it changes what the game does. The point is to observe a
running game without perturbing it.

## Compatibility and Migration

- No persisted data changes. Scenes, prefabs, the asset manifest, and
  packaged output are untouched.
- `PreviewRenderer::render_to_view` keeps its signature and behavior; the new
  function is additive. `PreviewRenderer` is `pub` in `crates/engine`, so the
  addition is a public API change to that crate and is covered by this ADR
  per `docs/AGENTS.md` §4.
- No authoring command, diagnostic code, or CLI/MCP surface changes.
- Which view Play Mode opens in is editor-local presentation state and
  belongs in `EditorPreferences`, not in project data.

## Verification

- Rendering the same world through `render_to_view` and through
  `render_to_view_with_camera` with that world's own camera produces the same
  camera uniform, so the wrapper is a pure refactor.
- Entering and leaving Play with the editor viewport selected leaves the Play
  world's entity set unchanged, including camera entities.
- `ViewportSize` observed by the Play world is unaffected by the editor
  viewport's panel size.
- Picking a moving entity in the Play viewport selects the authoring entity
  the runtime entity was spawned from.
