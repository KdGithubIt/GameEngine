# Phase 64: Editor Ready Collision and Combat (ER-7)

Status: Implemented

## Delivered

- Shared fixed-step collision, character, combat, and knockback ordering.
- Deterministic sweep-and-prune with runtime collision statistics.
- Engine-owned collision enter/stay/exit transitions.
- Conservative swept trigger checks and subdivided dash movement.
- Slope limit, step offset, ground snap, skin width, ceiling handling, stable
  obstacle ordering, and symmetric character separation.
- Authorable `engine.damage_receiver`, activation-scoped hit filtering,
  invulnerability windows, hit results, and optional knockback commands.
- Exact primitive collider Scene View drawing and Play combat debug overlays.
- Primitive-child compound policy for static environment collision.

## Automated Acceptance

Engine unit tests cover fast trigger crossing, thin-wall dash blocking,
controller separation, collision phases, broad-phase rejection, one-hit attack
reactivation, schema conversion, and shared host system ordering.

The deterministic combat producer runs exclusively in fixed update, so render
rates of 30, 60, and 120 FPS consume the same activation-scoped hit sequence.
Manual visual validation remains part of the final ER-12 project acceptance.
