# ADR 0126: Timeline / Sequencer Authoring and Runtime

Status: Proposed
Date: 2026-08-16
Builds on: ADR 0052, ADR 0072, ADR 0082, ADR 0103, ADR 0107, ADR 0113, ADR 0116, ADR 0121
Relates to: ADR 0122, ADR 0125

## Context

GameEngine has strong individual runtime domains for animation, cameras,
audio, VFX/particles, UI/events, scene flow, and MMD content. What it lacks is a
single authorable time-domain coordinator for cutscenes, scripted entrances,
camera cuts, animation sequences, synchronized audio/effects, and similar
content.

Without a Timeline/Sequencer, projects must encode sequencing in project Rust,
Behavior Trees, animation state machines, or ad-hoc timers. Those systems remain
valuable for gameplay decisions, but they are poor authoring surfaces for a
content creator who needs to see "camera cut at 3.0 s, animation at 3.2 s,
effect at 4.1 s, event at 5.0 s" on one shared time axis.

A naive Timeline can also damage architecture: if one runtime crate directly
depends on animation, audio, rendering, scene, and gameplay, it becomes a
cross-domain dependency hub. Editor-only playback logic would also drift from
packaged runtime. The design therefore needs a neutral timeline core plus
domain adapters at the composition boundary.

## Decision

### 1. Timeline is a versioned authoring asset with stable sub-object identity

Introduce a canonical `*.timeline.json` `TimelineDocument` in the authoring
domain. It contains:

- one stable timeline asset/document ID;
- tracks with stable track IDs, type IDs, name, enabled state, and ordering;
- clips with stable clip IDs, start/end ticks, blend/easing metadata where the
  track supports it, and typed payload;
- markers/events with stable IDs; and
- bindings to authoring entities/assets through `EntityRef`/`AssetRef` or
  equivalent stable authoring references.

Runtime ECS entity IDs, animation handles, audio voice IDs, GPU handles, and
Editor selection state are never serialized.

### 2. Use integer timeline ticks as canonical time

Timeline authoring/evaluation uses a fixed integer tick rate of 48,000 ticks per
second. `TimelineTick` is an integer type; clip boundaries and markers are
stored as ticks rather than `f32` seconds.

The Editor displays time as seconds, frames, or timecode. Frame snapping uses a
separate rational display frame rate, so 24/25/30/60 and NTSC-style rates can be
represented without redefining persisted timeline time.

This avoids float drift, makes event-edge semantics exact, and gives audio and
animation adapters a common conversion point. Runtime playback rate may be
floating point, but accumulation converts through a residual/fixed tick clock
rather than repeatedly storing clip boundaries in float seconds.

### 3. Separate immutable compiled schedule from per-player state

The authoring document validates/compiles to an immutable `CompiledTimeline`
with deterministic track/clip order and pre-resolved stable payload data.

Each playing entity owns a transient `TimelinePlayer` instance containing:

- current tick and previous evaluated tick;
- play/pause/stopped state;
- playback rate/direction policy;
- loop region/count where enabled;
- evaluation generation; and
- adapter-owned transient state tokens needed between evaluations.

Many players may share one compiled timeline without sharing playback state.

### 4. Introduce a neutral timeline runtime core instead of a dependency hub

Timeline scheduling/time semantics are a coherent reusable domain and may live
in a small `engine-timeline` crate if implementation confirms it prevents
cross-domain dependency cycles. That crate owns only neutral timeline runtime
contracts: tick math, compiled schedule traversal, player state, evaluation
requests/results, and adapter traits/IDs.

It MUST NOT depend directly on platform audio, render-runtime VFX, animation,
camera, physics, or Editor GUI implementations merely to evaluate time.

Concrete track adapters that need several runtime domains are registered/applied
at the top-level `engine` composition layer, consistent with ADR 0113. If the
neutral core fits cleanly in an existing owner without widening its dependency
role, implementation may avoid the new crate, but it may not place a
cross-domain dependency fan-in into a lower runtime crate for convenience.

Any new crate must be added to affected-package CI classification in the same
implementation series.

### 5. Track types use an extensible registry and domain-owned payloads

The timeline core identifies tracks by stable type ID and evaluates deterministic
clip ranges. A registry connects each supported track type to:

- authoring schema/validation;
- compile payload conversion;
- runtime evaluator/apply adapter;
- seek capability/policy; and
- Editor presentation metadata.

The initial track families are:

- Animation: select/play Animation Set motion slots or approved controller
  targets; it does not reintroduce raw importer clip ownership.
- Camera Cut: select an authored game camera for a clip interval through a
  transient timeline override that composes with ADR 0107 instead of rewriting
  camera priorities in the scene.
- Transform/Property: animate an explicitly supported typed property set with
  curves; arbitrary reflection into all ECS memory is not allowed.
