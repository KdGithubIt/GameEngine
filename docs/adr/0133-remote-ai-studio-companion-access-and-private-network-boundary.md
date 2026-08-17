# ADR 0133: Remote AI Studio Companion Access and Private Network Boundary

Status: Accepted
Date: 2026-08-17
Amends: ADR 0131
Relates to: ADR 0035, ADR 0117, ADR 0121, ADR 0132

## Context

ADR 0131 defines AI Studio as the project-scoped conversational surface for
planning, authorizing, executing, validating, playtesting, and reviewing AI
work. It also defines provider-independent `AgentEvent` progress, a resumable
`AgentRun` state machine, layered permissions, governed source-code mutation,
frame capture, and session persistence. Those contracts are sufficient for a
local Editor UI, but they do not define how the same AI Studio session may be
used from a phone or another personal device when the user is away from the
host PC.

The desired remote workflow is intentionally narrower than a remote Editor. A
phone does not need a second Hierarchy, Inspector, Scene manipulation surface,
or direct authoring controls. The useful remote interaction is conversation
with the same AI Studio agent, proposal review, Go and Stop, permission
responses, semantic progress, validation and playtest results, and captured
frames. When a live view of the Windows Editor is useful, it is an observation
and remote-display concern rather than a reason to duplicate Editor authoring
semantics on the phone.

ADR 0121 requires the initial write-capable MCP endpoint to remain
Editor-attached, project-scoped, authenticated with ephemeral session metadata,
and bound only to the local machine. Exposing that endpoint directly to a LAN,
VPN address, or the public Internet would weaken the single-authority and
transport assumptions of ADR 0121. A remote companion therefore needs an
application boundary above the agent host rather than a remotely reachable MCP
server or a second project writer.

Mobile connectivity is also intermittent. A phone may move between Wi-Fi and
cellular networks, suspend its browser, or disconnect while an `AgentRun` is
executing. Remote use must not make the lifetime of an agent run depend on one
HTTP connection, and reconnecting must not duplicate Go, Stop, permission, or
other state-changing actions.

Finally, GameEngine does not need to become a general remote-desktop or video
streaming product in order to support this workflow. A private authenticated
network and an existing remote-display stack can provide secure reachability
and optional low-latency screen viewing while GameEngine remains responsible
for AI Studio semantics, structured status, captured frames, and permissions.

A project-wide decision is therefore required for the remote AI Studio product
surface, gateway trust boundary, private-network deployment model, reconnect
semantics, security constraints, captured-frame delivery, and the relationship
to optional live desktop streaming.

## Decision

### 1. Remote AI Studio is a frontend over the existing project agent host

GameEngine will support a **Remote AI Studio** companion surface for personal
remote access. It is another frontend over the same project-scoped agent host
defined by ADR 0131, not a second AI runtime, a second authoring writer, or a
phone-specific automation system.

The intended architecture is:

```text
Remote browser / companion UI
  -> authenticated private network
  -> loopback-only Remote AI Studio Gateway
  -> project Agent Host
       -> Agent Runtime
       -> Agent Code Workspace
       -> managed Validation / Playtest
       -> local Editor MCP endpoint
       -> AI Agent Bridge for runtime observation
```

The local Editor AI Studio and Remote AI Studio MUST observe and control the
same authoritative `AgentSession`, proposal versions, `AgentRun` state,
permission broker, and audit history. The remote frontend MUST NOT implement
provider-specific orchestration, authoring rules, code mutation, validation
rules, or permission policy independently.

ADR 0131 owns local AI Studio presentation, including an embedded panel, a
detached native OS window/viewport, and any future loopback-only same-machine
frontend protocol. ADR 0135 continues to own native-inference resource
arbitration regardless of which frontend presents its state. This ADR begins
where an AI Studio frontend is made reachable from another device or trust/
network context. It therefore owns the private-network gateway, remote
authentication, idempotent mutation requests, reconnect semantics, remote
sanitization, and companion UX. Local detachment MUST NOT be implemented by
creating a second Agent Host or by treating the remote gateway as a second
writer.

### 2. The remote product surface is conversation and run control, not a remote Editor

The initial Remote AI Studio surface MUST support the user actions and state
needed to continue an AI creation session away from the host PC:

- view and continue the AI conversation;
- view proposal revisions and acceptance criteria;
- invoke Go for an exact proposal version;
- invoke Stop for the active run;
- view the current run state and structured progress events;
- answer `AwaitingUser` questions;
- approve or deny agent permission requests using ADR 0131 approval scopes;
- view code-change, authoring-change, validation, playtest, repair, and
  completion summaries; and
