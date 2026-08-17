# ADR 0149: Engine-Native Live Observation and Media Transport

Status: Proposed
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

## Verification

Implementation must measure end-to-end latency, encoder/readback overhead, renderer impact, reconnect behavior, access control, source identity, captured-frame audit continuity, and correct behavior when the media client disconnects while an AgentRun continues.

Remote/live-view UI requires Visual Validation. Performance/resource validation must accompany renderer/media implementation.

## Non-goals

This ADR does not create a general remote desktop product, provide public NAT traversal by default, replace typed MCP authoring with pixel interaction, or make live video mandatory for Remote AI Studio completion.
