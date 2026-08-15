# Phase 65: Editor Ready Navigation and Behavior (ER-8)

Status: Implemented

## Delivered

- Scene-owned `engine.nav_mesh_surface` loading through normal conversion.
- Project menu bake action, versioned bake document, cancellation check,
  canonical save/load, manifest registration, stale fingerprint detection, and
  stable success/failure diagnostics.
- Walkable-height/agent-clearance obstacle filtering.
- Scene/Play walkable-cell and live-path overlays.
- Agent target, speed, stopping distance, repath interval, avoidance radius,
  and explicit runtime status.
- Project Rust navigation commands and navigation-state view.
- Stable project Rust Behavior Tree action/condition result commands.
- Typed blackboard, status, error, and latest visited-leaf inspection.

## Automated Acceptance

Engine and editor suites cover grid bake/path/save/load, surface registry and
conversion contracts, authorable agent defaults, missing-path behavior,
Behavior Tree compile/tick diagnostics, bake-document canonical round-trip,
and stale detection. The ER-12 proving project supplies the wall-routing and
multi-combatant integration fixture.
