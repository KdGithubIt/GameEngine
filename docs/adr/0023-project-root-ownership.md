# ADR 0023: Project Root Ownership in engine-authoring

Status: Accepted
Date: 2026-06-10

## Context

Phase 9 introduces `ProjectConfig` / `ProjectRoot` (project folder, standard
directory layout, safe asset path resolution). The phase-09 document
contradicts itself about placement: its rationale section suggests a
temporary editor-crate module, while its implementation plan and
`IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` specify
`crates/authoring/src/project.rs`.

The types are GUI-free. The asset manifest (ADR 0021) is authoring data that
needs path resolution. CLI and MCP will need project-scoped operations.
Specification §16 forbids adapters from owning unique editing logic, and §17
requires path-safety checks at a shared boundary, not in one adapter.

## Decision

1. `ProjectConfig`, `ProjectRoot`, and `ProjectError` live in
   `crates/authoring/src/project.rs` (`engine_authoring::project`).
2. `ProjectRoot` owns: `project.json` create/load (with `schema_version`
   per the ADR 0020 policy), the standard directory layout
   (`assets/{scenes,graphs,meshes,textures,audio}`), and asset path
   resolution that rejects absolute paths, parent traversal, and resolved
   symlink escapes (§17).
3. Path resolution provides two explicit modes:
   - `resolve_asset` (read): canonicalize-based, for existing files
     (model: `engine::asset::AssetServer::resolve_path`).
   - `resolve_asset_for_write`: canonicalizes the parent directory and
     validates the final component lexically, because `fs::canonicalize`
     fails on not-yet-existing save targets. Both modes MUST stay under
     the assets root.
4. Editor preferences (recent projects, window state) are editor UI state
   and stay in `crates/editor` (`dirs::config_dir()`); they MUST NOT enter
   `engine-authoring`.
5. The runtime `engine::asset::AssetServer` keeps its own independent root
   confinement and does not require a `ProjectRoot`; editor and CLI resolve
   project paths and hand validated paths to the runtime.

## Consequences

- CLI/MCP project support later reuses the same type with no move.
- The phase-09 placement contradiction is resolved in favor of authoring.
- `engine-authoring` already performs file I/O (`persist.rs`); no new
  dependency class is introduced.

## Alternatives Considered

- Editor-crate placement with a later move: rejected; the move would churn
  a public API and invite CLI-side path logic duplication in the meantime.
- A dedicated `crates/project` crate: rejected for two small types; can be
  extracted later without changing the public path semantics.

## Compatibility and Migration

New public API only. `project.json` is a new versioned document. No
existing serialized formats change.