- view captured frames and visual-evaluation results when they are part of the
  run.

The initial companion does not need direct remote Hierarchy, Inspector, Scene,
graph, asset, material, or component editing. If a user wants one of those
changes, the normal remote path is to describe the desired result to the agent,
which then uses the same governed authoring and code services as a local AI
Studio run.

Direct semantic authoring controls may be added to a future remote UI only when
they remain thin clients of the same shared authoring capabilities defined by
ADR 0121 and ADR 0132. Their absence is not a parity defect for this companion
surface.

### 3. A GUI-free Remote AI Studio Gateway owns the remote application boundary

Remote access is served by a small GUI-free **Remote AI Studio Gateway** above
the agent host. The gateway owns remote session transport, authenticated client
identity, request idempotency, event delivery, reconnect snapshots, and safe
presentation of agent-host state.

The gateway MUST NOT become a new authoring or execution authority. In
particular, it MUST NOT expose generic endpoints that directly execute arbitrary
shell commands, write arbitrary workspace files, invoke Git commands, proxy raw
MCP requests, or bypass the agent permission broker.

The initial transport SHOULD use ordinary HTTP request/response operations for
client actions plus Server-Sent Events for the ordered agent event stream. A
WebSocket transport MAY be added later if a demonstrated interaction requires
full-duplex messaging, but the agent-host contract MUST remain transport
independent.

Conceptually, the surface may provide operations equivalent to:

```text
GET  session snapshot
GET  active run snapshot
POST conversation message
POST go
POST stop
POST awaiting-user response
POST permission response
GET  ordered event stream
GET  captured frame
```

These are application operations over existing agent-host semantics. Their
exact URL paths and wire DTOs are implementation details until published as a
separate stable external API contract.

### 4. The gateway binds to loopback; remote reachability is provided externally

The initial Remote AI Studio Gateway MUST bind only to a loopback interface on
the host PC. GameEngine does not directly bind the gateway to a public address,
forward a home-router port, or expose it to the public Internet.

Personal remote reachability is provided by an authenticated private overlay
network or equivalent trusted reverse proxy. The reference deployment may use
Tailscale and Tailscale Serve, but the GameEngine application contract MUST NOT
depend on Tailscale-specific APIs, address formats, account models, or client
libraries.

The reference topology is:

```text
iPhone / remote personal device
  -> authenticated private overlay network
  -> private HTTPS reverse proxy / Serve endpoint
  -> 127.0.0.1 Remote AI Studio Gateway
```

Public ingress mechanisms such as an Internet-wide tunnel or public reverse
proxy are outside the initial supported deployment. Adding supported public
Internet access requires an explicit threat model, authentication design,
abuse controls, credential policy, and deployment decision rather than merely
changing the bind address.

### 5. The ADR 0121 MCP endpoint remains loopback-only and is never the remote API

ADR 0121 remains authoritative for MCP lifecycle and Editor ownership. Remote
AI Studio MUST NOT make the project-scoped MCP endpoint remotely reachable.

The path remains:

```text
Remote user
  -> Remote AI Studio Gateway
  -> Agent Host
  -> local MCP client/runtime integration
  -> loopback-only Editor MCP endpoint
  -> live Editor authoring services
```

A remote connection therefore grants access to AI Studio user actions, not to a
raw authoring protocol. MCP credentials, endpoint ports, bearer material, and
other ephemeral Editor discovery metadata MUST NOT be forwarded to the remote
browser or stored in remote UI state.

The active Editor remains the authoritative project writer exactly as defined
by ADR 0121. Remote access does not create a headless second writer and does not
weaken stale-revision or transaction checks.

### 6. Remote authentication does not replace GameEngine authorization

Network membership or reverse-proxy authentication answers who may reach the
Remote AI Studio Gateway. It does not grant project mutation, network,
filesystem, command execution, runtime control, or other agent capabilities.

All AI work continues to use the ADR 0131 permission broker. Remote permission
requests MUST present the same semantic capability, reason, and available
approval scopes as the local AI Studio surface, including when applicable:

```text
Allow once
Allow for this run
Allow for this project
Deny
```

The gateway MAY consume authenticated identity asserted by a trusted local
reverse proxy only when the gateway is loopback-only and the deployment ensures
that untrusted clients cannot connect directly while spoofing those headers.
If the gateway adds its own session credential, that credential MUST be
short-lived or revocable, stored outside canonical project data, and protected
against cross-site request forgery and accidental disclosure.

