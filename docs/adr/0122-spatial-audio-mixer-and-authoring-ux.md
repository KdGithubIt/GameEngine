# ADR 0122: Spatial Audio Mixer and Authoring UX

Status: Proposed
Date: 2026-08-16
Builds on: ADR 0052, ADR 0054, ADR 0113, ADR 0114, ADR 0121

## Context

GameEngine already has a usable desktop audio baseline: decoded audio assets,
SE playback, looping BGM, BGM crossfade, master/BGM/SE volume control, and the
authorable `engine.audio_emitter`, `engine.audio_listener`, and
`engine.music_controller` components. The authoring contract already persists
`volume`, `spatial_blend`, `min_distance`, and `max_distance` on an audio
emitter.

The runtime does not yet honor the spatial part of that contract. Authored
autoplay emitters currently end in the same non-positional `play_se` path as a
2D sound effect. The listener transform, emitter transform, blend, and distance
range do not continuously affect an active voice. A moving enemy therefore can
have an authored 3D emitter while the player still hears an ordinary 2D sound.

Completing spatial audio is more than swapping one `rodio` sink type. The
design must keep platform dependencies isolated, avoid persisting backend voice
handles, support gameplay-triggered sounds as well as autoplay, give authors a
predictable listener-selection rule, and make spatial ranges and attenuation
understandable in the Editor. A backend-specific solution inside the Editor or
scene bridge would make later HRTF, occlusion, additional platforms, or mixer
work unnecessarily invasive.

## Decision

### 1. Separate ECS spatial extraction from the platform audio backend

ADR 0113 remains authoritative: `engine-platform` owns the audio runtime and
native audio backend, while the final `engine` composition layer is allowed to
compose several domains.

`engine-platform` MUST NOT depend on rig/scene transform types merely to read an
emitter position. Instead it owns backend-neutral audio DTOs such as a listener
pose, emitter pose, voice spatial parameters, and runtime-only voice identity.
The `engine` composition layer reads `GlobalTransform`, authored audio
components, and runtime identity, resolves the active listener, and submits
plain numeric spatial state to the platform audio runtime.

The native backend remains behind the `engine-platform` `audio` feature per ADR
0114. No `rodio` type may appear in persisted authoring data, project-game ABI,
or the neutral spatial contract.

### 2. Spatial playback is a voice lifecycle, not a fire-and-forget sink call

The audio runtime gains an opaque runtime-only `AudioVoiceId` and a voice
lifecycle with at least:

- start a one-shot or looping voice from an already-loaded `AudioAsset`;
- update gain and spatial parameters for an active voice;
- stop a voice explicitly; and
- retire completed one-shot voices without leaking runtime state.

An authored `AudioEmitter` that is playing owns only transient playback state
that maps it to a voice. The voice ID is never serialized and is discarded when
the runtime world or audio device is rebuilt.

Moving emitters and listeners update their active voices once per rendered
frame. Audio decoding and device work remain on the audio worker; ECS queries
and transform extraction stay on the engine thread. The worker accepts bounded
commands/state updates rather than engine references.

### 3. Define deterministic engine-owned spatial semantics

The initial spatial model is intentionally simple and testable:

- `spatial_blend = 0` is fully 2D and does not pan or attenuate by distance.
- `spatial_blend = 1` is fully positional.
- intermediate values blend the 2D and positional gains continuously.
- `volume` multiplies the final emitter gain.
- `min_distance` is the distance at which distance attenuation begins.
- `max_distance` is the distance at which the positional contribution reaches
  its configured floor.
- left/right panning is derived from the listener orientation and emitter
  direction with an equal-power stereo pan law.
- distance attenuation is selected by an engine-owned rolloff mode. The first
  modes are `linear` and `inverse`; `linear` remains the default for existing
  authored values.

All calculations are implemented as pure functions independent of the native
audio device and have finite-value/clamping rules. A later HRTF backend may
consume the same listener/emitter poses without changing scene data.

Doppler, environment reverb, portal acoustics, and geometry occlusion are not
part of the first implementation. The voice/spatial DTO deliberately leaves
room for those capabilities so they do not require replacing the playback
ownership model.

### 4. Listener selection is explicit and deterministic

`engine.audio_listener` gains the same authoring intent needed by other
single-selection runtime features: `enabled` plus `priority`.

The engine selects one game listener by:

1. excluding disabled listeners;
2. preferring the greatest priority; and
3. using a deterministic runtime tie-break only as a safety fallback.

Authoring validation warns when multiple enabled listeners share the greatest
priority because runtime entity order is not persisted intent. A scene with
spatial emitters and no enabled listener produces an actionable warning; it
does not silently use the Editor camera in packaged play.

The Editor MAY provide a one-click "Use selected/active game camera as listener"
action, but that action creates or edits the ordinary authoring component
through `AuthoringCommand`. It is not a hidden runtime coupling between Camera
and AudioListener.

### 5. Keep the authoring component simple while making the runtime extensible

`engine.audio_emitter` remains the normal scene-facing component. It gains only
settings that are meaningful to an author and shared by every backend, such as
rolloff mode and whether playback loops. Backend tuning and voice handles remain
runtime-only.

The current MusicController continues to use the non-spatial music path.
Background music is not turned into a spatial emitter merely to share code.
Both paths may share device/mixer infrastructure below their public semantics.

If custom mixer buses are added later, they will use stable engine-owned bus
identifiers rather than exposing backend channel objects. Arbitrary bus graphs
are outside this ADR; the current master/music/effects routing remains valid.

### 6. Gameplay-triggered spatial sounds use the existing deferred-command model

