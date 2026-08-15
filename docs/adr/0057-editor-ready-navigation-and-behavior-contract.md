# ADR 0057: Editor Ready Navigation and Behavior Contract

Status: Accepted
Date: 2026-07-18

## Context

The Phase 44 grid NavMesh and Behavior Tree executor existed as code, but a
normal scene could not load a baked navigation resource and project Rust could
not assign agent targets or register stable action/condition results.

## Decision

1. `engine.nav_mesh_surface` is the stable scene component that references one
   registered `.navmesh.json` artifact and installs `NavMeshQuery` during the
   normal authoring-to-runtime conversion.
2. A scene uses at most one active surface. The editor stores bake settings,
   output path, schema version, and a normalized scene fingerprint in a
   `.navmesh.bake.json` document. Surface-reference changes do not stale their
   own bake.
3. The grid bake includes static, non-trigger obstacle colliders intersecting
   the configured walkable-height/agent-height band. Floor colliders below the
   band are not obstacles.
4. `engine.nav_mesh_agent` version 2 adds repath interval, avoidance radius,
   and explicit idle/missing/no-path/moving/arrived status. A deterministic
   symmetric separation pass supplies local avoidance for the proving game.
5. Project Rust uses the additive Navigation command family to set/clear
   targets and reads status/path through `navigation_state`.
6. Project Rust registers stable Behavior Tree action and condition results
   through the additive BehaviorTree command family. Missing implementations,
   invalid graphs, and missing NavMesh resources remain explicit runtime or
   conversion diagnostics.
7. The Behavior Tree state view exposes the typed blackboard, last status,
   error, and leaf nodes visited during the latest tick. Scene and Play debug
   drawing show walkable cells and live paths.

## Consequences

Editor Play and packaged Player obtain navigation from the same scene asset
and runtime systems. The current grid bake targets mostly planar action-game
arenas; arbitrary multilayer navigation requires a later asset/schema ADR.

## Alternatives Considered

- Inserting `NavMeshQuery` from game-specific launcher code was rejected
  because it bypasses normal editor/package authoring.
- Direct straight-line movement as a missing-path fallback was rejected
  because it hides authoring errors and lets agents cross walls.

## Compatibility and Migration

The surface component and GameModule command/view variants are additive.
Version-1 agent values load version-2 fields through defaults. Existing
Behavior Tree graphs and stable behavior IDs do not change.