Secrets, provider credentials, MCP credentials, private-network credentials,
and reverse-proxy authentication material MUST NOT be placed in canonical
project files or project-shared AI session records.

### 7. Remote state-changing requests are idempotent

Mobile retries and reconnects MUST NOT duplicate state-changing operations.
Every remote mutation whose duplicate execution could matter MUST carry a
client-generated request identity or equivalent idempotency key.

At minimum this applies to:

- Go;
- Stop;
- permission responses;
- `AwaitingUser` responses; and
- any future remote operation that creates or commits work.

The gateway or agent host MUST remember the result of a recently accepted
request identity for a bounded period sufficient to make normal client retry
safe. Repeating the same request identity returns or reconstructs the original
outcome rather than starting a second run or applying the decision twice.

Go also identifies the proposal version being authorized. A retry MUST NOT
implicitly authorize a newer proposal version that appeared after the original
request.

### 8. Runs survive client disconnects and reconnect through snapshots plus event cursors

The lifetime of an `AgentRun` belongs to the project agent host, not to the
remote browser connection. Suspending Safari, losing cellular coverage, or
closing the companion UI MUST NOT cancel a run unless the user explicitly
requested Stop or host policy requires cancellation for another reason.

Agent events exposed to remote clients MUST have an ordered cursor or sequence
identity suitable for reconnect. The remote client reconnects by obtaining an
authoritative session/run snapshot and then requesting events after its last
confirmed cursor.

The host MUST make reconnect correct even when an old event is no longer
retained. In that case the snapshot is authoritative and the client resumes
from the oldest available event after the snapshot boundary instead of assuming
that every historical event can be replayed forever.

An `AwaitingUser` question or permission request remains pending across a remote
disconnect. Reconnecting shows the current pending decision rather than relying
on a transient notification that may have been missed.

### 9. Remote conversation and history reuse ADR 0131 persistence semantics

Remote access does not create a separate conversation store. The companion uses
the same local-private or explicitly project-shared `AgentSession` persistence
rules defined by ADR 0131.

A remote browser MAY cache presentation state such as the last event cursor,
scroll position, draft message, or selected run, but that cache is not the
authoritative conversation, proposal, run, permission, or audit record.

Project-shared session records continue to exclude secrets, machine-specific
endpoint metadata, transient ports, process identifiers, and complete local
agent workspaces. Remote transport details do not become project authoring data.

### 10. Captured frames are the built-in remote visual result

The built-in visual result for Remote AI Studio is the frame-capture and visual
evaluation path already required by ADR 0131 and provided through the AI Agent
Bridge boundary from ADR 0035.

When a run captures a relevant frame, the remote frontend SHOULD be able to
show that frame with the corresponding run step, playtest result, and visual
evaluation. Frame delivery MUST use bounded artifacts or streaming responses
appropriate for individual captures and MUST NOT require the phone to connect
directly to the AI Agent Bridge.

Captured frames are sufficient for the initial remote completion workflow:

```text
request work
  -> implementation
  -> validation
  -> playtest
  -> frame capture
  -> visual evaluation
  -> remote review
```

Failure to provide a live video stream does not make Remote AI Studio
incomplete when the required captured-frame review is available.

### 11. Live Editor video is an optional external remote-display integration

Low-latency live viewing of the Windows Editor or running game is useful but is
not part of the core Remote AI Studio transport. The initial recommended
approach is to use an existing remote-display or game-streaming host/client,
such as Sunshine and Moonlight or an equivalent system, through the same
private network.

GameEngine therefore does not initially implement a custom H.264/HEVC encoder,
WebRTC media server, desktop-capture stack, NAT traversal service, or mobile
video client solely for Remote AI Studio.

The remote display is observation and optional manual desktop access. It MUST
NOT become the semantic path by which the AI performs normal project authoring.
AI authoring continues through MCP and shared services; runtime visual
interaction continues through the AI Agent Bridge where ADR 0035 applies.

A future engine-native low-latency video surface requires a separate decision
if it introduces renderer readback contracts, encoder dependencies, media
transport, or security responsibilities inside GameEngine.

### 12. The initial host must already be online with the target Editor project open

The first Remote AI Studio implementation assumes:

