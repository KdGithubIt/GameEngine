# ADR 0062: Asset Folder Batch Transaction

Status: Accepted

Date: 2026-07-19

## Context

Physical asset folders need batch move, rename, and recoverable deletion while
registered assets retain stable IDs. Partial filesystem mutation would leave
the manifest and authoring references inconsistent.

## Decision

Folders are physical entries below `assets/` and never receive `AssetId`
values. A batch operation validates every normalized source and destination,
rejects absolute paths, traversal, symlinks, collisions, and descendant
cycles, and collects affected manifest rows before mutation.

Filesystem renames execute in deterministic order. Manifest paths are then
rewritten by exact path or folder-prefix match while IDs remain unchanged and
the manifest is atomically persisted. Any failure rolls completed renames back
in reverse order and restores the in-memory manifest. Rollback failure reports
the exact recovery paths as a blocking diagnostic. Deletion moves the complete
batch into a unique `.engine/asset_trash/` directory before unregistering
affected manifest rows.

## Consequences

The browser can organize nested folders and multiple assets without changing
logical asset identity. Folder cycles, collisions, and invalid targets leave
the project unchanged. Trash remains outside runtime packages.

