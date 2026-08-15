# Editor authoring tools

The main Engine Editor exposes **Authoring Tools** in the upper-right corner.
Open a project first, then open one of the project-scoped modeless windows:

- **Ability Designer** — Startup, Active, Recovery, and Cooldown timelines
- **Runtime Event Timeline** — live animation markers and accepted combat hits
- **UI Contract Designer** — typed bindings, UI events, initial focus, and directional focus links
- **Advanced Geometry Designer** — layered NavMeshes, floor links, static triangles, path tests, and raycasts

All four tools run inside the existing Engine Editor process and share its egui
context. Opening a tool does not start Cargo, build another binary, or launch a
sibling tool executable. Each window can remain open while the main scene, graph,
UI, or asset workflow continues underneath it.

Animation Motion Designer is intentionally not listed here because Animation
Graph Bool, Float, Trigger, and Blend1D integration is handled by the dedicated
animation workflow change rather than duplicated in this integration.

## Runtime Event Timeline

Open **Runtime Event Timeline**, then use **Launch Engine Editor with Live
Capture** in the embedded window. The viewer keeps the existing capture flow: it
starts an Editor process with the trace path configured and follows updates while
that Editor runs in Play mode.

The viewer window can also open an existing trace, choose another capture path,
refresh manually, filter event categories, search retained entries, or delete the
current trace.

## Ability data

Ability Designer persists deterministic phase timings. Project gameplay still
owns activation policy and the commands attached to phase changes, such as
hitboxes, movement, animation, audio, particles, and UI.

## UI contracts

`engine_authoring::UiAuthoringContract` provides JSON load/save helpers for
`.ui-contract.json`. `engine_authoring::UiFocusNavigator` validates the contract
against its `.ui.json` document, activates the authored initial focus, and moves
through explicit Up, Down, Left, and Right links by stable node ID.

A runtime UI adapter should map its keyboard or gamepad direction to
`UiFocusNavigator::move_focus` and request GUI focus for the returned node ID.
Typed `UiBindings` and `UiEvents` continue to supply runtime values and button
events through the existing UI document runtime.

## Advanced geometry

Advanced Geometry Designer's `.advanced-geometry.json` shape is available as
`engine::advanced_geometry::AdvancedGeometryDocument`.

```rust
use engine::advanced_geometry::AdvancedGeometryDocument;

let document = AdvancedGeometryDocument::load(path)?;
let geometry = document.build()?;

let path = geometry.nav_mesh().find_path(start, destination);
let ground = geometry.static_mesh("ground_probe");
```

Loading and building reject unsupported schemas, invalid layer settings, bad
links, duplicate mesh IDs, invalid triangle indices, non-finite vertices, and
degenerate triangles. The runtime therefore consumes the same validated data
that the authoring tool previews instead of requiring a second hand-written
conversion format.