- the host Windows PC is powered on and connected to the private network;
- the GameEngine application services required by the gateway are running; and
- the target project is open in its project-scoped Editor process when
  write-capable MCP authoring is required.

Remote machine power-on, OS login, Launcher control, arbitrary project
selection, and remote Editor process startup are separate lifecycle concerns.
They MAY be added later through narrow authenticated application operations,
but the first implementation MUST NOT solve them by exposing a generic remote
process launcher or shell.

If the required Editor project is not available, the companion reports that
state clearly and does not silently open the project through an ungoverned
second writer.

### 13. Remote actions are auditable and errors are sanitized

Remote user actions that affect agent execution SHOULD be represented in the
same AI Studio audit history as equivalent local actions, with enough origin
metadata to distinguish local and remote user decisions when useful for
security or diagnosis.

The remote surface MUST NOT expose raw provider credentials, environment
variables, private filesystem paths, MCP bearer material, reverse-proxy
secrets, or unrestricted process output merely because those values exist in a
local diagnostic context.

Structured errors sent to the remote client SHOULD contain a stable category,
human-readable explanation, and retryability information where relevant.
Detailed local diagnostics may remain available in host logs without being
copied verbatim to the phone.

### 14. The first client is a responsive web companion, not a required native app

The initial client SHOULD be a responsive web UI suitable for mobile Safari and
other modern browsers. It MAY be installable as a Progressive Web App, but
GameEngine does not require a native iOS or Android application to satisfy this
ADR.

The mobile layout prioritizes:

- conversation;
- proposal and acceptance criteria;
- Go and Stop;
- pending user or permission decisions;
- semantic run progress;
- validation and playtest result summaries; and
- captured frames.

Desktop-only Editor panels and pointer-precise controls are not copied into the
mobile layout merely for visual parity.

### 15. Implementation proceeds in capability slices

Implementation should proceed in the following order so remote access does not
create a parallel AI architecture:

1. **Agent-host remote contracts**
   - expose GUI-free session snapshots, active-run snapshots, pending decisions,
     ordered event subscription, and idempotent user-intent operations above
     the existing agent host;
   - keep provider and authoring logic below that boundary.
2. **Loopback gateway**
   - add the loopback-only HTTP application service;
   - implement request identity, event cursors, bounded event retention,
     sanitized errors, and captured-frame retrieval;
   - add security tests proving that forbidden generic execution surfaces do
     not exist.
3. **Responsive companion UI**
   - implement conversation, proposal, Go, Stop, pending decisions, run
     progress, result summaries, and frame viewing;
   - reuse semantic status and permission vocabulary from local AI Studio.
4. **Private-network deployment**
   - document and validate the reference private-overlay and local reverse-proxy
     topology;
   - keep the gateway loopback-only and avoid public router port forwarding.
5. **Disconnect and retry hardening**
   - test browser suspension, network changes, event replay, stale cursors,
     duplicate requests, and pending decisions across reconnect.
6. **Remote visual review**
   - integrate captured frames into the run timeline and completion result;
   - document optional external remote-display use for live viewing.
7. **Later lifecycle extensions**
   - consider narrow remote project/Editor activation only after the core
     companion is reliable and only with an explicit lifecycle and security
     contract.

A slice MUST NOT bypass Agent Host, MCP, permission, or project-writer
boundaries merely to make the mobile UI functional sooner.

### 16. Validation includes mobile reconnect, security, and existing run-completion semantics

Implementation of this ADR requires automated tests at the lowest useful layer
for at least:

- Go retry with the same request identity creates only one run;
- permission and `AwaitingUser` retry does not apply a decision twice;
- reconnect restores the authoritative session/run snapshot;
- event cursors resume ordered delivery without inventing missing state;
- a disconnected remote client does not cancel an active run;
- pending decisions remain visible after reconnect;
- stale proposal authorization is rejected;
- remote clients cannot obtain MCP credentials through the gateway;
- remote actions cannot invoke generic shell, raw filesystem, or Git endpoints;
  and
- captured frames are associated with the correct project session and run.

Manual deployment validation SHOULD cover access from the host LAN, cellular or
another external network through the private overlay, and denial when the
private-network identity is absent.

When the responsive companion UI is implemented or materially changed, normal
Windows Validation remains required and Visual Validation SHOULD inspect the
relevant desktop and mobile-sized layouts when their presentation affects
correctness.

ADR 0131 completion semantics remain unchanged. Remote use MUST NOT report a
run as completed when required authoring validation, source validation,
playtest, frame capture, visual evaluation, or interaction scenarios have not
actually passed.

