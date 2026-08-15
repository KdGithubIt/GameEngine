# ADR 0056: Editor Ready Collision, Character, and Combat Contract

Status: Accepted
Date: 2026-07-18

## Context

ER-7 requires deterministic collision transitions, fast-movement protection,
character separation, and reusable combat contacts across the engine, editor,
project Rust ABI, and persisted authoring components. The existing primitive
collision loop only exposed current overlaps and the GameModule host rebuilt
pair history independently.

## Decision

1. The engine collision resource owns canonical `enter`, `stay`, and `exit`
   transitions. Every host consumes that single lifecycle instead of keeping a
   second pair cache.
2. Primitive colliders use deterministic X-axis sweep-and-prune before exact
   narrow-phase tests. The latest proxy, candidate, narrow-phase, and contact
   counts are exposed as `CollisionStats`.
3. Moving trigger shapes retain their prior fixed-step shape and perform a
   conservative relative swept-AABB test when their current exact shapes do
   not overlap. This prevents fast melee volumes from skipping targets.
4. The kinematic character controller subdivides long displacement, resolves
   stable entity-ordered contacts, applies slope, step, ground-snap, skin, and
   ceiling rules, and symmetrically separates character-controller pairs.
5. Static environments use authorable primitive colliders. Compound collision
   is represented by multiple child entities, each with one primitive collider
   and a static body. Mesh colliders are not part of Editor Ready v1.
6. `engine.damage_receiver` is the stable authoring component for health,
   team, and invulnerability settings. `AttackHitbox` activation history,
   `HitResults`, and `KnockbackRequests` are engine-owned fixed-step state.
7. Project Rust may consume the additive `hit` event stream, inspect
   `damage_receiver_state`, and create hitboxes with optional knockback. Older
   hitbox commands omit knockback and retain zero impulse.
8. Scene View draws each supported primitive from its exact world shape.

## Consequences

Collision lifecycle and combat results are identical at any rendered frame
rate because production occurs in the fixed schedule. Broad-phase behavior is
inspectable and no longer scales as an unconditional all-pairs narrow phase.
The conservative swept trigger test may report a corner hit for shapes whose
enclosing boxes cross; exact continuous primitive casts may replace it later
without changing event or authoring contracts.

Compound static environments require more entities than a triangle-mesh
collider, but remain deterministic, portable, editable, and package-neutral.

## Alternatives Considered

- A third-party rigid-body and query library was deferred because ER-7 needs a
  narrow primitive feature set and adopting a second ECS/world would broaden
  runtime ownership and packaging work.
- Host-owned collision pair history was rejected because it allowed Editor
  Play, packaged Player, scripts, and project Rust to observe different phases.
- Serialized mesh collision was deferred because its cooking format and asset
  dependency policy require a separate compatibility decision.

## Compatibility and Migration

`engine.character_controller` version 2 adds fields with runtime defaults;
version-1 scene values continue to load through optional-field defaults.
`engine.damage_receiver` is new. GameModule enum variants and the optional
knockback payload are additive. Existing `create_hitbox` calls remain valid and
produce zero knockback. No existing stable ID or field is renamed.
