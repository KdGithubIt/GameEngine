# ADR 0068: Best-Effort Scene View Conversion

- Status: Accepted
- Date: 2026-07-20

## Context

`spawn_from_authoring_scene` converts a scene atomically: when any component
value fails conversion, every spawned entity and asset is rolled back and the
caller receives one error. That contract is correct for Play mode, the player
binary, and packaging, where invalid content must not run silently.

The editor Scene View used the same entry point for its preview world. As a
result, a single invalid component blanked the entire Scene View. The most
common trigger is routine editing: adding a component whose schema has a
required asset reference without a default (for example `engine.animator`'s
`clip_source`) produces a value that cannot convert until the user assigns
the reference, so the whole scene disappeared the moment the component was
added.

## Decision

The engine exposes a second conversion entry point for editing previews:

- `spawn_from_authoring_scene` keeps its atomic contract, unchanged.
- `spawn_from_authoring_scene_best_effort` skips a component whose value
  fails conversion, keeps converting the rest of the entity and scene, and
  records one non-blocking `scene_bridge.component_skipped` warning per skip
  in `AuthoringToRuntimeMap::asset_diagnostics`. Game-module component
  failures are skipped the same way.
- Failures that abort under every policy: blocking scene-level validation
  (the plan cannot be built) and runtime ECS mutation errors (the world is no
  longer trustworthy).

The editor Scene View preview uses the best-effort entry point. Play mode, the
player, and packaging continue to use the strict entry point.

ADR 0137 supersedes the original presentation rule that every skipped component
is listed as persistent yellow Scene View prose. Best-effort skip semantics and
internal `scene_bridge.component_skipped` evidence remain unchanged, but
repairable component problems are presented through the domain-oriented Problems
and navigation model from ADR 0137. Scene View directly owns long-form prose only
when the preview itself cannot meaningfully be produced.

## Consequences

- A newly added, not-yet-configured component no longer hides the scene; the
  user sees the rest of the scene while ADR 0137 keeps the repairable problem
  discoverable through Problems and contextual indicators rather than requiring
  persistent Scene View prose.
- A skipped component may leave partial per-component state (for example a
  cached asset handle) in the preview world. Originally the Scene View rebuilt
  its preview world every frame, so such state could not outlive one frame.
  Under ADR 0072 (persistent preview world) the guarantee is restated: partial
  state cannot outlive the next rebuild of its world, which happens on the next
  scene edit or preview-input change. A skipped component converts to nothing,
  so the only state it can leave is inert until that rebuild; and Stage B keeps
  the stronger whole-entity-respawn form. The best-effort contract is unchanged.
- Runtime hosts keep the atomic guarantee, so shipped content behaves exactly
  as before.
