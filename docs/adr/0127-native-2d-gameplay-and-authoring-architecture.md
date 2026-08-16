# ADR 0127: Native 2D Gameplay and Authoring Architecture

Status: Proposed
Date: 2026-08-16
Builds on: ADR 0053, ADR 0054, ADR 0072, ADR 0107, ADR 0113, ADR 0114, ADR 0118, ADR 0121
Relates to: ADR 0125, ADR 0126

## Context

GameEngine is currently 3D-first. It already has the reusable foundations a 2D
workflow should keep: ECS, scenes/prefabs, input actions, save/audio/UI, asset
management, one Transform hierarchy, Editor transactions, persistent Scene View
preview, mesh/material rendering, `Camera3D`, and a Rapier 3D physics backend.

A textured 3D quad is not a complete 2D workflow. Practical 2D games need stable
sprite regions and pivots, explicit draw order, orthographic/pixel-aware cameras,
sprite animation, tile authoring, 2D physics/queries, one-way platforms, and
Editor tools built for those concepts. The existing Editor UX plan also left the
sprite component as future work rather than defining a full 2D architecture.

The engine must not solve this by creating a second 2D engine beside the 3D
engine, nor by hiding all 2D semantics inside 3D meshes and constrained 3D
physics. This ADR defines native 2D as a first-class capability distributed
across the existing domain owners, with an authoring-first Editor workflow and
stable extension points for later lighting, parallax, VFX, and Timeline work.

## Decision

### 1. 2D uses existing domain ownership; there is no `engine-2d` mega-crate

Ownership follows ADR 0113:

- `engine-authoring`: Sprite Atlas, Sprite Animation, Tile Set, Tile Map,
  project 2D settings, schemas, commands, validation, and GUI-free services.
- `engine-assets`: backend-neutral compiled/runtime sprite-region and tile data
  consumed by several domains.
- `engine-animation`: sprite animation playback/time evaluation.
- `engine-physics`: 2D bodies, colliders, joints, queries, controller state,
  events, and solver adapters.
- `engine-render-runtime`: `Camera2D`, sprite batching/sorting, tile rendering,
  and later 2D lighting/presentation.
- `engine-scene`: authoring-to-runtime conversion for 2D scene components.
- top-level `engine`: cross-domain composition such as a Tile Map that creates
  both render chunks and physics collision chunks.
- Editor: presentation and interaction only, never alternate runtime semantics.

No lower crate depends upward merely because a feature is called 2D. The
`engine` facade continues to re-export supported public types.

### 2. One Transform hierarchy remains the only spatial authority

The existing `Transform`, `GlobalTransform`, `Parent`, and `Children` hierarchy
is reused. No parallel `Transform2D` or second hierarchy is introduced.

Native 2D convention is:

- gameplay plane: world XY;
- +X right, +Y up;
- sprites lie in local XY facing +Z;
- 2D rotation is rotation around Z;
- ordinary 2D scale uses X/Y; and
- logical draw order is not encoded by mutating Transform Z.

The Editor's 2D mode exposes the common X/Y, Z-rotation, and X/Y-scale controls
with planar gizmos, but still edits the normal Transform through
`AuthoringCommand`.

2D physics projects the resolved pose to solver X/Y plus Z rotation only when
the hierarchy is representable as planar 2D. Non-planar rotation, non-finite
values, or another unsupported effective transform shape produces a structured
diagnostic instead of silent projection.

### 3. Project Settings owns 2D defaults and stable sorting layers

Project Settings gains a typed 2D section with at least:

- default pixels per world unit;
- default sprite filtering policy;
- 2D gravity;
- default Camera2D/pixel-preview policy; and
- ordered named sorting layers with stable `SortingLayerId` values.

Sprites and Tile Maps persist the stable sorting-layer ID, not its display name.
Renaming a layer therefore keeps content bound to the same layer. Sorting layers
are distinct from collision layers and render/culling masks.

Editor, CLI, and MCP use the same Project Settings service/transaction contract
per ADR 0121.

### 4. Sprite regions use stable IDs and derived GPU packing

Introduce versioned `*.spriteatlas.json` `SpriteAtlasDocument` assets. Each
sprite region owns:

- stable `SpriteId`;
- display name;
- source texture reference and integer pixel rectangle;
- normalized pivot;
- pixels-per-unit policy; and
- sampling/extrusion metadata needed to avoid atlas bleeding.

A single image uses the same model with one full-image region. Sprite sheets use
slicing tools to create multiple regions. Renaming, reordering, or derived atlas
repacking does not change `SpriteId`.

