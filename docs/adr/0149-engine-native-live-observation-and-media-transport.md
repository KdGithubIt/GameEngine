# ADR 0149: Engine-Native Live Observation and Media Transport

Status: Accepted
Date: 2026-08-17
Builds on: ADR 0035, ADR 0131, ADR 0133
Relates to: ADR 0003, ADR 0135

## Context

Remote AI Studio first release uses bounded captured frames and visual-evaluation results. ADR 0133 explicitly keeps custom H.264/HEVC encoding, WebRTC, desktop capture, NAT traversal, and mobile video clients outside the first release and recommends existing remote-display software for optional live viewing.

A future engine-native live view is justified only when GameEngine needs semantic integration that external remote display cannot provide, such as run-step-linked low-latency Game View observation, bounded frame provenance, or controlled media delivery to an AI Studio client.

## Decision

If GameEngine adds engine-native live observation, it is an observation/media subsystem above renderer-owned capture boundaries and below AI Studio presentation. It does not become an authoring path or a second runtime input authority.

The media source must identify what is being observed, such as Game View, a specific runtime camera, or a supported Editor viewport. Renderer readback/capture APIs remain owned by renderer/application composition according to ADR 0003. The AI/remote layer consumes bounded frames or encoded samples and does not take ownership of GPU resources.

## Media pipeline

A concrete implementation must define:

```text
renderer-owned observation source
  -> bounded capture/readback
  -> encoder abstraction
  -> authenticated media session
  -> AI Studio client
```

Codec and transport are implementation choices only after their dependency, licensing, latency, platform, and security costs are measured. WebRTC or another low-latency protocol may be used, but signaling/media authentication must remain distinct from raw MCP and provider credentials.

## Completion and evidence

Live video supplements captured-frame evidence. ADR 0131 completion still requires owned frame/visual-evaluation evidence when applicable. A transient video stream does not replace the captured artifact used for audit, reconnect, or deterministic completion reporting.

## Resource arbitration

Live observation competes for renderer/encoder resources. ADR 0135 Play/frame-capture priority remains authoritative. Native inference must yield sufficient GPU budget for required rendering and observation. The remote client never receives raw model/renderer resource controls.

## Security

Media endpoints are private/authenticated by default and must not expose desktop-wide capture unless a separate explicit product decision authorizes that scope. Captured content may contain project/private information; access follows the same remote identity/session policy as Remote AI Studio and is not written to canonical project data.

## Dependencies and parallel work

This ADR can be implemented in parallel with other Wave A ADRs. It does not depend on ADR 0148 remote lifecycle. Integration with a native mobile app remains optional under ADR 0133 and is not required for this ADR.

## First implementation

The first accepted implementation is deliberately narrower than a general video stack:

- `Game View` is the only live source and is reported explicitly as `game_view`.
- The existing renderer-owned `FrameCapture` readback remains the capture boundary.
- A private encoder abstraction currently emits PNG samples, with no new codec or media dependency.
- Samples are bounded to at most 1280x720 and 8 fps. The media manager keeps only the latest encoded sample per session.
- Starting live observation is scoped to the current non-terminal `AgentRun`. A replacement start for the same run rotates the media-session identity and credential instead of accumulating stale sessions.
- Remote control still requires the ADR 0133 bearer credential. Media status/frame/stop requests additionally require a per-media-session token that is never an MCP or provider credential.
- Live sampling is transient. It never calls `AgentHost::store_captured_frame_artifact`, never creates `CapturedFrame` completion evidence, and therefore cannot satisfy or corrupt the ADR 0131 audit gate.
- Every delivered sample records measured renderer readback time, encode time, end-to-end capture time, sample byte size, dimensions, sequence, and aggregate averages/maxima exposed through the authenticated media-session status endpoint.
- A disconnected client does not cancel the `AgentRun`. Reload/reconnect creates a fresh authenticated media session for the same still-active run and invalidates the previous same-run session.
- Desktop-wide capture, arbitrary Editor viewport capture, WebRTC, H.264/HEVC, public NAT traversal, and media-driven authoring/input remain outside this implementation.

This shape preserves a replaceable encoder/transport boundary: a future low-latency codec can replace the PNG encoder without moving GPU ownership, Agent Host lifecycle, or authoring authority into the media layer.

## Verification

Implementation must measure end-to-end latency, encoder/readback overhead, renderer impact, reconnect behavior, access control, source identity, captured-frame audit continuity, and correct behavior when the media client disconnects while an AgentRun continues.

Remote/live-view UI requires Visual Validation. Performance/resource validation must accompany renderer/media implementation.

## Non-goals

This ADR does not create a general remote desktop product, provide public NAT traversal by default, replace typed MCP authoring with pixel interaction, or make live video mandatory for Remote AI Studio completion.
