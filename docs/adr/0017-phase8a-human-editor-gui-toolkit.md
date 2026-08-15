# ADR 0017: Phase 8-A Human Editor GUI Toolkit

Status: Accepted
Date: 2026-06-06

## Context

ADR 0016 defines the human visual editor as an adapter over the authoring
model. Semantic edits must flow through `GraphCommand`, presentation edits must
flow through `GraphViewCommand`, and transient UI state must not be persisted.

Phase 8-A needs a concrete GUI toolkit for the first prototype without turning
that toolkit into an authoring contract. The project also needs an initial
graph canvas strategy that can prove rendering, hit-testing, selection, drag,
pinning, property editing, and command emission before committing to a broader
node graph library.

## Decision

Phase 8-A will prototype the human visual editor with `egui` and `eframe`.

This decision applies only to the editor frontend implementation. It is not an
authoring model contract.

`engine-authoring`, `Graph`, `GraphView`, `GraphCommand`, and
`GraphViewCommand` must not depend on `egui`, `eframe`, or any GUI toolkit
type.

Add a future `crates/editor` crate for the Phase 8-A prototype. GUI toolkit
dependencies are isolated to that crate.

The first graph canvas will be a thin editor-owned canvas over egui painting,
hit-testing, and pointer input. Existing egui node graph crates, including
`egui-snarl`, `egui_node_graph`, and `egui-graph-edit`, may be spiked, but
Phase 8-A must not make a third-party node graph crate the owner of graph
storage, command semantics, validation, or layout policy.

An external node graph crate may be adopted later only if it satisfies all of
these conditions:

- It does not own the canonical graph model.
- It does not bypass or replace `GraphCommand` or `GraphViewCommand`.
- It can represent authoring `NodeId` and `EdgeId` values as the identity of
  displayed nodes and edges.
- It can preserve the `Graph` and `GraphView` document boundary from ADR 0016.

Semantic edits must produce `GraphCommand`. Presentation edits must produce
`GraphViewCommand`. Transient UI state remains editor-local and is not
serialized.

## Crate and Module Boundary

`crates/authoring` owns:

- `Graph`
- `GraphView`
- `GraphCommand`
- `GraphViewCommand`
- Validation
- Domain authoring services

`crates/authoring` must have no GUI toolkit dependency.

`crates/editor` owns:

- The `eframe` app entry point.
- egui UI panels.
- The graph canvas.
- The property inspector.
- Editor session orchestration.
- Transient UI state.

`crates/editor/src/session.rs` should define `EditorSession`. This module must
remain egui-free so it can be unit-tested without a GUI runtime.

`crates/editor/src/ui/` is egui/eframe-specific UI code.

`crates/editor/src/canvas/` is egui canvas code, including hit-test caches,
drag previews, temporary edge previews, and other transient canvas state.

`crates/editor/src/adapter.rs` maps UI gestures and inspector edits to
`GraphCommand` or `GraphViewCommand`.

## Phase 8-A Prototype Tasks

The Phase 8-A prototype should be implemented in this order:

1. Add `crates/editor` and create an `eframe` window shell.
2. Add `EditorSession` to hold a `Graph`, optional `GraphView`, and
   diagnostics.
3. Load the Behavior Tree example graph and draw node rectangles and edges on
   an egui canvas.
4. Apply node selection through `GraphViewCommand`.
5. Track node dragging as transient drag state and emit `GraphViewCommand` on
   release.
6. Apply pin and unpin through `GraphViewCommand`.
7. Apply property inspector node property edits through `GraphCommand`.
8. Implement minimal add node, delete node, and connect edge operations through
   `GraphCommand`.
9. Add an incremental layout action using the pinned-preserving deterministic
   merge from ADR 0016.
10. Add session-level unit tests proving UI-gesture-equivalent operations
    produce the expected command results.

## Consequences

- The first editor can move quickly while staying in Rust.
- The authoring model stays independent of egui, eframe, and node graph
  widget crates.
- The first canvas can be replaced or wrapped later without changing persisted
  graph documents.
- A future adoption of a node graph crate remains possible, but only as a UI
  implementation detail.
- Phase 8-A tests can focus on command emission and session behavior before
  GUI polish.

## Alternatives Considered

### Start with a third-party node graph crate

Rejected for Phase 8-A. Existing node graph crates may be useful, but adopting
one before validating the authoring boundary risks letting the widget model
drive graph identity, storage, or connection semantics.

### Use iced for Phase 8-A

Rejected for the first prototype. iced has a strong message/update/view model,
but the initial graph canvas needs direct painting, hit-testing, and pointer
gesture iteration with minimal adapter overhead.

### Use Slint for Phase 8-A

Rejected for the first prototype. Slint is useful for declarative native UI,
but the initial graph canvas and command-boundary proof is better served by a
Rust-immediate canvas.

### Use Tauri for Phase 8-A

Rejected for the first prototype. A web frontend would add a JavaScript or
TypeScript bridge and another serialization boundary before the Rust authoring
adapter is proven.

### Use Bevy egui for Phase 8-A

Rejected for the first prototype. Bevy integration is useful for game runtime
tools, but the Phase 8-A editor is an authoring frontend and should avoid
introducing a second ECS model into the editor boundary.

### Build a custom wgpu UI

Rejected. The project already has renderer code, but implementing a GUI toolkit
would distract from authoring command and graph editor behavior.

## Compatibility and Migration

No persisted schema changes are introduced.

No existing authoring, CLI, MCP, runtime, or graph APIs change. `egui` and
`eframe` dependencies must remain outside `crates/authoring`, `crates/engine`,
`crates/ecs`, `crates/cli`, and `crates/mcp`.

If Phase 8 later changes GUI toolkit, existing `Graph`, `GraphView`,
`GraphCommand`, and `GraphViewCommand` data remains compatible because the GUI
toolkit is not part of the authoring contract.