`SpriteRef` identifies the atlas asset plus stable `SpriteId`. Scene/animation
data never persists GPU handles, UV floats, packed-page indices, or filesystem
paths as logical sprite identity. Runtime atlas packing is derived cache/build
work and can change without rewriting authored references.

### 5. `engine.sprite_renderer_2d` has a dedicated batched render path

A stable SpriteRenderer2D authoring component contains a `SpriteRef`, tint,
flip flags, sorting layer, `order_in_layer`, visibility, and only the material/
blend overrides that are meaningful to the supported 2D renderer.

Runtime rendering generates shared quad/instance data from sprite regions rather
than creating one Mesh asset per sprite. Compatible sprites batch by texture
page/material/sampler/render state while preserving deterministic authored draw
order.

Sorting layer + `order_in_layer` define logical order. Equal authored order has
a deterministic runtime tie-break; authors do not need presentation-only Z magic
numbers.

The default path is unlit and alpha-aware and follows ADR 0118 color/texture
semantics. Lit sprites/normal maps may extend this later without replacing
SpriteId, sorting, or Transform contracts. 3D world-space billboards remain a
separate rendering use case.

### 6. `Camera2D` is orthographic and shares one active-camera arbitration rule

`engine-render-runtime` gains `Camera2D` with enabled/priority,
orthographic size/zoom, near/far range, pixel-perfect mode, reference
pixels-per-unit/resolution, and viewport fitting/crop policy.

Camera2D uses the normal Transform and views the XY gameplay plane. Invalid
non-planar tilt is rejected or diagnosed when it violates 2D camera semantics.

ADR 0107's active-camera rule is generalized rather than copied. `Camera3D` and
`Camera2D` contribute to one common enabled/priority ordering, and highest-
priority ties across camera types receive an authoring diagnostic rather than
being resolved by query order as persisted intent.

Pixel-perfect mode adjusts projection/camera sampling without rewriting authored
entity transforms. The Editor warns when arbitrary rotation/scale means exact
pixel-grid alignment cannot be guaranteed.

### 7. Sprite animation is a typed asset with independent runtime state

Introduce versioned `*.spriteanim.json` Sprite Animation assets. Frames reference
`SpriteRef` values and use exact non-negative integer durations. The asset also
owns looping/default playback policy and optional named frame events.

Immutable clip data is shared; current time/frame is per entity. Frame evaluation
belongs to `engine-animation`; rendering consumes only the current SpriteRef.

A stable `engine.sprite_animator_2d` component exposes autoplay, speed, looping
override, and initial clip state. Project gameplay receives additive deferred
commands/copied views for play, pause, stop, clip selection, and state queries.

The first version does not force sprite clips into the skeletal Animation Graph.
A later 2D state-machine/controller may select Sprite Animation assets through a
typed domain without changing the base clip format.

### 8. Tile Sets use stable TileIds; Tile Maps are sparse and chunked

Introduce:

- `*.tileset.json`: stable `TileId` -> SpriteRef plus collision shapes, tags,
  custom values, and extension metadata for future terrain/autotile rules.
- `*.tilemap.json`: stable layers plus sparse fixed-size chunks whose cells
  reference TileIds.

Tile layers have stable IDs, display names, enabled/locked state, and the same
sorting-layer/order semantics as sprites. Cells never persist UV or atlas array
indices.

Chunking is the runtime and authoring unit. Render batches, collision extraction,
preview invalidation, and future streaming update affected chunks rather than
creating one ECS entity, draw call, or collider per tile.

Backend-neutral compiled tile data lives below render/physics ownership so
render-runtime and physics can consume it independently. Cross-domain assembly
stays in the higher composition layer.

### 9. Tile painting is transactional by gesture and incremental by chunk

The Editor provides Tile Palette plus paint, erase, rectangle, line, fill,
eyedropper, selection/stamp, layer, grid, collision-overlay, and chunk-overlay
tools.

One pointer stroke is one semantic TileMap transaction/undo entry. Preview during
the gesture is transient; cancel restores the exact previous cells. Commands
operate on bounded regions/chunks instead of issuing hundreds of unrelated
per-cell document transactions.

Changing one chunk invalidates only affected render/collision chunks where
practical. Large fill/build work is bounded and cancellable; heavy work is kept
off the immediate UI edit path in the spirit of ADR 0104, while cheap validation
remains immediate.

### 10. 2D physics has engine-owned contracts and an isolated dedicated solver

`engine-physics` gains native 2D contracts rather than constraining the 3D
solver to Z = 0. The initial surface includes:

- `engine.rigid_body_2d`: fixed, dynamic, and kinematic modes;
- `engine.collider_2d`: box, circle, capsule, and supported polygon shapes,
  sensor state, material values, collision layers/masks;