ADR 0052 remains the safety boundary for project Rust. Existing
`PlaySoundEffect` remains a 2D sound request. A new additive spatial playback
request may name a generation-checked source entity plus an audio asset and
playback options. The host resolves the source transform and creates a transient
voice after normal command preflight.

Project code never receives `AudioVoiceId`, a backend sink, or a mutable audio
system reference. Long-lived authored emitters continue to be controlled by
engine-owned runtime state. If later gameplay requires stop/update by handle,
that requires an engine-owned request ID/voice-token contract rather than
leaking backend identity.

### 7. Editor authoring is visual, audible, and uses the same runtime semantics

The AudioEmitter Inspector provides:

- an audio-asset picker filtered by the existing audio category;
- a volume control with numeric entry;
- a clearly labeled 2D <-> 3D spatial-blend control;
- min/max distance controls with validation and sensible defaults;
- rolloff preset selection plus a small read-only attenuation preview;
- autoplay/loop controls; and
- explicit Play/Stop audition controls.

Scene View draws emitter gizmos for the min/max distance shells and a listener
icon/orientation. Selected-emitter gizmos are directly manipulable where that
can map cleanly to the existing component command path; numeric Inspector edits
remain available for precision.

Audition is transient Editor state. It MUST NOT set `autoplay`, modify the
scene, or write project data. By default audition listens from the active game
listener. An explicit "Listen from Scene View" toggle may temporarily use the
Editor camera for audition only, and is clearly labeled so packaged-game
semantics are never implied by the preview.

Preview and Play use the same attenuation/panning functions and voice backend.
The Editor MUST NOT implement a separate approximation of spatial audio merely
for its gizmo or audition UI.

### 8. Validation and failure states are actionable

Shared authoring/conversion validation covers at least:

- non-finite volume/blend/distance values;
- blend outside `[0, 1]`;
- negative distance;
- `min_distance > max_distance`;
- missing or wrong-category audio assets;
- spatial emitters with no enabled listener; and
- ambiguous highest-priority listeners.

Device-unavailable state remains non-fatal for headless validation and produces
an explicit runtime/editor status. A missing audio device must not cause scene
conversion to fabricate a different spatial model.

### 9. Editor responsiveness is a contract

Changing a transform or emitter distance must not decode the audio file again
or restart every unrelated voice. Asset decode remains cached by asset identity,
and voice parameter updates are incremental.

Audition and waveform/metadata work, if added, runs outside the UI-thread edit
path when it requires file I/O. The Inspector must remain responsive according
to ADR 0104.

### 10. Implementation is staged behind stable boundaries

Implementation proceeds in these slices:

1. pure spatial math, neutral pose/voice DTOs, and runtime tests;
2. backend voice lifecycle and per-voice spatial updates;
3. ECS listener selection and emitter-to-voice synchronization;
4. authoring schema/validation plus project-Rust spatial command support;
5. Inspector controls, Scene View gizmos, and audition controls; and
6. packaged-player and Editor-Play integration verification.

Each slice uses the existing component IDs. No Editor-only semantic mutation API
is introduced.

## Verification

The accepted implementation must prove at least:

- equal inputs produce identical pure attenuation/pan results without a device;
- a moving emitter updates an already-playing voice rather than restarting it;
- a moving listener changes the perceived pan of a stationary emitter;
- 2D blend bypasses positional attenuation and full 3D blend applies it;
- min/max/rolloff boundaries are continuous and finite;
- listener priority is deterministic and tie diagnostics are produced;
- headless execution drains/updates audio state without panicking;
- Editor audition does not dirty or mutate the authoring document;
- Inspector/gizmo edits round-trip through ordinary authoring commands; and
- Editor Play and packaged Player use the same spatial computation path.

The Editor UI and gizmos require Visual Validation when implemented. Runtime
audio correctness additionally requires focused automated tests because a
screenshot cannot validate panning or attenuation.

## Consequences

Spatial audio becomes a completed engine capability rather than unused fields
on an authoring component. The backend boundary can later support HRTF or a
different native library without changing scene components. The Editor makes
3D sound ranges visible and auditionable without creating preview-only game
semantics.

The audio runtime becomes more stateful because active voices must survive and
receive updates. The engine composition layer also gains one cross-domain
system to combine transforms with platform audio, which is consistent with ADR
0113's composition responsibility.

## Alternatives Considered

### Read `GlobalTransform` directly from `engine-platform`

Rejected. It would create an unnecessary dependency from the platform domain to
the rig/runtime transform domain and weaken the crate DAG established by ADR
0113.

### Use backend spatial-sink types as the public audio API

Rejected. It would make scene/runtime APIs backend-specific and make alternate
platforms or HRTF replacement a breaking change.

### Automatically use the active game camera whenever no listener exists

Rejected. It is convenient but hides authoring intent and makes audio behavior
change when camera setup changes. The Editor provides a quick authoring action
instead.

### Implement only Editor gizmo visualization now

Rejected. Showing spatial ranges while runtime playback ignores them would
repeat the current mismatch between authorable data and actual behavior.

## Compatibility and Migration

The stable component IDs from ADR 0054 remain unchanged. Implementation may
advance the current `engine.audio_emitter` / `engine.audio_listener` component
schema versions when rolloff, looping, or priority fields are added. Under ADR
0115, the engine updates current in-repository content and fixtures to the new
canonical format rather than maintaining compatibility-only readers for older
engine revisions.

Existing public `engine::*` facade paths remain supported under ADR 0113. New
runtime voice IDs are transient and are never serialized. Existing 2D project
Rust sound commands keep their meaning; spatial playback is additive.
