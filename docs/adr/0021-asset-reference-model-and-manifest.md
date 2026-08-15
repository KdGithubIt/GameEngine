# ADR 0021: Asset Reference Model and Asset Manifest

Status: Accepted
Date: 2026-06-10

## Context

Specification §7.4 fixes the tagged reference forms to `entity_ref` and
`asset_ref`. The Phase 2.5 bridge resolves four hard-coded builtin
`AssetId` values (`crates/engine/src/scene_bridge.rs`).

`IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md` Phase 14-C originally proposed a
third tagged form, `{ "$type": "asset_path", "path": "meshes/player.obj" }`,
embedding file paths in scene files. File paths are not stable identifiers:
renaming an asset file would break every referencing scene, contradicting
§7.1 and ADR 0004 (identifiers never change on rename). Path strings are
also unreliable cache keys on Windows (case, separators).

## Decision

1. Scenes reference assets exclusively through
   `{ "$type": "asset_ref", "id": "asset_<ULID>" }` (`Value::AssetRef`).
   No path-shaped tagged value is added. Phase 14-C is rewritten
   accordingly.
2. A project-owned **asset manifest** maps `AssetId` to file data. Location:
   `<project root>/asset_manifest.json`, next to `project.json`:

   ```json
   {
     "schema_version": 1,
     "assets": {
       "asset_01JZ...": { "path": "meshes/player.obj", "name": "player_mesh" }
     }
   }
   ```

   - `assets` is a `BTreeMap<AssetId, ManifestEntry>` for deterministic
     serialization (§15.2). `schema_version` follows the ADR 0020 policy.
   - `path` is relative to the assets root and MUST pass `ProjectRoot`
     containment validation (ADR 0023). A structurally valid entry whose
     file is absent is not a load failure; it produces the
     `asset.missing_file` diagnostic instead.
   - `name` is the search slug per §7.1. `display_name` and `description`
     MAY be added later without a format break.
3. Manifest placement rationale: the manifest is project-level data, not an
   asset. The asset browser scans only the assets root and intentionally
   does not list the manifest. Trade-off accepted: distributing the
   `assets/` directory alone separates content from its manifest; the
   project root is the distribution unit, exactly as for `project.json`.
4. Resolution flow: `asset_ref` → manifest lookup → resolved path →
   `AssetServer` load → `RuntimeAssetId`. The per-session
   `AssetId → RuntimeAssetId` mapping (§5.2) is unchanged. `AssetServer`
   caches keyed by `AssetId`, not by path string.
5. Renaming or moving an asset file updates one manifest entry; scene files
   are untouched.
6. Builtin engine assets keep their reserved `AssetId` constants and resolve
   without a manifest entry. A manifest entry that redefines a builtin ID is
   a diagnostic error (`asset.builtin_conflict`).
7. Consistency diagnostics: `asset.missing_file` (entry without file,
   error) and `asset.unregistered_file` (file without entry, warning) are
   produced by project validation and surfaced by the asset browser.

## Consequences

- Rename-safe references; one-line-per-asset Git diffs; Windows-safe cache
  keys; AI tools search by `name` and edit by `AssetId` (§7.1 workflow).
- One extra document to keep in sync; orphan detection is required tooling.
- Phase 14 gains a small manifest-editing surface (register asset on
  import).

## Alternatives Considered

- Path-based references (original plan 14-C): rejected; breaks on rename
  and contradicts §7.4 / ADR 0004.
- Per-asset sidecar `.meta` files: avoids a central file but multiplies
  file count and merge noise; revisit only if manifest merge conflicts
  become a real problem.
- Content-hash identifiers: rejected; intentional edits change identity.
- Manifest inside `assets/`: rejected; the manifest would appear in the
  asset browser as an unknown asset and would itself be subject to asset
  path rules.

## Compatibility and Migration

The scene format is unchanged. The manifest is a new document, versioned
from birth per ADR 0020. The builtin fallback keeps Phase 2.5 examples
working with no manifest present.
