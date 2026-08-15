# ADR 0073: Shared UI Interpreter for the Builder Preview

- Status: Accepted
- Date: 2026-07-21
- Depends on: ADR 0046 (declarative UI document)

## Context

The declarative UI runtime (`crates/engine/src/ui_document.rs`) is the single
authority for how a `UiDocument` is laid out: the root panel and each panel that
is a direct child of the root are promoted to independently anchored
`egui::Area`s positioned by `anchor_position(anchor, viewport, offset)`; every
other node is drawn nested inside its parent's layout. Scene View draws its UI
overlay through exactly this interpreter
(`App::run_ui_systems_with_options(.., UiDocumentDrawOptions::editor_preview())`),
so the overlay is what the shipped game shows.

The editor's UI Builder preview (`crates/editor/src/ui_builder.rs`,
`show_preview_node`) did **not** use the interpreter. It re-implemented the
layout for every node kind with its own egui calls, and — critically — it
ignored a `Panel`'s `anchor`, `offset_x`, and `offset_y`, stacking every node
from the canvas top-left instead. A `Text` inside a bottom-right–anchored panel
therefore appeared at the top-left in the Builder but at the bottom-right in
Scene View. Beyond anchoring, the two implementations could drift on any node
kind because they were separate code.

## Decision

Delete the Builder's duplicate layout and drive the preview through the runtime
interpreter, so there is exactly one UI layout implementation feeding both the
Builder preview and Scene View.

The interpreter previously read its inputs (bindings, events, diagnostics,
viewport) from ECS `World` resources, which the Builder does not have. We
introduce a `World`-free entry point:

- `UiDrawFrame<'a>` — explicit per-draw inputs: `bindings`, `events`,
  `diagnostics`, `viewport_rect`, and an optional asset `base_path` for
  resolving relative image sources.
- `draw_ui_document_with_frame(ctx, document, frame, options) -> UiDocumentDrawReport`
  — the shared core.

`draw_ui_document_with_options` (the `World` path used by the runtime and Scene
View) now builds a `UiDrawFrame` from world resources and delegates to the core,
so runtime behavior is unchanged.

The Builder preview:

- builds a `UiBindings` table from the transient Preview Bindings values,
- passes the on-screen canvas rectangle as the `viewport_rect` (identical to how
  Scene View passes its scene-image rectangle), so the responsive scale the
  interpreter computes is the scale for that on-screen size,
- selects and hover-highlights nodes using the `UiNodeDrawRecord` rectangles the
  interpreter returns, instead of a parallel hit-test tree.

### Why the canvas rectangle is the viewport (no layer transform)

> **Amended by ADR 0090.** The conclusion below — no layer transform — still
> holds, but handing the interpreter *only* the canvas rectangle made it treat
> that rectangle as the target screen, so a `constant_pixels` document kept its
> authored point size while the canvas shrank. ADR 0090 splits the presented
> rectangle from the layout screen (`UiViewport`) and folds the shrink into the
> interpreter's existing scale factor instead.

The preview zooms a chosen logical resolution down to fit the editor panel. We
considered laying the document out at the full logical resolution and applying
an `egui` layer transform to shrink the result. We rejected it: there is no
existing pan/zoom layer-transform precedent in this codebase, and layer
transforms interact awkwardly with the per-`Area` clip rectangles the
interpreter sets. Instead, the zoom is folded into the canvas size, and that
canvas rectangle is handed to the interpreter as the viewport — exactly what
Scene View already does with its scene-image rectangle. Because the Builder and
Scene View now call the same function with the same kind of input, they cannot
drift: at equal viewport sizes they render identically.

### base_path threading

`UiDrawFrame` carries an optional `base_path` so relative image `source` paths
resolve against the project root in the Builder (the runtime `World` path passes
`None`, preserving its current working-directory behavior). This threads through
`draw_top_level_panel` / `draw_node` / `draw_stack` to the image draw, matching
the module's existing explicit-argument idiom.

## Consequences

- One layout implementation; the reported anchor/offset mismatch is gone by
  construction, and future node-kind changes cannot desynchronize the two
  surfaces.
- A document whose root is not a `Panel` now previews as empty in the Builder,
  matching what Scene View and the shipped game already show. This is a
  deliberate correctness alignment, not a regression: the old Builder was the
  outlier that drew such content.
- Unset bindings preview as the runtime `--` placeholder (text) and the runtime
  numeric defaults, rather than the Builder's former `{name}` / `0.5`, again
  matching Scene View.
- `UiDrawFrame` and `draw_ui_document_with_frame` are new public engine surface,
  documented as the non-ECS drawing entry point.

Crate boundary: this is `engine` runtime API consumed by the `editor` adapter,
consistent with the existing direction (`editor` already depends on `engine`);
no new dependency edge is introduced.
