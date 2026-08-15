# ADR 0046 — Declarative UI Document Model and egui Interpreter

## Status: Accepted

Date: 2026-07-11

## Context

Runtime UI (Phase 24) is code-only: games register Rust `UiSystem` callbacks
via `App::add_ui_system`, and neither the editor, scenes, nor Rhai scripts can
author or reference UI. The M1 milestone (an action-RPG-capable engine,
Phases 53-63) requires HUDs, menus, and dialog UI that are authorable as data,
editable by tools, and wired to scripts.

The design question is how to introduce a declarative UI system on top of an
engine whose entire UI stack (editor and runtime overlay) is egui, an
immediate-mode library, without rebuilding a React-style retained framework.

## Decision

### 1. UI is a serialized document, not code

A **UI document** (`*.ui.json`, owned by `crates/authoring`, module `ui`) is a
tree of typed nodes describing the interface. Authoring owns the format
(schema version, serde round-trip, validation with stable diagnostic codes),
mirroring `material_asset.rs` (ADR 0029) and `project_settings.rs` (ADR 0031).
The engine interprets documents at runtime; it never defines the format.

### 2. No VDOM, no reconciler — an interpreter over immediate mode

egui already re-renders the whole UI from state every frame, which is the
property a virtual DOM exists to recover in retained-mode systems. The engine
therefore adds a single built-in interpreter (`crates/engine/src/ui_document.rs`)
that walks the document tree each frame and emits egui widgets. There is no
diffing, no retained widget tree, and no component lifecycle. React-like
composition is expressed as data (documents can be split and included later),
not as a Rust framework.

### 3. Node model v1

Four node kinds cover HUD and menu foundations:

| Kind | Purpose | Key props |
| --- | --- | --- |
| `panel` | Anchored container | `anchor` (9-way), `offset_x/y`, `layout` (`vertical`/`horizontal`), `spacing`, optional `background` RGBA, `padding` |
| `text` | Label | `content` (string or binding), `size`, `color` RGBA |
| `button` | Clickable | `label` (string or binding), `event` (string event name) |
| `spacer` | Fixed gap | `size` |

Every node has a document-unique string `id` (used for egui `Id`s and
diagnostics). Images and includes are deferred to Phase 54+ because they need
asset-manifest integration.

### 4. Data bindings via a named value table

A string-valued prop may be written as `{ "$bind": "name" }`. At draw time the
interpreter reads `name` from a `UiBindings` world resource (an ordered map of
`String -> UiBindingValue` where values are strings, numbers, or booleans).
Game systems (and, from Phase 60, Rhai scripts) write into `UiBindings`; the
UI shows the current value on the next frame. Binding to arbitrary ECS
component paths is rejected for v1: a named table is host-language-agnostic,
trivially testable, and avoids coupling the UI format to component schemas.
A missing binding renders a placeholder (`--`) and reports a non-blocking
diagnostic once; it never panics.

### 5. Events are data

`button.event` is an event **name**, not a callback. When clicked, the
interpreter pushes the name into a `UiEvents` world resource (drained each
frame). Phase 54 routes these to Rhai `on_event` and Behavior Trees. Functions
are never serialized; this is the same boundary Rhai lifecycle hooks use
(ADR 0037).

### 6. CJK font loading is an App capability

egui's default fonts cannot render Japanese. `App::add_ui_font(name, bytes)`
registers a font that is installed into the egui context (both the standalone
runner and the editor Play host). Font bytes are supplied by the host/project;
the engine does not bundle a font.

### 7. Crate boundaries

- `authoring` gains the `ui` module (pure data + validation). No new deps.
- `engine` gains `ui_document.rs` (interpreter + `UiBindings`/`UiEvents`
  resources + document loading via the existing `engine -> authoring` edge).
  No new deps (egui is already an engine dependency from Phase 24).
- `renderer`, `ecs`, `cli`, `mcp` are untouched.

## Consequences

- HUDs and menus become project data: serializable, diffable, hot-reloadable
  (Phase 54), editable by tools and AI agents through the same authoring
  pipeline as scenes.
- Per-frame interpretation cost is O(nodes), matching egui's own model;
  documents at HUD/menu scale (tens to hundreds of nodes) are negligible.
- Layout expressiveness is bounded by v1 nodes (anchored stacks). Grids,
  images, nine-slice panels, and nested documents are additive follow-ups.
- The document format is a persisted format from day one: changes require a
  `schema_version` bump and migration tests (same rule as scenes).

## Alternatives Considered

- **VDOM + reconciler (React clone)** — rejected: solves a retained-mode
  problem the engine does not have; a multi-year Rust ecosystem effort
  (Xilem, Dioxus) with no payoff over interpreting data into egui.
- **Adopting a retained UI crate (Iced, Xilem)** — rejected: replaces the
  entire egui stack (editor + runtime overlay + Phase 24 API) and adds a
  parallel render path into wgpu.
- **Binding to ECS component paths in the document** — rejected for v1:
  couples the UI format to component schemas and entity identity; a named
  value table is simpler and sufficient for HUD/menu data flow.
- **Rust builder DSL only (no serialized format)** — rejected: fails the
  actual requirement (editor/script/AI-agent authorable UI); a typed builder
  can still be layered over the document type later.

## Compatibility and Migration

Additive only: one new authoring module, one new engine module, two new world
resources, one new `App` method. `*.ui.json` is a new persisted format with
`schema_version: 1`; no existing formats change. Phase 24's `add_ui_system`
API remains the escape hatch for fully custom Rust UI.
