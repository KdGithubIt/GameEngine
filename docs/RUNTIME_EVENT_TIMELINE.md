# Runtime combat and animation event timeline

## Event Timeline Viewer GUI

Open a project in Engine Editor, select **Authoring Tools**, then open **Runtime
Event Timeline**. The viewer appears as a modeless window inside the current
editor process; no separate viewer executable or first-use Cargo build is
required.

Use **Launch Engine Editor with Live Capture** in the embedded viewer. It starts a
normal `engine-editor` process with `GAMEENGINE_EVENT_TRACE_PATH` configured.
During Play, the runtime writes updated snapshots whenever an animation marker or
accepted hit is added.

The viewer provides:

- automatic 250 ms refresh
- Animation Event and Combat Hit filters
- free-text search across event names and entity IDs
- newest-first or oldest-first ordering
- fixed-step and monotonic sequence columns
- attacker, hitbox, target, damage, remaining health, activation, and clip time
- selectable capture path and loading of previously saved traces

The trace uses `engine::RuntimeEventTrace` JSON, including entity generations,
so stale runtime IDs are not confused with reused entities.

## Runtime contract

`engine::RuntimeEventTimeline` retains a bounded, fixed-step ordered history of:

- animation markers published through `AnimationEvents`
- accepted combat contacts published through `HitResults`

The shared runtime profile installs the timeline in both Editor Play and the
packaged Player and registers `engine.event_timeline` after animation and combat
knockback. Every entry includes a local monotonic sequence and fixed-step index.
Producer generation cursors prevent stale resource contents from being appended
again.

The default capacity is 256 entries. Oldest entries are discarded first. This is
an inspection resource, not a gameplay event bus. Project gameplay must continue
to consume the typed Animation and Hit event streams through the project Rust
API.
