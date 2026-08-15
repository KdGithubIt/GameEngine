# ADR 0015: MCP Behavior Tree Tool Adapter

Status: Accepted
Date: 2026-06-06

## Context

Phase 6 requires an MCP adapter that lets AI agents discover Behavior Tree
schemas, inspect graphs, validate, apply bulk commands, compile, and request
layout without knowing Rust source code or calculating node coordinates.

The project does not yet have an accepted MCP transport and process lifecycle.
Open Decision #7 still covers transport ownership, connection lifecycle, and
transaction identity across multiple MCP calls. The implementation still needs a
testable adapter boundary now, and it must not duplicate CLI editing logic.

## Decision

Add `crates/mcp` as `engine-mcp`, a thin MCP tool-handler crate over
`engine-authoring`.

The first tool set is Behavior Tree only:

- `behavior_tree.schemas`
- `behavior_tree.validate`
- `behavior_tree.compile`
- `behavior_tree.layout`
- `behavior_tree.nodes`
- `behavior_tree.edges`
- `behavior_tree.apply`

`behavior_tree.apply` accepts a semantic graph document and an array of
`GraphCommand` values, applies them as one transaction through
`BehaviorTreeAuthoringService`, and returns success state, diagnostics, semantic
diff, and the updated graph when validation succeeds.

The MCP adapter crate owns:

- Tool-shaped input and output DTOs.
- Tool descriptors and JSON input schemas for registration by a future MCP
  transport layer.
- Domain routing and wrong-domain input rejection.

The MCP adapter crate does not own:

- MCP server process lifecycle or transport binding.
- File persistence policy.
- Authoring command semantics.
- Graph validation, compilation, layout, or transaction logic.
- Runtime ECS execution.

## Consequences

- CLI and MCP can share the same Behavior Tree authoring service.
- MCP tool behavior is testable without an MCP server process.
- AI-facing bulk command application exists before the final transport is
  selected.
- The future MCP server layer can remain a small wrapper that registers these
  handlers and maps transport errors.

## Alternatives Considered

### Put MCP handlers in `crates/cli`

Rejected. CLI owns command-line parsing and file-path behavior. MCP needs
tool-shaped structured inputs and outputs without inheriting CLI file semantics.

### Wait for full MCP transport ADR

Rejected. Tool behavior can be implemented and tested independently while the
transport lifecycle decision remains open.

### Make MCP tools call CLI commands

Rejected. That would route structured authoring edits through stdout, argv, and
temporary files, and would duplicate error mapping instead of sharing the
authoring service directly.

## Compatibility and Migration

No persisted project data changes.

No existing CLI output changes are required. `behavior-tree apply` is added as a
CLI write command that shares the same authoring service path as MCP apply and
the existing `commit` command.

The MCP transport lifecycle remains open and must be resolved before exposing a
long-running MCP server process.
