# ADR 0151: Headless Write-Capable MCP Host and Project-Writer Ownership

Status: Proposed
Date: 2026-08-17
Builds on: ADR 0117, ADR 0121
Relates to: ADR 0131, ADR 0139, ADR 0152

## Context

ADR 0121 deliberately makes the initial write-capable MCP endpoint Editor-attached and project-scoped. When no Editor is open, CLI is the supported headless mutation path. A future headless write-capable MCP host is useful for automation servers, non-visual agent work, and environments where starting the full Editor is unnecessary, but it cannot become a second writer beside a live Editor.

ADR 0117 already defines an OS-backed exclusive Editor lease for a canonical project location. ADR 0139 separately defines authoritative in-memory working copies inside a live Editor process. A headless writer needs an application-level ownership contract that composes with both.

## Decision

GameEngine may add a GUI-free **Headless Authoring Host** that exposes the same `engine-mcp` tool semantics only after acquiring explicit project-writer ownership for the canonical project location. It is a host for shared authoring services, not a separate authoring implementation.

## Writer ownership

A project location has one authoritative write host at a time. The ownership mechanism must prevent the headless host from acquiring write authority while an Editor owns the same location. The existing project-lifecycle lease may be generalized or composed with a writer-role lease, but authority must remain OS/process robust rather than metadata-only.

A read-only headless MCP service may coexist when it cannot observe dirty Editor working copies; it must clearly report that its view is saved-file state rather than the live Editor working copy. It MUST NOT claim parity with the live Editor for unsaved authoring state.

## Authoring semantics

The headless host uses the same capability registry, schemas, commands, transactions, validation, path confinement, stale-generation checks, and serialization as Editor MCP/CLI. It does not route through CLI argv/stdout and does not add headless-only mutation rules.

One mutation request remains one bounded transaction unless a later ADR explicitly changes ADR 0121 transaction identity.

## Persistence and working copies

A headless host loads canonical saved project data and owns any in-memory working state it creates for its process. There is no hidden synchronization with an Editor process. Transitioning writer ownership between headless and Editor hosts requires the prior writer to release ownership and persist/resolve dirty state according to its contract before the next writer becomes authoritative.

## Agent Host integration

ADR 0131 Agent Runtime may target a headless host only when the project-writer ownership contract confirms that host is authoritative. Remote AI Studio or a native Agent must not silently start a headless writer beside an Editor to bypass availability or stale-state errors.

## Dependencies and parallel work

This ADR can be implemented in parallel with ADR 0141-0149 and ADR 0153. ADR 0152 depends on the writer-ownership model established here before enabling concurrent AI writers.

## Verification

Implementation must prove mutual exclusion with Editor ownership, crash-safe lease release, identical shared authoring semantics, no MCP-specific direct file replacement, explicit saved-state visibility for read-only coexistence, safe writer handoff, and unchanged canonical serialization/Stable IDs.

Headless-only implementation requires no Visual Validation unless Launcher/Editor ownership/conflict UX is changed.

## Non-goals

This ADR does not introduce concurrent writers, real-time collaboration, cross-call open transactions, or a second project working copy that is magically merged with dirty Editor memory.
