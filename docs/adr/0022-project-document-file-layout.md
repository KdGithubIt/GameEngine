# ADR 0022: Project Document File Layout (Return to ADR 0008)

Status: Accepted (supersedes ADR 0019)
Date: 2026-06-10
Legacy read-support removed: 2026-06-11

## Context

ADR 0008 decided that semantic and presentation graph data are serialized as
separate documents and fixed the naming convention `<name>.graph.json` /
`<name>.graph.view.json`. That decision remains authoritative and is not
redefined here.

ADR 0019 later introduced an editor-only combined single-file format
(`format_version: 1`, wrapping `graph` and `graph_view`) as a convenience at
a time when the editor had no project or file-management concept.

Phase 9 introduces `ProjectRoot` (ADR 0023) and an asset browser whose kinds
assume separate files. The CLI (Phase 5) reads and writes bare `Graph` JSON.
Specification §18 expects humans, AI agents, CLI, and editor to edit the
same project files. The combined format prevents CLI and MCP tooling from
editing editor-saved documents and double-encodes the graph.

The only consumers of the combined format are editor-internal
(`EditorSession::save_to_path` / `load_from_path` and their tests); CLI,
MCP, and authoring services never read it.

## Decision

1. The editor returns to the ADR 0008 file layout. Inside a project,
   documents are stored as separate files:
   - `*.scene.json` — `AuthoringScene` canonical JSON (versioned, ADR 0020)
   - `*.graph.json` — semantic `Graph` (existing CLI format)
   - `*.graph.view.json` — `GraphView` presentation document (optional)
   ADR 0008's naming and semantic/presentation separation remain the
   authority; documents that spell the view suffix `.graph_view.json` are
   corrected to ADR 0008's `.graph.view.json`.
2. Editor open: opening `foo.graph.json` auto-loads a sibling
   `foo.graph.view.json` when present; a missing view file is not an error
   (§9.1: presentation is regenerable).
3. Editor save: semantic file first, then view file, each through
   `engine_authoring::persist::replace_file_contents`. The pair is not
   atomic; a crash between writes loses only regenerable presentation data
   (ADR 0008 / §9.1).
4. ADR 0019's combined format is **superseded**: read-only support for
   `format_version: 1` combined files is retained, an opened combined
   session converts to the separated layout on its next save, the combined
   writer is removed, and read support is scheduled for removal after
   Phase 10 ships.
5. ADR 0019's status becomes `Superseded` referencing this ADR; the ADR
   index is updated.

## Consequences

- CLI, MCP, and editor operate on identical files (§18 and the §24
  reference scenario).
- Asset browser kind detection is a suffix match.
- Semantic-versus-presentation churn is visible in Git, making §18's
  "avoid changing both files when only one is necessary" reviewable.
- The editor session tracks two paths per graph document (matches the
  planned `CurrentDocument` shape in Phase 9-C).

## Alternatives Considered

- Keep the combined format inside projects: rejected; CLI and MCP cannot
  edit those files without learning an editor-private wrapper.
- Teach the CLI the combined format: rejected; violates the thin-adapter
  rule (§16) and duplicates parsing.

## Compatibility and Migration

Only development-local combined files exist today. Conversion is automatic
on open-then-save. No CLI, MCP, scene, or graph payload format changes.
