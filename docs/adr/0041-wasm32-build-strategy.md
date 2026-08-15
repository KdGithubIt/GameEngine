# ADR 0041 — wasm32 Build Strategy

**Status**: Accepted  
**Date**: 2026-06-14  
**Supersedes**: —  
**Relates to**: Phase 46 (WASM / Web Build Target)

---

## Context

Phase 45 completed the rendering pipeline. The next step is confirming that the engine
compiles for `wasm32-unknown-unknown` and can bootstrap in a browser. Desktop-only
dependencies (`gilrs`, `rodio`, `pollster`, `env_logger`) are already target-gated;
however several questions remain:

- Which wgpu and winit feature flags are needed on wasm32?
- How are asset I/O, navmesh persistence, and scene loading handled when `std::fs`
  operations silently fail on wasm32?
- Does `rhai` with the `sync` feature work on wasm32?
- What is the browser entry-point and canvas bootstrap mechanism?

---

## Decision

### 1. Target and Build Verification

Phase 46 MVP goal: `cargo check --target wasm32-unknown-unknown -p engine` passes.

Full `wasm-pack` / `trunk` packaging and a live browser demo are deferred beyond Phase 46.
Once the check passes, a developer can build with:
```
cargo build --target wasm32-unknown-unknown -p engine
```
and link the resulting `.wasm` with `wasm-bindgen` CLI to obtain JS glue code.

### 2. wgpu Backend

Add `wgpu = { version = "29", features = ["webgpu"] }` to the `wasm32` target-dependency
section so that the WebGPU backend is compiled in.  On wasm32 the Instance creation uses
the browser-provided GPU; `wgpu::Backends::all()` includes WebGPU when the feature is
present.

### 3. winit Web Platform

Add `winit = { version = "0.30", features = ["web"] }` to the `wasm32` target-dependency
section.  This enables `winit::platform::web::WindowExtWebSys`, already used in `app.rs`
to attach the canvas to the document body.

### 4. Asset I/O on wasm32

`AssetServer`, `SceneLoader`, and navmesh persistence (`save_navmesh` / `load_navmesh` /
`bake_navmesh`) delegate to `std::fs`.  On `wasm32-unknown-unknown`, filesystem operations
are absent; these functions are cfg-gated:

- Desktop (`#[cfg(not(target_arch = "wasm32"))]`): full implementation.
- wasm32 (`#[cfg(target_arch = "wasm32")]`): stub that returns
  `io::ErrorKind::Unsupported`.

HTTP-based asset loading is deferred to a future phase.

### 5. Scripting (rhai)

`rhai` with `features = ["sync"]` compiles on wasm32-unknown-unknown.  The `sync` feature
switches `Rc` → `Arc` for thread safety; Arc is available on wasm32.  No separate
feature gate is required.

### 6. Panic Hook and Logging

`console_error_panic_hook::set_once()` and `console_log::init_with_level(...)` are already
called in `App::run()` inside `#[cfg(target_arch = "wasm32")]` blocks.  No additional entry
point is needed for Phase 46.

### 7. GPU Initialisation on wasm32

The wasm32 branch of `EngineRunner::resumed()` attaches the canvas and calls
`wasm_bindgen_futures::spawn_local` with a stub future.  Full async GPU init on wasm32
(calling `GpuState::new` from within `spawn_local`) is deferred beyond Phase 46 MVP.

### 8. Clipboard Backend Boundary

`egui-winit` MUST be declared separately for native and wasm32 targets. Native builds
use its default `clipboard` feature and therefore the operating-system clipboard via
`arboard`. wasm32 builds disable the default features so the native-only `arboard`
backend cannot enter the Web dependency graph.

This target split is the platform boundary, not a claim that the browser clipboard is
synchronous or native-compatible. Browser clipboard integration MUST use Web APIs
(`ClipboardEvent` for user-initiated paste/cut/copy and `Navigator.clipboard` where an
asynchronous operation is required) when the deferred Web runtime is completed. A
permission denial or unavailable browser API MUST fall back to application-local
clipboard state rather than preventing the engine from starting.

---

## Consequences

**Positive**
- `cargo check --target wasm32-unknown-unknown -p engine` passes.
- Desktop path is unaffected; no existing tests break.
- Clear cfg boundaries prevent accidental desktop-only code from reaching wasm builds.
- Native clipboard dependencies cannot regress the wasm32 build graph.

**Negative / Deferred**
- `AssetServer` is a no-op stub on wasm32; wasm builds cannot load assets.
- Browser canvas renders nothing until GPU init is wired in a later phase.
- Browser system clipboard integration is deferred with the Web runtime; wasm32 does
  not pretend that the native synchronous clipboard backend is available.
- `wasm-pack` / `trunk` integration and HTTP asset loading are future work.

---

## Alternatives Considered

| Option | Reason rejected |
|---|---|
| wasm-pack as build tool (Phase 46) | Adds complexity; `cargo check` suffices to validate compile-time correctness |
| webgl feature instead of webgpu | WebGPU is the engine's stated backend per ADR 0040 |
| Removing rhai from wasm32 builds | Not necessary; rhai `sync` compiles fine |
| HTTP asset loading in Phase 46 | Adds significant scope; deferred |
