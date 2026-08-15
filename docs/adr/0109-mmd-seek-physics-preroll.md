# ADR 0109: MMD Seek Physics Pre-Roll

Status: Superseded by ADR 0112
Date: 2026-08-14

## Context

ADR 0096 deliberately reseats an isolated MMD Rapier world whenever animation
continuity breaks. Reseating every body on the new animated pose and clearing
velocity prevents a seek, loop wrap, hard state switch, or teleport from being
misread as physical motion.

That is safe but incomplete for a seek. A dynamic skirt or hair body at clip
time `t` depends on the fixed-step simulation that preceded `t`; normal
playback therefore reaches a different state than "reseat at `t`, velocity
zero". ADR 0096 explicitly left reconstruction by physics pre-roll out of
scope.

The discontinuity flag also erased information needed to choose correctly:
`Animator::seek`, a loop wrap, and a zero-duration state switch all reached
MMD physics as the same boolean. A loop has no unique preceding lap, and an
instant switch has no preceding interval of its target clip, so neither should
silently acquire seek semantics. A positive-duration crossfade is continuous
and must remain ordinary simulation.

A fixed rule such as "start 30 VMD frames earlier" is not defensible. The
amount of history a solver retains is a property of the simulated bodies,
their authored damping, and the fixed timestep. It is also undesirable to pay
pre-roll cost on every normal frame or to route MMD cosmetic simulation
through the gameplay physics world.

## Decision

### 1. Preserve the reason for an animation discontinuity

The runtime keeps seek distinct from a generic repositioning internally.

`Animator::seek` declares an animation seek. Loop wrapping and a
non-positive-duration `crossfade_to` declare a generic repositioning. A
positive-duration crossfade declares neither. A normal loop wrap with no pending seek is a generic repositioning. An
explicit seek remains a seek even when `advance_time` normalizes its target
across a loop boundary: normal playback reseats at the wrap, so the local
post-wrap interval from clip start to the seek target is reconstructible.
A generic repositioning already pending in the same fixed interval still wins
over a later seek.

`RigPose::mark_discontinuous` remains the public generic-reposition API.
RigPose carries the more specific seek reason internally for the MMD modifier.
No serialized or public API changes are introduced.

### 2. Pre-roll belongs to the isolated MMD stateful modifier

Only `mmd_rigid_body_physics_system` reconstructs secondary-motion history.
It replaces that character's isolated Rapier world with a fresh world seeded
from an earlier animation sample, then advances the world to the seek target.

Animation evaluation remains owned by the pure pose layer. MMD physics asks
`PoseGraphOutput::sample_into` for historical `AnimationClip` samples and
temporarily evaluates those samples through `RigPose`'s Animation stage. It
does not advance the live `Animator`, fire animation events, extract root
motion, run the Animation Graph, run gameplay systems, or publish historical
joint Transforms.

Physics owns the integration cadence. Every historical solver step uses the
current `FixedTime::fixed_delta`. The corresponding clip-time increment is
`fixed_delta * playback_speed`. The final historical sample is always the
actual seek target, so VMD's import sample rate is not repurposed as a physics
timestep.

### 3. The pre-roll horizon is derived from solver memory

For each solver-controlled body, use the same PMX-to-Rapier damping conversion
defined by ADR 0108 and Rapier's actual discrete fixed-step retention:

`retention = 1 / (1 + fixed_delta * damping_rate)`.

The slowest linear or angular retention across simulated bodies defines the
history horizon. Pre-roll starts far enough back that velocity carried from
before the start contributes less than one percent at the target. The
one-percent tolerance is the existing ADR 0108 convention used for the
"fully damped" PMX case; it is not a VMD-frame count.

If any simulated degree of freedom has zero damping, there is no finite
forgetting horizon and pre-roll starts at the beginning of the clip. If the
seek target is earlier than the derived horizon, it likewise starts at the
clip beginning.

Consequently ordinary highly damped MMD rigs only simulate the physical
history they can still remember. There is no pre-roll on continuous playback.

