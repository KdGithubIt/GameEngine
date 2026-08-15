# Phase 40 — AI Agent Bridge

## Goal

Expose the in-engine observation and virtual-input primitives — Phase 10-E
`FrameCapture` and Phase 12-F `VirtualInput` — as CLI and MCP tools so
external AI agents can observe and control the running game without OS-level
input automation.

## Why

The engine-side building blocks were implemented early (Phases 10-E and 12-F,
2026-06-11) with the boundary defined in ADR 0026.  What remains is the
CLI/MCP adapter layer that bridges those primitives to external AI tooling.

## Scope

Exact scope depends on the ADR.  Likely items:

| Item | Placement |
|------|-----------|
| CLI tool: capture Game View frame → PNG file | outside `crates/engine` |
| CLI tool: inject `InputCommand` into the running session | outside `crates/engine` |
| MCP tool adapter: frame capture + input injection | outside `crates/engine` |
| IPC / transport between editor and the CLI/MCP process | decided in ADR |

## Key Constraints

- **An ADR is required before implementation.**  The MCP/CLI surface, IPC
  transport mechanism, and session lifecycle are shared-contract decisions
  (ADR 0028 §Decision 6).  ADR 0026 defines the engine boundary; the engine
  must not change to accommodate this phase.
- **Outside the engine:** PNG encoding, prompt construction, AI API
  communication, and agent orchestration.  These must not enter `crates/engine`
  or `crates/authoring`.
- **OS-level input automation is permanently prohibited** (ADR 0026).
- Virtual input and frame capture are already in `VirtualInputQueue` and
  `FrameCapture`; this phase only wraps them in an external adapter.

## Completion Criteria

- ADR is Accepted.
- CLI / MCP tools can capture a Game View frame as PNG.
- CLI / MCP tools can inject `InputCommand` into the running game.
- All AI API communication lives outside the engine boundary.

## Feeds Into

No further planned phases.  This is the final phase of the current Advanced
Authoring roadmap.