- Audio: start/stop/fade music or sound cues through engine-owned audio commands;
  spatial cues use ADR 0122 semantics when a target binding is present.
- VFX: start/stop/restart a VFX player/effect through ADR 0125's runtime
  interface.
- Event: emit a stable sequence-level event for project gameplay/UI/scene
  orchestration.

Morph/property-specific adapters may be added when their ownership is clear.
A track cannot reach into another domain through untyped JSON or raw component
pointers.

### 6. Animation clip events and Timeline events remain distinct

ADR 0116 remains authoritative: clip-local animation events belong to Animation
Set bindings because they travel with that motion slot wherever it is played.

Timeline marker/Event tracks represent sequence-level events tied to the
cutscene/sequence itself. The Timeline does not copy animation events out of
Animation Sets, and the Animation Set editor does not become a cutscene editor.
This distinction prevents duplicated event rows with ambiguous ownership.

### 7. Seeking is an explicit per-track capability

The Timeline core supports `evaluate_at(tick)`/seek, but each track adapter must
declare how it handles discontinuous time:

- `Stateless`: result is a pure function of the target tick (camera cut,
  transform curve, many property tracks).
- `Seekable`: the target domain exposes a deterministic seek/sample API
  (animation sampling, suitable audio cursor operations where supported).
- `ReplayRequired`: state must be reconstructed from an earlier checkpoint or
  start and simulated forward (some VFX/secondary-motion cases).
- `NonSeekable`: Editor clearly marks the limitation and does not fabricate a
  result.

The Sequencer coordinates these policies. It MUST NOT run physics or VFX with a
negative delta to fake reverse playback. Domain-specific pre-roll/checkpoint
behavior remains owned by the domain adapter.

### 8. Event firing has exact crossing semantics

During normal forward playback, a marker fires exactly once when the playhead
crosses from a tick before the marker to the marker tick or beyond. Loop
boundaries define explicit crossing intervals so a marker is not accidentally
lost or fired twice.

Manual Editor seek/scrub samples visual state but suppresses gameplay Event-track
side effects by default. An explicit "Preview Events" mode may opt into event
preview for authoring, clearly separated from normal scrub.

Reverse playback and event firing are not implicitly symmetric; if reverse
event semantics are needed they must be specified per marker/track policy.

### 9. Camera cuts override selection transiently without mutating authoring

ADR 0107's enabled/priority fields remain the ordinary camera selection policy.
A playing Timeline may install a runtime-only camera-selection override scoped
to that Timeline player/track interval.

The override references the target camera resolved from stable authoring
identity and disappears on stop, clip exit, player removal, or binding failure.
It never edits persisted camera priority/enabled values and never changes the
Scene View camera defined by ADR 0103.

Conflicting active Timeline camera overrides are resolved by explicit player/
track priority in the Timeline runtime, with a deterministic tie diagnostic;
query order is never authoring intent.

### 10. The Sequencer is a dedicated Editor workspace over shared authoring commands

Opening a Timeline asset presents a Sequencer with:

- hierarchical track list and lane area;
- playhead, current time/frame display, zoom, pan, and horizontal scrollbar;
- drag/drop clip creation from compatible assets/entities;
- trim/move/duplicate/delete and multi-selection;
- snapping to frames, markers, clip edges, and playhead with visible snap
  feedback;
- track mute/solo/lock presentation controls, with a clear distinction between
  persisted enabled state and transient Editor-only view controls;
- searchable Add Track menu driven by the shared track registry;
- Inspector for selected track/clip/marker properties;
- curve editor for track types that expose animatable numeric properties;
- marker/event lane;
- binding diagnostics and one-click focus/select of the bound scene entity;
- Play/Pause/Stop/Step and loop-region preview controls; and
- inline indications when a track is ReplayRequired/NonSeekable during scrub.

All persisted edits are granular `TimelineCommand`s applied transactionally
through a GUI-free Timeline authoring service. Undo/redo, CLI, MCP, tests, and
the Editor share the same validation and mutation semantics per ADR 0121.

### 11. Preview runs against the real persistent preview world

The Sequencer evaluates targets in the existing persistent Scene View preview
world from ADR 0072, or in Play when explicitly attached to a running player.
It does not build a separate simplified camera/animation/VFX simulator.

Editor preview owns an explicit preview clock and transient adapter tokens.
Changing one clip or property recompiles/invalidates only the Timeline preview
state and affected domain state where practical; it must not force unrelated
asset reimports or full scene rebuilds on every playhead drag.

Heavy ReplayRequired reconstruction is debounced/cancellable and may use
transient checkpoints so scrubbing remains responsive under ADR 0104.

### 12. Bindings fail visibly and never retarget by display name

