# Phase 38 — Animation Authoring / Animation Graph

## Goal

Build an Animation Graph domain that reuses the existing graph foundation
(`crates/graph`) for authoring animation state machines.  Graph validation,
compile, and runtime transition must each be tested.  The Behavior Tree domain's
shared graph model must not be duplicated.

## Why

Animation state machines (idle → walk → run → jump) require graph-based
authoring.  The graph foundation (Phases 3–8) can host this domain without
duplication, just as it hosts the Behavior Tree domain.  Reusing the foundation
avoids a second graph editor implementation.

**Prerequisite: Phase 37 (Animation Runtime) for the clip playback layer.**

## Scope

| Item | Location |
|------|----------|
| Animation Graph domain using `crates/graph` foundation | `crates/authoring/src/animation_graph/` (new) |
| State node / transition edge schema | domain-specific types |
| Graph validation (unreachable states, missing clips, invalid transitions) | domain validation |
| Compile step: graph → runtime transition table | domain compile |
| Runtime transition evaluation (condition-based) | `crates/engine/src/animation.rs` |

## Key Constraints

- **Must reuse `crates/graph`.**  Do not duplicate the graph node/edge/port
  model from the Behavior Tree domain.
- Depends on Phase 37 for the clip playback layer that transitions drive.
- Validation / compile / runtime transition must each have independent tests.

## Completion Criteria

- Animation Graph domain is implemented on top of `crates/graph` foundation.
- Graph validation / compile / runtime transition tests all pass.
- No shared graph model is duplicated between Animation Graph and Behavior Tree
  domains.

## Feeds Into

Phase 39 (Build / Packaging — animation graph assets must be includable in the
package alongside clip assets).
