# ADR 0165: External Agent and MCP Reliability Boundaries

Status: Accepted
Date: 2026-08-21
Builds on: ADR 0121, ADR 0131, ADR 0145, ADR 0152, ADR 0160, ADR 0163
Supersedes: ADR 0163 sections 1 and 4 where this decision differs

## Context

The external provider path crossed four independently evolving protocols: a
provider CLI stream, GameEngine semantic events, MCP HTTP, and the Agent Host
run lifecycle. Truncated provider lines, unbounded version drift, an Editor
thread timeout, and credentials not bound to a run allowed one layer to succeed
while another layer reported failure. Restart could also retain a run whose
child process no longer existed.

## Decision

1. Provider JSON is retained intact until parsing. Invalid or unknown provider
   JSON becomes an explicit protocol diagnostic instead of an empty event.
   Prompts include the complete GameEngine event schema and the exact completion
   gate names; an environment variable is only a duplicate machine-readable
   copy.
2. Claude Code and Codex installers use exact versions validated with their
   adapters. Discovery accepts only those versions, all readiness probes have a
   finite timeout, and unreadable authentication output fails closed. Updating
   a version requires updating and testing arguments, parser shapes, MCP
   behavior, and authentication probing together.
3. The Editor MCP transport supports the current `2026-07-28` protocol and the
   legacy `2025-11-25` initialize flow during migration. Request bodies may be
   up to 64 MiB. An accepted Editor request has no fixed 30-second host timeout:
   it either receives the authoritative Editor result or fails because the host
   disconnects. The Editor executes at most one queued MCP authoring request per
   frame.
4. The Editor creates separate read-write and read-only MCP credentials. Ask
   receives only the read-only credential, whose advertised inventory omits
   mutating tools and whose transport rejects a mutation even if requested.
   This lets Ask inspect unsaved authoritative Editor state without an
   authoring claim. Unrelated user MCP servers remain excluded by provider
   launch configuration.
5. Every first-class Build provider sends its `GAMEENGINE_AGENT_RUN_ID` on each
   MCP request. The Editor accepts that context only while the matching external
   run is active. A mutating MCP call acquires `canonical_authoring` only for the
   duration of that call, records the actual Editor result in Agent Host, and
   releases the claim immediately. A code-only external Build never preclaims
   the whole authoring surface.
6. Any nonterminal persisted run is failed on Editor restart, its running
   validation is marked interrupted, and every claim is released. Proposal,
   workspace, audit, and event evidence remain available for a new run, but an
   in-process child is never represented as resumed.
7. Cancellation terminates the provider process tree on Windows. Interactive
   sign-in receives a piped input channel exposed by the settings UI. Claude
   Build denies tools that could require an unanswered permission prompt in its
   non-interactive launch.
8. WSL preserves existing `WSLENV` entries and proves reachability with an
   authenticated MCP handshake when `curl` is available. Absence of `curl` is
   not treated as proof that MCP is unreachable. Codex Windows Native keeps the
   `workspace-write` sandbox, requests the supported elevated Windows sandbox,
   and reports a specific failure when the provider claims a file change but
   the isolated workspace remains unchanged.
9. Managed code apply supports file deletion and rename pairs. It stale-checks
   every destination before mutation and rolls live files and baseline state
   back when a write, delete, or baseline persistence step fails.
10. A project permission denial is persisted as `ProjectPermission::Deny`.
    Composer Build submission also derives a conservative minimum structured
    proposal from the submitted text instead of changing only `goal`.

## Consequences

- MCP mutation success is Host evidence and can satisfy authoring validation;
  it no longer depends on the provider repeating that success in a second
  protocol.
- A timed-out HTTP caller cannot leave a queued mutation that applies later.
- Residual provider children cannot use a run-bound credential after the run
  has ended, and Ask cannot mutate even if provider configuration drifts.
- Provider updates are deliberate compatibility changes rather than automatic
  production changes.

## Verification

- Parse provider events whose JSON line exceeds 4,000 characters and report
  malformed JSON explicitly.
- Verify read-only MCP inventory and mutation rejection, modern discovery
  metadata, legacy initialize compatibility, and run-ID forwarding.
- Reopen persisted nonterminal runs and verify failure plus empty claims.
- Apply a managed rename as one create/delete change set.
- Persist and reload a project denial.
- Run targeted Editor and MCP host tests, followed by the repository core gate.