Entity bindings use stable authoring identity. Asset bindings use `AssetId`.
Renaming a camera/entity/asset does not break a valid binding. Deleting or
replacing the target produces a stable diagnostic on the track/clip; the engine
does not search for a same-name replacement.

Prefab/scene instantiation binding semantics must be explicit before Timeline
assets are embedded/reused across prefabs. The initial implementation may limit
a Timeline to bindings resolvable in its owning scene rather than invent weak
name-based rebinding.

### 13. Project gameplay controls Timeline through deferred commands and copied views

Project Rust can start, pause, resume, stop, seek where allowed, set playback
rate, and query copied Timeline state through an additive command/view family
under ADR 0052. It never receives a mutable `TimelinePlayer` or domain adapter
reference.

Timeline Event tracks enter the normal bounded host event path with stable event
IDs and source Timeline identity. Their ordering relative to other deferred
commands is explicitly scheduled and tested.

### 14. Implementation is staged around the neutral contract

Implementation order is:

1. Timeline document, stable IDs, tick math, commands/transactions, validation,
   and compiled schedule;
2. neutral player/evaluation core plus Event and simple stateless test tracks;
3. Animation and Camera Cut adapters, including seek and transient camera
   override semantics;
4. Audio and VFX adapters after their stable runtime controls from ADR 0122 and
   ADR 0125 are available;
5. Sequencer basic track/lane editing and persistent Scene View preview;
6. snapping, curve editing, markers, binding UX, and ReplayRequired checkpoints;
   and
7. project-Rust commands/views plus packaged-player integration proving project.

A full recording/keyframe-capture workflow, nested Timelines, nonlinear audio
mixing, and cinematic render/export are future extensions. The data model uses
stable IDs so those features do not require replacing basic track/clip identity.

## Verification

The accepted implementation must cover at least:

- exact integer clip/marker boundary behavior across variable frame deltas;
- looping without missing or double-firing markers;
- manual scrub suppressing gameplay events by default;
- deterministic track ordering for overlapping clips;
- two TimelinePlayer instances sharing one compiled Timeline without state
  leakage;
- target rename retaining stable bindings and target deletion producing a
  diagnostic instead of name retargeting;
- camera override entering/exiting without mutating persisted camera settings;
- Animation track seeking matching direct animation sampling;
- ReplayRequired adapter reconstruction producing the same state as forward
  playback from the same checkpoint/start;
- Editor commands/undo/save/reopen preserving exact tick positions and IDs;
- Editor/MCP/CLI validation equivalence; and
- packaged Player producing the same cross-domain ordering as Editor Play.

The Sequencer, curve editor, camera cuts, and scene preview require Visual
Validation when implemented. Scrub latency should be measured with animation,
VFX, and camera tracks active together.

## Consequences

Cutscenes and scripted sequences become authorable as content instead of being
hidden in gameplay timers/code. Integer time and stable IDs make sequencing
precise and diffable. A neutral evaluator plus composition-layer adapters
avoids turning Timeline into a crate that owns every engine domain.

The engine gains a substantial new authoring surface and cross-domain runtime
coordination. That complexity is controlled through strict adapter contracts,
seek policies, and an implementation order that proves time/event semantics
before the full Sequencer UI lands.

## Alternatives Considered

### Put sequencing into Behavior Trees

Rejected. Behavior Trees model decision/control flow, not dense authored time.
They remain appropriate for gameplay decisions that may start/stop a Timeline.

### Put all sequencing into Animation Graphs

Rejected. Animation Graphs own pose/motion state and transitions. Camera cuts,
audio, VFX, scene events, and exact cutscene markers should not become animation
state-machine responsibilities.

### Let the Timeline crate directly depend on every target runtime domain

Rejected. It would create a dependency hub and make lightweight timeline tests
compile audio/GPU/animation backends unnecessarily. Neutral scheduling plus
composition adapters follows ADR 0113/0114.

### Store time as `f32` seconds

Rejected. Long sequences and marker/loop edge behavior should not depend on
floating-point equality. Integer ticks make persisted boundaries exact.

### Implement Sequencer preview as an Editor-only simulator

Rejected. It would drift from packaged playback. The Editor supplies preview
time and caching; domain evaluation remains the runtime implementation.

## Compatibility and Migration

`*.timeline.json`, Timeline track/clip IDs, and any `engine.timeline_player`
component introduced by implementation are new current-format contracts. No
existing scene or animation asset needs automatic migration merely because the
Timeline feature exists.

ADR 0116 remains unchanged: Animation Set binding events stay motion-local and
are not migrated into Timeline markers. ADR 0107 camera enabled/priority data
also stays unchanged because Timeline camera selection is a transient override.

If a new `engine-timeline` crate is created, it must follow ADR 0113/0114
one-way dependency and backend-isolation rules and update CI changed-path
classification in the same implementation series. The top-level `engine`
facade re-exports the supported public Timeline API.