The isolated world also records a runtime-only history cursor when its current
state corresponds to one plain clip/time/playback-speed. A later forward seek in the same clip
continues that already-correct solver when the gap is no larger than rebuilding
the damping horizon; otherwise it rebuilds from the derived start. This makes a
monotonic editor scrub advance only the newly crossed fixed steps. Loop wraps,
crossfades, generic discontinuities, and unsupported procedural inputs clear the
cursor. An actually undamped rig can still require full history on a backward
or uncached seek because no shorter history is physically equivalent.

### 4. Fall back to the existing safe reseat when history is not reconstructible

Clip-only pre-roll is used only when the historical input can be reproduced
without running other stateful/world-aware systems. It falls back to ADR
0096's reseat-and-zero behavior when:

- the animator is not actively playing;
- a crossfade is active;
- root motion is enabled;
- the clip animates the character entity itself rather than only rig bones;
- a procedural RigPose layer is active; or
- the clip/animator data needed for historical sampling is unavailable.

Imported VMD IK does not trigger this fallback because ADR 0097 bakes MMD IK
and appended-parent evaluation into the resulting `AnimationClip`.

The character's current root placement is the frame of reference for the
clip-local reconstruction. Teleports remain generic repositionings and never
request seek pre-roll.

### 5. Isolation and replay contracts do not change

The reconstructed world is still the per-character MMD Rapier world from ADR
0096. It never enters gameplay collision events, layers, hit tests, character
controller queries, or gameplay rigid-body integration.

ADR 0064 remains unchanged. Replay continues to record fixed-step input rather
than Rapier internal state. Seek pre-roll is deterministic with respect to the
same clip, rig, fixed step, and solver implementation; this ADR does not add a
cross-platform bit-exact physics promise.

## Consequences

- Seeking to a VMD time reconstructs hair/skirt position and velocity from
  preceding fixed steps instead of presenting a zero-velocity reseat as the
  final state.
- Loop wraps and instant state switches retain their existing safe semantics.
- Crossfades remain continuous and do not trigger pre-roll.
- The hot continuous-play path performs no historical sampling.
- The cost of a seek is tied to the rig's physical memory. Very weakly damped
  or undamped rigs legitimately cost more to reconstruct.
- Historical sampling reuses `PoseArena` storage after the first sample and
  never writes historical poses to ECS joint Transforms.
- Forward seeks can reuse the current isolated solver through a runtime-only
  clip/time/playback-speed cursor; no Rapier snapshot is serialized or exposed publicly.
- No authoring schema, serialized asset, StableId, command semantics, public
  API, crate boundary, or gameplay-physics contract changes.

## Alternatives Considered

**Fixed N-frame pre-roll** — rejected. The required history changes with
damping, fixed timestep, and playback speed; an arbitrary VMD-frame count
under-simulates one rig and wastes work on another.

**Run the whole runtime fixed schedule from an earlier time** — rejected. It
would mix animation seeking with gameplay simulation, events, root motion,
world queries, and replay semantics, and would violate the isolated
secondary-motion boundary.

**Always replay from clip start** — correct for undamped state but rejected as
the default because strongly damped MMD rigs have already forgotten most of
that history, making editor seeks unnecessarily expensive.

**Treat loop wrap and hard state switches as seeks** — rejected. Neither has a
unique preceding interval in the target clip. Their existing reseat semantics
are the only unambiguous state transition.

**Cache or serialize Rapier snapshots in authored assets** — rejected for this
change. Solver state is runtime-only by ADR 0096, and persisting it would
couple authored content to Rapier internals and alter replay/save contracts.

## Compatibility and Migration

No migration is required. Existing scenes, manifests, Animation Sets,
Animation Graphs, VMD-derived `AnimationClip`s, `RigidBodyRigAsset`s, and
replays keep their current formats.

The behavioral change is limited to reconstructible `Animator::seek` calls on
entities that also opt into MMD rigid-body physics. Unsupported seek contexts
continue to use the previous reseat-and-zero fallback.
