# ADR 0144: Hosted and Enterprise ModelBackends and Credential Ownership

Status: Proposed
Date: 2026-08-17
Builds on: ADR 0131
Relates to: ADR 0141, ADR 0142, ADR 0150

## Context

ADR 0131 separates an engine-owned native Agent Runtime from the `ModelBackend` that performs inference. The current implementation only provides a loopback local-model backend. Direct hosted APIs and future enterprise inference gateways are planned but must not collapse into the separate external Agent Runtime abstraction used by coding-agent processes.

Hosted integration also introduces credentials, network permission, rate limits, provider errors, usage accounting, and data-boundary concerns that do not exist for a loopback local runtime.

## Decision

GameEngine may add hosted and enterprise `ModelBackend` adapters behind the same native Agent harness. These adapters provide inference only; they do not own AgentRun lifecycle, MCP authoring, code mutation, permissions, validation, Play, or completion.

Conceptually:

```text
NativeAgentRuntime
  -> ModelBackend
       -> Local backend
       -> Hosted API backend
       -> Enterprise-managed backend
```

A provider's API shape MUST NOT leak into the provider-independent Agent Host contract. Provider-specific request/response mapping, streaming, retry classification, usage metadata, and authentication live in the adapter.

## Credential ownership

GameEngine-owned API credentials must be stored in an OS credential facility or equivalent secure user secret store. They MUST NOT be written to canonical project files, project-shared AI history, logs, Remote AI Studio payloads, benchmark fixtures, or provider-independent session serialization.

Provider-managed login/session flows may retain credentials in the provider's own approved storage. Enterprise-managed authentication may use an organization-provided credential mechanism without forcing an API-key abstraction.

## Network and data policy

Using a hosted backend requires the existing ADR 0131 network capability. AI Studio must make the active backend and remote-processing posture visible. Repository/project context is sent only as selected by the harness for the authorized task; adapters must not silently upload entire projects merely because the backend is remote.

Provider request failures are structured as retryable/non-retryable backend evidence. Rate limiting, authentication expiry, safety refusals, server errors, and transport loss cannot be converted into successful AgentRun completion.

## Capability profile

Hosted adapters report discoverable context, tool/structured-output, image, reasoning, streaming, and usage capabilities. Unsupported features remain unavailable. Backend-specific resource controls for remote GPUs are not represented as local ADR 0135 GPU residency controls.

## Dependencies and parallel work

This ADR can be implemented in parallel with ADR 0141 and other Wave A ADRs because ADR 0131 already defines the ModelBackend boundary. ADR 0150 may later route to hosted models only after ADR 0142 proves a benefit.

## Verification

Implementation must cover secret-storage boundaries, no secret serialization, explicit network permission, provider error mapping, cancellation/stream interruption, capability honesty, context-size handling, sanitized Remote AI Studio status, and unchanged host-owned completion gates.

Credential/backend selection UI requires Editor Visual Validation.

## Non-goals

This ADR does not define provider-specific external coding agents, route between multiple models, or make a hosted provider authoritative for project mutation.