## Consequences

A user can continue the same AI Studio conversation and authorize implementation
from a phone without turning the phone into a second Editor or project writer.
The difficult implementation logic remains in the agent host, shared authoring
services, code workspace, validation, and playtest systems, so local and remote
surfaces are less likely to drift.

The MCP endpoint remains local and retains the live Editor as the authoritative
writer. Private remote reachability is separated from GameEngine application
semantics, reducing the need for custom NAT traversal, public ingress, or
Internet-facing credential infrastructure in the engine.

Mobile reliability requires explicit idempotency, snapshots, event cursors, and
bounded replay rather than assuming a permanent connection. These contracts
also improve resilience for a future desktop web client or other companion
surfaces.

The first remote client is intentionally limited. Users do not receive a full
remote Inspector or native mobile Editor, and the host PC and target Editor
must already be available. That limitation keeps the initial security and
lifecycle boundary narrow.

Live low-latency Editor viewing depends on an external remote-display solution
in the initial design. This avoids introducing video encoding and streaming
complexity into GameEngine, at the cost of a separate optional application on
the host and phone.

## Alternatives Considered

### Expose the existing MCP endpoint through the VPN

Rejected. ADR 0121 intentionally makes the write-capable MCP endpoint
loopback-only, project-scoped, and owned by the active Editor. Remote AI Studio
needs user-facing agent orchestration, not raw authoring transport access.

### Build a complete remote Editor for mobile

Rejected for the initial goal. Hierarchy, Inspector, Scene manipulation, graph
editing, and pointer-precise desktop UI add substantial presentation and
interaction work without improving the core need to converse with the agent and
review implementation results.

### Add generic remote shell, Git, and filesystem endpoints

Rejected. Those endpoints would create a broad remote execution surface outside
ADR 0131's permission broker and governed code workspace. The remote client
should express user intent and decisions; the agent host remains responsible
for performing governed work.

### Bind the gateway to the LAN or public Internet and use a password

Rejected as the default. A private authenticated overlay keeps the initial
exposure narrow and avoids making GameEngine responsible for an Internet-facing
account, password-reset, brute-force protection, certificate, and public ingress
system. Public deployment requires a separate explicit security decision.

### Implement engine-native WebRTC video streaming immediately

Rejected. Captured frames already satisfy structured visual review, while
mature external remote-display systems solve low-latency desktop viewing. A
custom media stack would add renderer readback, encoding, transport, NAT, and
mobile-client complexity before the AI companion itself is proven useful.

### Require a native iOS or Android client

Rejected. A responsive private web companion can provide conversation, run
control, permissions, progress, and frame review with less platform-specific
code. Native clients remain possible later if they provide a demonstrated
benefit such as better background notifications or media integration.

### Use remote desktop alone instead of Remote AI Studio

Rejected as the primary workflow. Remote desktop can show the local Editor, but
it does not provide mobile-appropriate semantic progress, reliable permission
prompts, reconnect-safe run control, structured validation results, or direct
conversation history. It remains a useful optional live-view companion.

### Cancel the agent whenever the remote client disconnects

Rejected. Mobile connections are routinely interrupted, and ADR 0131 already
models a run as host-owned resumable state. Disconnect is a transport event, not
an explicit user cancellation decision.

## Compatibility and Migration

This decision does not change canonical project serialization, stable IDs,
authoring command semantics, existing MCP tool names, or the ADR 0121
project-scoped MCP lifecycle. The MCP endpoint remains loopback-only.

ADR 0131 local AI Studio behavior remains valid. Remote AI Studio is additive
and reuses the same session, proposal, run, permission, code-workspace,
validation, playtest, capture, and audit contracts. Existing AI sessions do not
require migration merely because a remote frontend becomes available.

Remote gateway endpoint metadata, request identities, event cursors,
private-network information, and web-session credentials are application state.
They MUST NOT be serialized into `project.json`, Scene files, Graph files,
asset manifests, project settings, or packaged game content.

The initial private-network reference deployment may depend operationally on an
external overlay-network and remote-display installation, but those products do
not become Rust workspace dependencies or persisted project contracts.

Future public Internet access, native mobile clients, remote process launch,
full remote Editor controls, or engine-native live video may be added without
changing this ADR only when they preserve the boundaries above. If they require
a broader trust model, new project-writer model, public authentication system,
media transport contract, or machine lifecycle authority, they require a new
or amending ADR before implementation.
