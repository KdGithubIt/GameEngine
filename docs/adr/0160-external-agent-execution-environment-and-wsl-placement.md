# ADR 0160: External Agent Execution Environment and WSL Placement

Status: Accepted
Date: 2026-08-20
Builds on: ADR 0121, ADR 0131, ADR 0145
Relates to: ADR 0139, ADR 0155

## Context

ADR 0145 adopts first-class provider adapters for external coding-agent
runtimes, and the implementation launches those runtimes as Windows processes:
the adapter resolves `claude` or `codex` on the Windows `PATH`, probes it with
`--version`, and spawns it with the Editor's ephemeral MCP endpoint and bearer
token in its environment.

That assumption does not match how these runtimes are installed on the target
workstation. Both provider CLIs are commonly installed inside a WSL2
distribution rather than on Windows, together with their own sign-in state,
Node runtime, and configuration. A Windows-only launch path reports such an
installation as "not found" and offers no way to use it, even though the same
machine has a working, authenticated provider one command away.

GameEngine already runs managed local inference inside WSL2 under ADR 0155, so
the platform dependency is not new. What is missing is a decision about the
external-agent side of that boundary, because three things change when a
provider process moves into a distribution:

- **Argument passing.** A launch that goes through a shell would re-quote the
  provider prompt and the injected MCP configuration JSON.
- **Environment.** Windows environment variables do not reach a Linux process
  unless they are named for forwarding, so the MCP bearer token, the run
  identity, and the captured-frame path would silently disappear.
- **Reachability.** ADR 0121 binds the Editor MCP endpoint to loopback. A WSL2
  distribution reaches that endpoint only when it shares the host loopback, so
  a provider placed in WSL can start successfully and then fail every MCP call.

## Decision

### 1. Execution environment is a launch property of the provider, not a new provider

A provider selection gains an **execution environment**: Windows native, or a
WSL2 distribution. The environment changes only how the process is started and
how values cross the boundary. It does not change which provider adapter is
used, what the provider is permitted to do, who owns authoring mutations, or
any completion gate. The Agent Host stays the single authority exactly as ADR
0131 defines.

The WSL2 placement MAY name a distribution. An empty name uses the user's
default distribution. GameEngine does not provision, install, or modify a
distribution for external agents; that is the user's own environment, unlike the
dedicated distribution ADR 0155 provisions for managed inference.

### 2. Placement wraps the argument vector, never a shell command line

A WSL2 launch runs `wsl.exe [-d <distribution>] -- <program> <arguments…>`,
passing the provider's argument vector through unchanged. No shell is
interposed, so the prompt, the MCP configuration JSON, and generic-command
arguments keep exactly the semantics the Windows-native path gives them.

Discovery and authentication probes run through the same placement as the launch
they predict. Probing on Windows and launching in WSL, or the reverse, is not
permitted, because the answer would describe a different installation than the
one that will run.

### 3. Forwarded variables are named explicitly and paths are translated

Editor-provided variables reach a WSL2 provider only through an explicit
forwarding list. Variables whose value is a Windows path are marked for
translation so the provider opens the same file the Editor wrote — in
particular the host-captured frame that ADR 0131 visual evaluation depends on.

A variable that is not named is not forwarded. Adding an Editor-provided
variable therefore requires deciding whether it crosses the boundary, rather
than discovering later that a provider ran without it.

### 4. Loopback reachability is proven before a run starts

ADR 0121 remains authoritative: the MCP endpoint stays bound to loopback and is
never rebound to a virtual adapter, LAN address, or forwarded port to make a
WSL2 provider work.

Before a run is started with a WSL2 placement, the Editor proves from inside the
distribution that the endpoint is reachable. A distribution that cannot reach it
fails the launch with a diagnostic that names the cause and the two supported
resolutions — enable WSL mirrored networking, or run the provider in the Windows
native environment. A run MUST NOT start into a state where every MCP call will
fail for a reason the provider cannot report.

### 5. Confinement claims stay truthful across the boundary

Windows-side process confinement does not extend into a WSL2 distribution. The
recorded confinement profile for an external agent already reports its
guarantees as unavailable under the application-policy provider, and a WSL2
placement MUST NOT report stronger guarantees than a Windows-native one. If
enforced confinement is ever implemented for Windows-native external agents, a
WSL2 placement MUST report that requirement as unsatisfied rather than
inheriting the claim.

## Verification

- a WSL2 placement produces the same provider argument vector as the
  Windows-native placement, wrapped by the distribution launcher;
- an empty distribution name omits the distribution selector rather than
  passing an empty one;
- the forwarding list names every Editor-provided variable and marks path
  variables for translation;
- a Windows-native placement performs no reachability probe; and
- an endpoint that is not loopback HTTP is rejected before a WSL2 launch.

## Consequences

A user whose provider CLIs live in WSL can use them from AI Studio without
installing a second copy on Windows, which is the common case on this platform.

GameEngine takes on a second platform-dependent launch path for external agents.
That cost is bounded: the adapters, the event protocol, the permission model,
and the completion gates are unchanged, and the placement is expressed as a
wrapper around an argument vector rather than as a provider variant.

The loopback requirement becomes visible to users who have not enabled mirrored
networking. That is deliberate. The alternative is a run that starts and then
fails on every authoring call.

## Alternatives Considered

**Rebind the MCP endpoint so WSL can always reach it.** Rejected. It weakens the
ADR 0121 transport assumption for every user in order to accommodate one
networking mode, and it exposes a write-capable authoring endpoint to a virtual
network segment.

**Run the provider through `wsl.exe bash -lc "…"`.** Rejected. A shell command
line re-quotes the prompt and the MCP configuration JSON, which is precisely the
class of defect the argument-vector launch avoids.

**Treat "Claude Code in WSL" as a separate provider kind.** Rejected. It would
duplicate every adapter, probe, and event translation for a difference that is
purely about process placement.

**Ask users to install the provider CLI on Windows.** Rejected as the only
option. It duplicates authentication state and toolchains for no benefit, and it
does not match how these runtimes are distributed on this platform.

## Compatibility and Migration

The execution environment is an additive machine-local preference that defaults
to Windows native, so an existing configuration behaves exactly as before. No
session, run, project document, or benchmark identity format changes.
