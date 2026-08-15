# Phase 26 — Project Hub / Scene-first Startup

> Historical implementation note: ADR 0117 supersedes this phase's placement
> of the Project Hub and recent-project ownership inside the Editor. The
> project-selection UX moves to the separate Launcher / Project Manager while
> `ProjectRoot` remains owned by `engine-authoring` under ADR 0023.

## Goal

Make the editor cold-start land on a Project Hub screen rather than directly
opening the Behavior Tree graph canvas.  The hub shows Recent Projects, New
Project, and Open Project actions.  Once a project is open the Scene workspace
is the default view.

## Why

Phase 9 implemented the underlying project-opening machinery (`ProjectRoot`,
`EditorPreferences.recent_projects`, Asset Browser), but the editor still
starts inside the graph canvas.  New users have no obvious entry point to
create or open a game project, and switching between projects requires
navigating hidden menu items.

## Scope

| Item | Location |
|------|----------|
| Startup screen with Recent / New / Open actions | `crates/editor/src/hub.rs` (new) |
| "New Project" flow: name + directory picker (`rfd`) | `crates/editor/src/hub.rs` |
| "Open Project" flow: directory picker (`rfd`) | existing `EditorSession::open_project` |
| Recent projects list from `EditorPreferences` | existing `crates/editor/src/preferences.rs` |
| Scene workspace as default view after project open | `crates/editor/src/app.rs` |
| Graph canvas shown only when a graph asset is open | existing `CurrentDocument::Graph` path |
| Dirty-document guard before switching project | existing `is_dirty` logic |

## Key Constraints

- **No new project model.**  `ProjectRoot` and ADR 0023 are the existing
  contract; this phase adds UX over that model only.
- **Recent project history stays in editor preferences** (`crates/editor`),
  not in `engine-authoring`.
- `rfd` is already a dependency (introduced in Phase 8-C).
- The graph canvas remains the view for graph assets; it is not removed.

## Completion Criteria

- Editor cold start shows the Project Hub or the most-recent project's Scene
  workspace.
- The graph canvas is not shown until a `.graph.json` asset is opened.
- New Project / Open Project / Recent Projects all route through `ProjectRoot`.
- Switching project with an unsaved document shows a dirty-guard prompt.

## Feeds Into

Phase 27 (Edit Mode Scene View — needs the project-open lifecycle to be the
front-door workflow before adding a visual editing canvas).
