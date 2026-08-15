# ADR 0060: Editor-ready UI and Workflow Documents

Status: Accepted

Date: 2026-07-18

## Context

The runtime could interpret four UI node kinds, but the editor opened
`*.ui.json` through the operating system. Scene hierarchy reparenting also had
no authoring command, and crash recovery was absent. Runtime reachability was
therefore being confused with usable human authoring.

## Decision

- `UiDocument` schema version 2 is additive and introduces image with
  nine-slice, progress bar, stack, grid, overlay, and scroll-view nodes.
- Version 1 UI documents migrate in memory to version 2. Future versions are
  rejected. Runtime loading validates the complete tree before replacing a
  live document.
- UI editing uses `UiDocumentCommand` and `UiDocumentTransaction`; the editor
  owns only selection, preview, clipboard, and other presentation state.
- UI documents are first-class editor documents with atomic save and the same
  snapshot Undo/Redo policy as graph documents.
- Scene hierarchy reparenting is represented by the additive
  `AuthoringCommand::SetEntityParent` command. It validates missing parents,
  self-parenting, and descendant cycles and records an inverse command.
- Editor recovery files are non-authoritative `.autosave` siblings. A newer,
  valid snapshot is restored as dirty state; a normal save removes it.
- Asset deletion is a move into project-local `.engine/asset_trash`, not an
  irreversible unlink. File relocation and manifest persistence roll back
  together when persistence fails.
- When the project file watcher observes an external removal below `assets/`,
  the editor automatically unregisters only the affected missing manifest
  entries. This cleanup is deferred while a dirty document is open so the
  author can still save the document back; the Project menu remains a fallback
  for older orphaned entries and deferred removals.
- Runtime inspection is read-only and may expose a curated set of known
  components without adding reflection requirements to the runtime ECS.

## Consequences

- Existing version 1 UI assets continue to load and are rewritten as version
  2 on the next save.
- UI Builder, CLI, and future MCP adapters can share the same structural UI
  commands.
- Reparenting is available to every authoring adapter rather than being an
  editor-only direct mutation.
- Recovery and trash data stay outside runtime packages and authoring assets.
- The initial runtime inspector is useful without promising arbitrary Rust
  value reflection.

## Alternatives Considered

### Keep schema version 1 and add unversioned variants

Rejected because older binaries could not distinguish the expanded contract
from the original four-node format.

### Let the editor mutate UI and hierarchy structures directly

Rejected because it would violate the shared authoring command boundary and
make Undo, validation, CLI, and MCP behavior diverge.

### Permanently delete assets

Rejected because a human editor should prefer recoverable operations and
because manifest persistence can fail after a filesystem mutation.

## Compatibility and Migration

`UiDocument::from_json_str` upgrades schema versions below 2 after parsing.
No version 1 field or node changes meaning. `SetEntityParent` is an additive
serialized command variant. Recovery and trash paths are editor-local and are
not part of package or runtime serialization.
