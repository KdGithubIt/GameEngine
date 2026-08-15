# ADR 0033 — Animation State Machine Graph Schema

## Status: Accepted

## Context

Phase 38 implements an animation state machine by reusing the graph foundation
in `crates/authoring` (`GraphDomain`, `NodeSchema`, `GraphKind`).  The
domain-specific node types, port schema, and compile contract form a new
serialized format and must be decided before implementation.

## Decision

### Graph kind

`"anim.graph"` — animation state machine graphs carry this kind string.

### Node types

| Type ID | Description |
|---------|-------------|
| `anim.State` | An animation state; plays the clip identified by `clip_id` when active. |
| `anim.Entry` | Marks the default initial state.  Exactly one per graph. |

### Port schema

Each node type has exactly two port schemas with **hardcoded stable IDs**
(fixed 26-character Crockford base32 strings, same convention as the Behavior
Tree domain):

| Port constant | ID suffix | Direction | Node |
|---------------|-----------|-----------|------|
| `ANIM_ENTRY_OUT_PORT` | `0000000000000000ANENTRPRT0` | Output | `anim.Entry` |
| `ANIM_STATE_IN_PORT`  | `0000000000000000ANSTATEPRT` | Input  | `anim.State` |
| `ANIM_STATE_OUT_PORT` | `0000000000000000ANSTATEPTR` | Output | `anim.State` |

Edges connect `out.default → in.default` to model transitions.

### Edge annotations

Transitions carry a `condition` annotation key (string) that labels the event
or guard that triggers the transition.  Empty string means "unconditional".

### Compile step

`compile_animation_graph(domain, graph)` validates the graph via the domain,
then produces `CompiledAnimGraph { states, transitions, entry_state }`.

### Validation rules

1. Exactly one `anim.Entry` node (error if zero or more than one).
2. `anim.Entry` must connect to at least one `anim.State` (error otherwise).
3. `anim.State` nodes without a `clip_id` annotation generate a non-blocking
   warning.

## Consequences

- Animation state machine files are `.graph.json` files with `kind: "anim.graph"`.
- The existing graph editor (Phase 27) can be extended with animation graph
  node registration without changes to the graph foundation.
- The Behavior Tree domain code is **not** duplicated; only the domain
  implementation differs.
