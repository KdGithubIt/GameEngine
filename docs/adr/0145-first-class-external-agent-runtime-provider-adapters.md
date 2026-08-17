# ADR 0145: First-Class External Agent Runtime Provider Adapters

Status: Proposed
Date: 2026-08-17
Builds on: ADR 0131
Relates to: ADR 0121, ADR 0133, ADR 0153

## Context

ADR 0131 already supports a generic external-process Agent Runtime with ephemeral Editor MCP connection injection. That boundary is intentionally provider-neutral, but a generic process launcher cannot expose provider-specific login state, capabilities, structured progress, cancellation behavior, resumability, or diagnostics for products such as Claude Code, Codex, and future compatible coding agents.

First-class adapters are useful only if they remain clients of the existing Agent Host rather than creating vendor-specific AI Studio semantics.

## Decision

GameEngine may provide dedicated `ExternalAgentRuntime` adapters for supported coding-agent providers. Each adapter maps provider lifecycle and protocol behavior into existing `AgentEvent`, run-state, permission, cancellation, and diagnostic contracts.

Adapters may own:

- provider executable/session discovery;
- provider-managed login/authentication status;
- supported capability detection;
- provider command/protocol construction;
- MCP endpoint/credential injection at process launch;
- structured provider event translation;
- graceful cancellation/termination; and
- provider-specific retry/diagnostic classification.

Adapters MUST NOT own project authoring semantics, bypass the code workspace, commit Git history automatically, define separate permission scopes, or mark host completion gates successful.

## Authentication

When a provider legitimately manages subscription/account authentication, GameEngine does not require or copy an API key. Provider-managed credentials remain provider-owned. GameEngine stores only non-secret adapter configuration and provider status needed for UX.

## MCP and workspace boundary

The active Editor MCP endpoint remains loopback-only and project-scoped. Ephemeral MCP connection material is injected only into the local provider process that needs it and is not persisted to project/shared AI records or forwarded to Remote AI Studio. Source edits continue through the Agent Code Workspace unless an explicitly elevated escape-hatch operation is approved.

## Provider events

Raw stdout/stderr may be retained for local diagnostics but is not the sole UI truth. The adapter should translate provider progress into semantic events when the provider exposes enough structure. Unknown provider messages remain diagnostics instead of inventing semantic success.

## Dependencies and parallel work

This ADR can be implemented in parallel with ADR 0141-0144 and ADR 0146-0149. ADR 0153 may later add provider/OS sandbox capabilities around these processes but is not required for first-class adapter semantics.

## Verification

Each supported adapter must prove provider discovery/auth status, MCP injection without persistence, cancellation, process failure mapping, sanitized remote status, no bypass of host permissions/completion, and compatibility with the generic external runtime fallback.

Provider selection/auth UI requires Editor Visual Validation.

## Non-goals

This ADR does not treat external coding agents as `ModelBackend`, assume one authentication method for all providers, or guarantee OS confinement merely because an adapter is first-class.
