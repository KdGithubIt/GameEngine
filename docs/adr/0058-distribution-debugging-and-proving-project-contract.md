# ADR 0058: Distribution, Debugging, and Proving Project Contract

Status: Accepted

## Context

ER-9 through ER-12 require scene flow, save recovery, prefab editing, runtime
debugging, packaging, and the proving game to use the same editor-to-player
path. Package-relative writable data and a standalone sample implementation
would make editor acceptance diverge from distribution behavior.

## Decision

1. Distributed saves and logs use the OS local-data directory. Package-local
   `saves/` or `logs/` require the explicit `GAMEENGINE_PORTABLE_SAVES` or
   `GAMEENGINE_PORTABLE_LOGS` opt-in.
2. Save slot enumeration reports corrupt documents as metadata diagnostics;
   one corrupt slot never hides other slots or crashes the host.
3. Repeated requests for the current or already-pending scene are ignored.
   Different pending requests retain ADR 0047's visible last-request-wins rule.
4. Prefab instance roots persist an editor-only `editor.prefab_instance`
   source marker. The runtime bridge ignores editor-only components. Create,
   place, apply, revert, and unpack operations remain atomic authoring
   transactions. Packaging traverses nested source markers and rejects cycles.
5. Editor Play owns Pause, Resume, and single-step state. A single step runs
   exactly one fixed update and one frame update. Fixed-step and collision
   counts are read-only runtime diagnostics.
6. Package output always contains a deterministic `build_report.json` and
   `THIRD_PARTY_NOTICES.txt`, copies project legal notices, supports Unicode
   and spaces, and records the symbol/crash policy. The player appends startup
   failures to a writable log directory.
7. `examples/busters_lite` is the proving project's authoring source of truth.
   Its project-local Rust module, scenes, UI, NavMesh, prefabs, save commands,
   and package dependencies are shared by Editor Play and Player. The engine
   example with the same name contains no separate gameplay implementation.
8. Project Rust compiler messages are retained in Problems as well as Console.
   A source-linked diagnostic stores a project-relative path and optional line;
   editor navigation canonicalizes the target below `game/` before opening it.

## Consequences

- Deleting a distributed package no longer deletes user progress by default.
- Editor metadata remains reviewable in scene JSON without becoming a runtime
  ECS component.
- Package contents and portability policy can be audited without launching the
  executable.
- Proving-project failures exercise product paths instead of a bespoke sample.

## Compatibility

Existing editor saves remain under each project. Portable distributions can
retain the old package-relative behavior through the environment opt-in.
Prefab schema v1 and scene schema v1 are unchanged; the source marker is an
ordinary additive authoring component ignored by older runtime bridges.
The `source_file` diagnostic target is additive; older diagnostic consumers
that ignore unknown target kinds continue to consume severity, code, and text.
