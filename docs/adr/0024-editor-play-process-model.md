# ADR 0024: Editor Play Process Model and GPU Stack Unification

Status: Accepted (Track W resolved 2026-06-11)
Date: 2026-06-10

## Context

Phase 10 runs authored scenes from the editor. The implementation plan
assumed the editor shares eframe's wgpu device to render the game into an
egui texture. At the time this ADR was accepted, the lockfile contained two wgpu versions:
`engine-renderer` / `engine` use wgpu 22.1.0, while eframe 0.34.3 /
egui-wgpu 0.34.3 use wgpu 29.0.3. GPU objects cannot cross wgpu major
versions, so the planned approach is unimplementable as written.

Current implementation status (2026-06-11): Track W is complete. `engine`
and `engine-renderer` now use `wgpu = "29"`, matching eframe 0.34.3 /
egui-wgpu 0.34.3. Phase 10-C uses the in-process path with the editor's
`egui_wgpu::RenderState`; the out-of-process fallback remains documented
but is not active.

Two further constraints: `GpuState::render` (`crates/engine/src/app.rs`)
renders only into the window surface texture, and
`engine_ecs::App::update()` runs headless without any GPU.

Already-verified non-risks: winit (0.30.13), raw-window-handle (0.6.2), and
egui (0.34.3) are each a single version in `Cargo.lock`; wgpu is the only
forked dependency.

## Decision

Primary: **in-process play on a unified GPU stack.**

1. **Track W (prerequisite):** migrate `engine-renderer` and `engine` from
   wgpu 22 to wgpu 29, matching the editor's egui-wgpu version. ADR 0003
   ownership boundaries are unchanged. Verification gates: `hello_window`,
   `minimal_playable`, and `cargo test --workspace`.
2. **Target-agnostic render entry:** extract from `GpuState::render` a path
   equivalent to `render_world(world, color_target, depth_target, size)`
   used by both the window surface path and an editor offscreen texture
   path. The engine `render` module is crate-private, so this refactor is
   not a public API break.
3. **Viewport decoupling:** camera aspect derives from the render-target
   size, not the window size (`ViewportSize` wiring in
   `crates/engine/src/app.rs` / `camera.rs`).
4. **Editor play state:** `crates/editor` gains an `engine` dependency.
   Play mode owns `World` + offscreen target + egui `TextureId`; per frame:
   tick the runtime schedule, render into the offscreen texture on the
   eframe device, display via `egui::Image`. The engine's winit
   `EngineRunner` is not reused inside the editor.
5. **Staging:** Phase 10-A/B may ship as logic-only play (spawn, tick,
   inspect; no game view) before Track W completes, because the ECS tick is
   GPU-free. However, **merging the editor→engine dependency (Phase 10-B)
   is recommended only after Track W completes**: merging earlier links
   wgpu 22 and 29 into the editor simultaneously (it compiles, but build
   time and binary size inflate). Development may proceed on a branch.
6. **Panic policy:** the schedule tick runs inside `catch_unwind`; a panic
   aborts play, discards the runtime world, emits an
   `editor.runtime.panicked` diagnostic, and returns to Edit mode.
7. **Input routing:** while playing and the game view is focused, keyboard
   and mouse go to the runtime `Input` resources; Escape stops play instead
   of exiting the process. The standalone engine runner's Escape-exit
   behavior becomes configurable when this lands.

## Implementation Status

- Track W was completed on 2026-06-11 by migrating `engine` and
  `engine-renderer` to `wgpu = "29"`.
- Phase 10-C implemented the target-agnostic render path as
  `WorldRenderer::render_to_view` plus the public editor-facing
  `PreviewRenderer`.
- The editor Game View renders in-process into an offscreen `Rgba8Unorm`
  texture registered through `egui_wgpu::Renderer::register_native_texture`.
- Phase 10-D runtime diagnostics now use `editor.runtime.*` diagnostic
  codes, including `editor.runtime.no_scene`,
  `editor.runtime.scene_conversion_failed`,
  `editor.runtime.missing_asset`, `editor.runtime.no_camera`,
  `editor.runtime.tick_failed`, `editor.runtime.panicked`, and
  `editor.runtime.render_error`.

Fallback (documented retreat): an **out-of-process player binary**
(`engine::App` + scene path argument, diagnostics as JSON on stdout)
launched by the editor. Trigger: Track W blocked by an upstream
incompatibility or exceeding its timebox (recommended: two weeks). In that
mode the editor stays on wgpu 29 and the engine remains on wgpu 22.

## Migration Risk Inventory (wgpu 22 → 29)

The full wgpu API surface in this workspace is two renderer modules, four
engine modules, and two examples. Enumerated risks:

| # | Location | Risk | Expected effort |
|---|----------|------|-----------------|
| 1 | `renderer/src/context.rs` | `request_adapter` returns `Result` instead of `Option` in newer wgpu; `request_device` lost its second (trace) argument | Mechanical |
| 2 | `engine/src/render.rs:231,237` | `entry_point: "vs_main"` becomes `Option<&str>` (wgpu 24+) | Mechanical |
| 3 | `engine/src/app.rs:417` | `Instance::new` takes the descriptor by reference; `InstanceDescriptor` fields were restructured (`backend_options`, flags) | Mechanical |
| 4 | `engine/src/material.rs:95` | `create_texture_with_data` and texel-copy type renames (`ImageCopy*` → `TexelCopy*`) | Mechanical, verify |
| 5 | `shaders/triangle.wgsl`, `shaders/mesh.wgsl` | Seven majors of naga/WGSL validation tightening | Low (two simple shaders) |
| 6 | `Cargo.toml` features | Backend feature flags were reorganized in newer wgpu | Verify defaults on Windows |
| 7 | wgpu 26–29 specifics | Changes beyond current knowledge | Bounded: egui-wgpu 0.34.3 in this workspace is a living wgpu 29 reference; read the wgpu CHANGELOG |
| 8 | `renderer/src/surface.rs` | Surface configuration API is a historically stable area; verify `get_capabilities` / `SurfaceConfiguration` fields | Low |

Verified absent risks: no `device.poll` / `Maintain` usage anywhere; winit
and raw-window-handle do not fork (single versions shared with eframe).

## Consequences

- One GPU stack also unblocks the runtime UI phase, Phase 24 under the
  2026-06-13 renumbering, formerly Phase 16 (runtime egui requires matching
  wgpu); the migration is cheapest now while GPU code is small
  (`renderer` two modules, `render.rs` ~500 lines).
- The editor binary grows by linking `engine`.
- The runtime UI phase's pinned `egui = "0.29"` dependency listing is
  obsolete and is
  aligned with the workspace egui version.

## Alternatives Considered

- Out-of-process player as the primary model: better isolation, but worse
  iteration UX and permanent IPC plumbing; retained as the fallback.
- CPU readback bridge between wgpu 22 and 29: per-frame copies; rejected.
- Editor-side duplicate renderer on wgpu 29: violates ADR 0003's
  no-duplication rule; rejected.
- Downgrading eframe to a wgpu-22-era release: regresses five egui versions
  and the editor already uses 0.34 APIs; rejected.

## Compatibility and Migration

No serialized formats change. Engine-internal render refactor only; `ecs`
and `authoring` are untouched. The wgpu bump changes no public engine API
beyond types re-exported from wgpu itself.