- ray/shape casts, overlap and contact queries;
- fixed-step collision/trigger transitions;
- continuous-collision policy for fast bodies; and
- fixed, distance, revolute, and prismatic joint contracts where supported.

The concrete backend may be Rapier 2D, but Rapier types never enter persisted
scene data, scripting ABI, or neutral public contracts. Its heavy dependency is
isolated behind an owner-local feature according to ADR 0114, so contract-only
or 3D-only consumers do not compile the 2D solver unnecessarily.

The 2D and 3D collision worlds are explicit and independent. They never collide
through implicit projection. A future cross-dimensional proxy requires its own
explicit bridge design.

### 11. Platformer-critical movement is included in the usable baseline

A kinematic CharacterController2D/motor path is built on the 2D query/solver
contract. It exposes collider/skin configuration, grounded state/normal, slope
limit, ground snap, wall/ceiling classification, deterministic fixed-step
movement, and one-way-platform/drop-through support.

One-way behavior is a collider/surface policy shared by scene-authored and
project-code characters, not special-case position checks embedded in one game.

Moving-platform gameplay, ladders, wall jumps, coyote time, jump buffering, and
acceleration curves remain project gameplay policy. The engine supplies the
collision/controller primitives those mechanics need.

### 12. The existing Editor gains 2D mode and dedicated 2D document tools

Scene View gains an explicit 2D mode with orthographic XY viewing,
cursor-centered pan/zoom, planar gizmos, world/pixel grids, sorting inspection,
sprite bounds/pivots, Camera2D framing, collider/joint/platform gizmos, and Tile
Map painting. 2D/3D view mode is transient presentation state and does not
rewrite scene content.

Dragging a sprite into 2D Scene View creates Transform + SpriteRenderer2D through
the shared Scene authoring service. Dedicated Sprite Atlas and Tile Set/Map
workspaces provide slicing, pivot editing, palette/layer tools, collision-shape
editing, and previews, but all persisted edits remain GUI-free service commands
with transaction/undo/validation semantics shared by Editor, CLI, and MCP.

Preview uses the same runtime SpriteRef, camera, tile, animation, and physics
interpretation as Play. Scene View reuses ADR 0072 persistent preview and
invalidates only affected entities/chunks where practical. No Editor-only sprite
or physics interpreter is accepted.

Screen-space UI remains a separate domain; world sprites/tile maps are not
implemented through the UI document runtime.

### 13. Lighting/parallax/VFX/Timeline are additive extensions

The first production slice targets correct unlit sprites, Tile Maps, animation,
and 2D physics. The design reserves typed extensions for Light2D/lit sprites,
normal maps, occluders, parallax, sprite masks, and 2D VFX render outputs.

Those extensions must reuse SpriteRef, Transform, sorting, Camera2D, and asset
contracts instead of adding unrelated paths. ADR 0125 VFX and ADR 0126 Timeline
may later add typed 2D adapters, but they do not become the owners of sprite or
2D physics state.

### 14. A first-party 2D proving project is part of the definition of done

Implementation must include an editor-openable 2D project/template created by
the standard Launcher/Project workflow. It exercises persisted authoring data
for:

- image -> Sprite Atlas -> SpriteRenderer2D;
- Camera2D and pixel-aware Game View;
- input actions and sprite animation;
- Tile Set/Tile Map editing;
- Rigidbody2D/Collider2D/CharacterController2D;
- one-way platforms;
- existing HUD, audio, and save systems; and
- packaged-player execution from the same project.

A Rust-only example does not prove native 2D authoring usability.

### 15. Implementation proceeds in playable dependency-safe slices

1. **Shared contracts:** project 2D settings/sorting layers, Sprite Atlas/
   SpriteRef, SpriteRenderer2D, Camera2D, generalized camera arbitration.
2. **Basic sprite workflow:** GPU batching, import/slicing, 2D Scene View,
   pivot/sorting tools, drag/drop, minimal editor-playable sprite project.
3. **Physics:** 2D contracts/backend, bodies/colliders/queries/events/joints,
   controller/one-way platform, debug gizmos, gameplay commands/views.
4. **Animation:** Sprite Animation service/player, SpriteAnimator2D, events,
   preview and gameplay control.
5. **Tiles:** Tile Set/Map documents, chunk renderer/collision extraction,
   paint/palette/layer/collider tools with gesture-level undo.
6. **Production polish:** proving template/level, performance verification,
   then optional Light2D/parallax/VFX/Timeline integration.

Each slice introduces persisted/public contracts only together with the owner
and runtime path needed to make them meaningful.

## Verification

The accepted implementation must prove at least:

