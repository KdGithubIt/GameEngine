# ADR 0153: Agent Process Confinement and OS/Provider Sandbox Integration

Status: Accepted
Date: 2026-08-17
Builds on: ADR 0131
Relates to: ADR 0145

## Context

ADR 0131 explicitly distinguishes GameEngine application permissions from operating-system confinement. An external agent process running with the user's normal OS identity may access resources outside GameEngine even when AI Studio policy would have denied the equivalent managed capability. The first release correctly avoids claiming a universal sandbox, but future provider/OS integration can strengthen the boundary where the platform supports it.

## Decision

GameEngine may integrate real provider or operating-system confinement as an optional capability of an external Agent Runtime adapter. Confinement is defense in depth above the existing permission broker; it does not replace semantic permissions, MCP transactions, code-workspace confinement, or audit history.

Supported mechanisms may include provider-native sandbox modes, restricted OS tokens, process/job restrictions, filesystem/network policy, containers, VMs, or equivalent platform primitives. The concrete mechanism is adapter/platform-specific.

## Capability truth

An adapter reports a structured confinement profile describing only guarantees it can actually enforce. The UI distinguishes:

- application policy only;
- provider-enforced sandbox;
- OS/container/VM confinement; and
- unavailable/unknown isolation.

GameEngine MUST NOT display a process as sandboxed merely because it was launched through a first-class adapter.

## Minimum invariants

Confinement integration must preserve required access to the approved session code workspace, ephemeral loopback MCP endpoint, and explicitly granted managed resources while denying or restricting broader capabilities according to the selected profile. Provider credentials and MCP credentials remain secret even inside a sandboxed process.

A sandbox failure, unsupported platform, or policy-application error is surfaced before the process is represented as confined. Falling back to an unconstrained process requires explicit policy/UX rather than silent downgrade when the user required confinement.

## Network and filesystem policy

When the platform can enforce them, sandbox network/filesystem scopes should derive from ADR 0131 capabilities instead of inventing a second unrelated permission vocabulary. However, application permission decisions remain authoritative for GameEngine services even when OS enforcement is unavailable.

## Dependencies and parallel work

This ADR can be implemented in parallel with ADR 0141-0149 and ADR 0151. First-class adapters from ADR 0145 are a natural integration point but the confinement contract may be developed against the existing generic process runtime first.

## Implementation

The first-release Agent Host now defines a provider-neutral confinement request and profile at the existing external-process launch boundary. The generic process runtime reports **Application policy only** and never claims provider/OS sandboxing. A user may instead require provider/OS confinement; the generic runtime then fails closed before process spawn rather than silently downgrading.

The transient launch request carries only the approved session code-workspace root, the loopback MCP endpoint, the selected requirement, and a direct-network policy projection derived from the existing `NetworkAccess` capability. It is not serialized. `ExternalAssetAcquisition` remains a governed GameEngine/MCP capability and therefore does not silently widen ambient child-process network access. The persisted run/audit record contains only the non-secret confinement profile and its truthful guarantee levels. MCP authorization material remains process-environment-only.

The launch boundary exposes a confinement-provider seam so first-class adapters or platform integrations can own the real process creation mechanism. A profile satisfies the strict requirement only when it reports provider, OS, container, or VM confinement with enforced filesystem, network, and process-tree guarantees. Unsupported generic launches report unavailable rather than simulated confinement.

AI Studio persists the user's external-process confinement requirement as local application preferences and displays the actual run profile separately from GameEngine application permissions. This applies consistently to the embedded and detached ADR0147 presentations. The generic path also rejects non-loopback MCP endpoints and a launch working directory that differs from the approved code workspace.

## Verification

Platform-specific tests must prove the stated restriction profile, safe failure/downgrade behavior, required MCP/workspace access, denied out-of-scope access where testable, no secret serialization, cancellation/process cleanup, and accurate UI/audit reporting. Unsupported platforms must report unavailable rather than pass simulated confinement.

Security/confinement status UI requires Editor Visual Validation.

## Non-goals

This ADR does not promise one portable universal sandbox, secure arbitrary third-party binaries against all attacks, replace OS security updates, or weaken GameEngine's existing application permission checks.
