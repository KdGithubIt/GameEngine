# ADR 0147: Detached Local AI Studio Frontend and Versioned Host Protocol

Status: Accepted
Date: 2026-08-17
Builds on: ADR 0131
Relates to: ADR 0117, ADR 0133, ADR 0135, ADR 0139

## Context

ADR 0131 states that the embedded Editor panel is only one presentation of AI Studio and requires the first local presentation extension to support detaching into a separate native OS window/viewport without creating a second Agent Host. It also permits a later same-machine separate-process or loopback local-web frontend through a versioned authenticated local protocol.

The current AI Studio presentation is an Editor-owned egui window. A stronger separation improves workspace usability and allows AI conversation/progress to remain visible without giving a new frontend project-authoring ownership.

## Decision

GameEngine introduces two compatible local presentation tiers over the same project Agent Host.

### Tier 1: detached native viewport/window

The first implementation uses the existing Editor process and Agent Host but presents AI Studio in an independent native OS window/viewport. Detach, close, reopen, and reattach are presentation operations only. They MUST NOT create a new session, provider runtime, writer, permission broker, code workspace, or GPU resource broker.

Closing the detached window does not cancel an active run.

### Tier 2: same-machine frontend protocol

A later separate process or loopback local-web client may connect to the same Agent Host through a versioned authenticated local frontend protocol. The protocol exposes frontend-oriented session/run snapshots, conversation/proposal actions, permission decisions, semantic events, and presentation-safe artifacts.

It does not expose raw MCP, arbitrary filesystem/shell/Git execution, provider credentials, or a second mutable project working copy.

## Local authentication

Separate-process access uses ephemeral/revocable user-private credentials and loopback-only transport by default. Endpoint/credential state is application data outside canonical project files and project-shared AI history. Remote reachability remains ADR 0133 rather than being obtained by widening the local bind address.

## State ownership

Agent Host remains authoritative for sessions/runs. ADR 0139 remains authoritative for Editor working copies. ADR 0135 remains authoritative for native inference resource state. The frontend may display these states and request host actions but cannot own or mutate their underlying policy independently.

## UX continuity

Embedded, detached, and later same-machine clients should preserve the same conversation, proposal version, active run, pending permissions/questions, validation/playtest state, completion report, and audit history. Presentation-specific layout/scroll/draft state may remain local client state.

## Dependencies and parallel work

This ADR can be implemented in parallel with ADR 0141-0146, ADR 0148-0149, ADR 0151, and ADR 0153. It does not require Remote AI Studio changes.

## Verification

Implementation must prove one Agent Host across detach/reattach, run survival when a local presentation closes, identical proposal/run identity across views, no duplicate project writer, no credential persistence, and preserved ADR 0135 interrupt/resource semantics.

Both detached-window and separate-frontend UI work require Visual Validation.

## Non-goals

This ADR does not make AI Studio remotely reachable, define mobile clients, move project authoring out of the Editor, or expose raw native GPU controls to the frontend.
