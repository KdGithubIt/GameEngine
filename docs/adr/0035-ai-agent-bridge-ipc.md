# ADR 0035 — AI Agent Bridge IPC

## Status: Accepted

## Context

Phase 40 wraps the `FrameCapture` (Phase 10-E) and `VirtualInputQueue`
(Phase 12-F) primitives as CLI and MCP tools for external AI agents.
ADR 0026 defines the engine boundary: the engine must not change for this
phase.  ADR 0028 §Decision 6 requires an ADR for the CLI / MCP surface.

## Decision

### IPC transport: filesystem inbox / outbox

The running editor session polls a **session directory** for incoming input
commands and writes frame PNGs to an outbox.

```
<session_dir>/
  inbox/     # agent drops AiAgentInput JSON files; editor reads and deletes
  outbox/    # editor writes <frame_N>.png after each fixed tick
```

### CLI subcommands (added to `crates/cli`)

| Subcommand | Arguments | Description |
|-----------|-----------|-------------|
| `ai-agent describe-tools` | — | Returns a JSON array of available AI agent tools |
| `ai-agent validate-input <json>` | JSON string | Validates an `AiAgentInput` document |

### MCP tools (added to `crates/mcp`)

| Tool name | Description |
|-----------|-------------|
| `ai_agent.describe_session` | Returns session capabilities as JSON |
| `ai_agent.validate_input` | Validates an agent input payload |

### Data types (in `crates/mcp/src/ai_agent.rs`)

```rust
pub struct AiAgentInput { pub action: String, pub payload: serde_json::Value }
pub struct AiAgentOutput { pub success: bool, pub message: String }
```

Known `action` values: `"key_press"`, `"key_release"`, `"mouse_move"`,
`"mouse_click"`, `"capture_frame"`.

### Deferred

The editor session poll loop (reading inbox, writing outbox) is deferred to a
future integration phase.  This ADR establishes the protocol; Phase 40
implements the CLI / MCP adapter layer only.

## Consequences

- No OS-level input automation: ADR 0026 boundary is maintained.
- The filesystem IPC approach is debuggable and language-agnostic; agents can
  be implemented in any language.
- Frame and input latency is bounded by the editor's poll rate (approximately
  one fixed timestep, ~16 ms at 60 Hz).
- `crates/engine` and `crates/authoring` are not changed by Phase 40.