- 2D and 3D share one Transform/Hierarchy authority;
- SpriteId survives rename/reorder/repack and SpriteRef retains logical identity;
- sprite ordering is deterministic without Z-position edits and batching does
  not change that order;
- Camera2D projection/pixel policy is deterministic, with visual warning for an
  intentionally incompatible pixel-perfect fixture;
- Camera2D and Camera3D share one deterministic active-camera rule;
- entities sharing one Sprite Animation asset keep independent playback state;
- 2D bodies never interact with the 3D world implicitly;
- non-planar 2D-physics hierarchy state produces a structured diagnostic;
- one-way platform approach/pass/drop-through follows one documented policy;
- 2D queries/events preserve shared collision-layer and fixed-step semantics;
- one Tile Map stroke is one undo entry and cancel restores exact prior cells;
- a changed Tile Map chunk does not rebuild unrelated chunks;
- TileId remains stable across tile rename/palette reorder;
- Editor/CLI/MCP produce equivalent persisted validation/results;
- Editor preview, Play, and packaged Player resolve the same authored 2D data;
- the first-party 2D proving project opens, plays, and packages through the
  normal authoring-first path; and
- heavy 2D solver/GPU dependencies do not leak into ADR 0114 contract-only
  dependency graphs.

Sprite rendering, Camera2D pixel framing, 2D Scene View, pivots, Tile Map tools,
physics gizmos, sprite animation preview, and the proving project require Visual
Validation when implemented. ChatGPT must inspect the screenshots themselves.

Performance verification records sprite count/draw batches, Tile Map visible
chunk/rebuild counts, large-map edit latency, and representative 2D physics step
cost. Editor interaction must remain responsive while derived work is pending.

## Consequences

GameEngine gains a native production path for platformers, top-down action,
puzzle, pixel-art, tile-based RPG, and similar 2D games while preserving the
same project, scene, prefab, input, UI, audio, save, scripting, and packaging
foundations as 3D projects.

The design adds several explicit data/component contracts because 2D has real
semantics a generic MeshRenderer/3D collider workflow cannot express cleanly.
Those contracts remain in the existing domain owners rather than creating a
second monolithic engine.

Stable SpriteId/TileId/sorting identity and the shared Transform hierarchy make
future lighting, parallax, streaming Tile Maps, VFX, and Timeline integration
additive instead of replacement work.

## Alternatives Considered

### Treat 2D as textured MeshRenderer quads

Rejected. It does not provide sprite identity/pivots, batching/order semantics,
pixel-aware cameras, Tile Map authoring, or a practical 2D Editor workflow.

### Add one `engine-2d` crate containing every 2D feature

Rejected. Rendering, physics, animation, assets, and scene conversion have
separate existing owners; a mega-crate would recreate the dependency problem
ADR 0113 removed.

### Add a separate Transform2D hierarchy

Rejected. Scenes/prefabs would have two spatial truths and hierarchy, selection,
attachment, Timeline, and mixed 2D/3D behavior would require synchronization.

### Constrain Rapier 3D to a plane

Rejected. It retains 3D solver cost/semantics and complicates ordinary 2D
queries, joints, and platform behavior. A dedicated neutral 2D contract and
isolated solver backend is clearer.

### Use Transform Z as sprite sorting

Rejected. Visual order is authoring intent. Stable named sorting layers plus
explicit order keep presentation semantics inspectable without magic positions.

### Use one ECS entity per tile

Rejected. Large maps would create unnecessary entity, draw, and collision
overhead. Chunked tile data is the correct edit/render/collision unit; special
interactive objects remain ordinary entities placed over the map.

### Build a separate 2D Editor

Rejected. It would duplicate project lifecycle, Scene/Prefab operations,
Inspector/undo, Play, validation, and AI parity. The existing Editor gains 2D
mode and specialized document workspaces instead.

## Compatibility and Migration

Sprite Atlas, Sprite Animation, Tile Set, Tile Map, SpriteRenderer2D,
SpriteAnimator2D, Camera2D, Rigidbody2D, Collider2D, joints, and the 2D controller
are additive current-format capabilities.

Project Settings advances when 2D defaults/sorting layers land. Per ADR 0115,
in-repository projects/fixtures move to that canonical current format rather
than gaining compatibility-only readers for older engine revisions.

Existing 3D component IDs, 3D physics semantics, mesh/material assets, Camera3D,
UI, input, audio, save, and package layout are not redefined. Generalized active
camera selection preserves Camera3D enabled/priority meaning.

No runtime entity, GPU/Rapier handle, packed UV/page index, Tile Map buffer
offset, or Editor selection state is persisted. Stable authoring IDs and shared
command/transaction semantics remain authoritative.
