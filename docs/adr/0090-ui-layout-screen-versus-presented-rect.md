# ADR 0090: UI Layout Screen Separated from the Presented Rectangle

- Status: Accepted
- Date: 2026-07-26
- Depends on: ADR 0046 (declarative UI document), ADR 0073 (shared UI interpreter)
- Amends: ADR 0073 §"Why the canvas rectangle is the viewport"

## Context

A `UiDocument`'s authored `offset_x`, `offset_y`, `size`, `padding`, and
`spacing` values are logical units on the **target screen**. The document's
`scale_policy` decides how those units respond to a screen of a different size:
`ConstantPixels` (the default) keeps them as target-screen pixels, while
`ScaleWithViewport` scales them against `reference_resolution`.

Every host, however, passed the interpreter a single rectangle — the on-screen
rectangle it wanted to draw into — and the interpreter treated that rectangle as
both the anchor frame *and* the screen the policy resolves against:

```rust
let scale = document.viewport_scale([viewport_rect.width(), viewport_rect.height()], None);
```

For an editor surface those are two different things. The Scene View game frame,
a letterboxed Game View, and the zoomable UI Builder canvas all present the
shipped screen at a fraction of its size. Treating the presented rectangle as
the screen means a `ConstantPixels` HUD keeps its authored point size while the
frame shrinks, so the fraction of the screen it covers grows without bound.
Measured with `examples/coin_collision_loop/assets/ui/game.ui.json`
(1920x1080 reference, `constant_pixels`):

| Presented size | HUD root rect | Fraction of the screen |
| --- | --- | --- |
| 1920x1080 | 528x620 | 27% x 57% |
| 960x540 | 528x540 | 55% x 100% |
| 480x270 | 480x270 | 100% x 100% |

The last row is the reported bug: in a short, wide Scene View dock the HUD
covered the entire game frame, which is not what the shipped screen shows. ADR
0073 chose to hand the canvas rectangle to the interpreter as the viewport
precisely to avoid an `egui` layer transform; that avoided one problem and
created this one.

## Decision

Split the two concepts and keep them in one value, so a host cannot supply only
half of the information:

```rust
pub struct UiViewport {
    rect: Rect,          // where the screen is presented, in egui points
    screen_size: Vec2,   // how large that screen is, in target pixels
}

UiViewport::direct(rect)          // screen_size == rect.size(); display_scale == 1
UiViewport::scaled(rect, screen)  // display_scale == min(rect / screen)
```

Layout resolves against `screen_size`, and the result is scaled into `rect` by a
uniform `display_scale`. Because the interpreter already multiplies every
authored dimension by one `cx.scale`, the presentation shrink folds into that
same factor instead of a layer transform — ADR 0073's objection to layer
transforms and per-`Area` clip rectangles still stands:

```rust
pub fn ui_document_scale(document: &UiDocument, viewport: UiViewport) -> f32 {
    let screen = viewport.screen_size();
    document.viewport_scale([screen.x, screen.y], None) * viewport.display_scale()
}
```

Anchoring and clipping continue to use `rect`, since anchors are normalized to
the frame the screen is presented in. Reported node rectangles stay in screen
space, so editor hit testing is unchanged.

Widget metrics that come from `egui`'s style rather than from authored fields —
a `Button`'s label font and padding, scroll bars, indentation — are scaled by the
same factor at each top-level `Area` (`apply_scaled_style`). Without that, those
nodes would keep host point sizes and grow relative to their neighbors as the
screen shrinks, reintroducing the same class of mismatch inside a single
document. Documents with `scale_policy: constant_pixels` presented at 1:1 have
`scale == 1` and are bit-for-bit unaffected.

### Hosts and their target screens

| Host | Viewport |
| --- | --- |
| Player / standalone window | `direct(window rect)` — the window *is* the screen |
| Game View | `scaled(image rect, render target size)` — `1920x1080` renders at full resolution and is scaled into the panel |
| Scene View game frame | `scaled(frame, ViewAspect::target_resolution())` |
| UI Builder preview | `scaled(canvas, chosen preview resolution)` — the zoom slider is now a true zoom |

`ViewAspect` gains `target_resolution()`, because previewing a UI document needs
a screen *size*, not only a shape: `16:9` previews at 1920x1080, `4:3` at
1440x1080, `Free` has no target screen and previews at panel resolution.

### Alternatives considered

- **A project-level target resolution setting.** More honest than canonical
  per-preset resolutions: a `ConstantPixels` HUD really does cover a different
  fraction of a 1280x720 screen than of a 1920x1080 one, and only the project
  knows which one ships. Rejected for now because it changes the
  `ProjectSettings` serialized format; the per-preset resolution is reversible
  and can become the default value of such a setting later.
- **Keeping the presented rectangle as the screen and asking authors to use
  `ScaleWithViewport`.** This does not fix anything: with a policy that scales,
  the preview is self-consistent but still lays out against the wrong reference
  size, and `ConstantPixels` — the default, and the correct policy for a HUD
  that must stay pixel-crisp — would remain unpreviewable.
- **An `egui` layer transform per document.** Rejected again, for ADR 0073's
  reasons.

## Consequences

- The same document covers the same fraction of the screen in the UI Builder,
  the Scene View overlay, the Game View, and the shipped game. This is locked by
  `a_scaled_presentation_keeps_a_panel_at_the_same_fraction_of_the_screen`.
- Breaking change across `engine` and `editor` (both updated here):
  `UiDrawFrame::viewport_rect` becomes `UiDrawFrame::viewport: UiViewport`,
  `UiContext::viewport_rect` becomes `UiContext::viewport` with a
  `viewport_rect()` accessor, and `App::run_ui_systems` /
  `App::run_ui_systems_with_options` / `RuntimePlayState::run_ui_systems` take a
  `UiViewport`. No serialized format changes.
- Runtime behavior for a game that fills its own window is unchanged: its
  `display_scale` is one.
- The UI Builder zoom slider now scales the document instead of changing its
  layout. Authored values no longer read as canvas points at 50% zoom, which is
  the point, but any authoring habit built on the old behavior will feel
  different.
- Documents using `ScaleWithViewport` now resolve their responsive scale against
  the target screen rather than the preview canvas, so their preview changes too
  — in the same, corrected direction.
