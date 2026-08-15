# ADR 0019: Phase 8-C Editor File Persistence

Status: Superseded by ADR 0022
Date: 2026-06-06

## Context

Phase 8-B introduced session-local undo/redo (ADR 0018). History is in-memory
and does not survive process restarts. To be useful as an authoring tool the
editor must be able to save the current graph and graph view to disk and reload
them in a later session.

The editor session holds two documents: a `Graph` (semantic) and an optional
`GraphView` (presentation). The CLI and MCP adapters already save these as
separate files through `engine-authoring::persist::replace_file_contents`. The
visual editor wraps both documents in a single human-readable file so that
users can move, rename, and version-control one file per project.

## Decision

Phase 8-C introduces a **single-file combined JSON document** for editor
sessions, using the extension `.json`. The format wraps the two authoring
documents under a top-level version field:

```json
{
  "format_version": 1,
  "graph": { ... Graph JSON ... },
  "graph_view": { ... GraphView JSON ... }
}
```

`graph_view` is `null` when the session has no view document.

`EditorSession::save_to_path(path: &Path)` serializes both documents and writes
the combined file using `engine-authoring::persist::replace_file_contents` (the
shared atomic write helper from ADR 0008).

`EditorSession::load_from_path(path: &Path)` reads and parses the combined file,
validates `format_version == 1`, deserializes both documents, and returns a new
`EditorSession`. Undo history is not loaded; callers must call
`EditorSession::clear_undo_redo()` after replacing a session.

Native file dialogs use the `rfd` crate. `EditorApp` tracks the currently open
path. Ctrl+S saves to the current path without a dialog (Save); Ctrl+Shift+S
and the "Save As" button always open a dialog. Ctrl+O opens a file.

## Consequences

- Users can save and reload Behavior Tree graphs across editor sessions.
- The combined file keeps graph and view together, simplifying file management.
- `format_version` allows a future ADR to extend or migrate the format without
  breaking existing files.
- Undo history is session-local and is discarded on load (per ADR 0018).
- No change to CLI, MCP, runtime, or ECS serialized formats.

## Alternatives Considered

### Separate graph and view files (CLI/MCP convention)

The CLI saves `<name>.graph.json` and `<name>.graph_view.json` as separate
files. Rejected for the visual editor because users would need to manage two
files per project, and the "open file" dialog would need to infer the
corresponding view path. A combined wrapper is simpler for a GUI tool.

### Custom binary format

Rejected. The graph scale does not justify binary encoding complexity. JSON
is readable, diff-friendly, and compatible with existing authoring tooling.

### Reuse `BehaviorTreeAuthoringService::graph_to_canonical_json`

The canonical JSON path uses the schema registry and is the authoritative
round-trip for the CLI. For the editor combined document, the inner `graph`
field must be embeddable as a `serde_json::Value`, not a flat string. Both
paths use the same `serde_json` serialization and produce identical content.

## Compatibility and Migration

`format_version: 1` files produced by Phase 8-C are self-describing. A future
format change increments `format_version` and may provide a migration path.

No existing CLI, MCP, ECS, or runtime serialized formats are affected. The
combined document is an editor-only file format; authoring services continue
to accept and produce plain `Graph` JSON.
