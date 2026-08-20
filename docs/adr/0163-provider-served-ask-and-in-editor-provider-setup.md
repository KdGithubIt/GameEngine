# ADR 0163: Provider-Served Ask and In-Editor Provider Setup

Status: Accepted
Date: 2026-08-20
Builds on: ADR 0145, ADR 0162
Relates to: ADR 0121, ADR 0131, ADR 0155, ADR 0158, ADR 0160

Update: ADR 0165 supersedes the no-MCP Ask launch and `@latest` installer
choices. Ask now receives a transport-enforced read-only Editor MCP credential,
and provider CLI versions are pinned to the validated adapter versions.

## Context

ADR 0145 made Claude Code and Codex first-class external agent runtimes and kept
their authentication provider-owned. That is what lets a user pay one coding
subscription and have AI Studio's Build mode use it.

Three gaps remained. The first two are experienced as "GameEngine needs a second
AI setup that I did not ask for", and the third as "the product tells me to go
somewhere else to finish setting it up".

1. Ask was reachable only through a `ModelBackend`: Managed Local, external
   local, Hosted API, or Enterprise. A user with a working, signed-in provider
   still had to install a local model or supply an API key before the composer
   could answer a question. Two runtimes, two costs, two failure modes, for one
   product surface.
2. Provider sign-in existed only outside the Editor. The user had to find a
   terminal, run `codex login` or `claude auth login`, and come back. That is a
   context switch out of the product for a step the product depends on, and it
   is the step most likely to be abandoned.
3. Installing and updating the provider CLI had the same problem, with a worse
   failure mode: an outdated CLI keeps working until the provider's service
   moves, and then fails with a message about the CLI's version that the Editor
   was not surfacing at all.

None of these is an authentication-ownership problem. GameEngine does not want
the provider credential, and ADR 0145 is right that it must not have it.

## Decision

### 1. Ask may be served by a ready external provider

An Ask turn is answered by the selected external agent provider when all of the
following hold: the user keeps the Ask routing preference on, the selected
provider is one whose read-only launch GameEngine constructs itself, and that
provider's own status reports it discovered and authenticated. Otherwise Ask
keeps using the selected `ModelBackend`, unchanged.

A provider-served answer is not a run. It acquires no work claim, prepares no
Agent Code Workspace, creates no `AgentRun`, and receives no Editor MCP endpoint
or bearer material. It is one provider process, started with the narrowest
argument vector that can still read the project:

- Claude Code runs non-interactively with read-only tools allowed and every
  write-capable tool named on the provider's deny list, because an allow list
  only decides what runs without a prompt while a deny list is the rule a
  project's own provider settings cannot widen. It also runs with a strict,
  empty MCP configuration, so unrelated user MCP servers are not loaded.
- Codex runs `exec` under the provider's `read-only` sandbox, with the user's
  MCP servers overridden away for the same reason.

A failure the provider states on its own stream is reported as the provider
stated it, and it ends the turn even when the provider process still exits
successfully. The classified exit remains the fallback for a process that fails
without explaining itself.

The generic external command is never used for Ask. GameEngine cannot prove that
a user-defined command stays read-only, and Ask's contract in ADR 0162 is that
it never writes. The generic provider therefore falls through to the
`ModelBackend` path.

This does not make an external agent a `ModelBackend`. ADR 0145's non-goal
stands: the provider is not registered in model routing, is not benchmarked as a
model backend, carries no resource-residency plan, and reports no model
telemetry. It answers as a provider, and it is named as one in the composer and
in the transcript status.

The routing preference defaults to on. Ask carries no write capability, so
adopting a provider the user already selected and signed into does not grant
anything ADR 0162 withholds; it removes a second runtime the user never asked
for. The composer names the answering provider before the turn is sent, and the
settings surface states plainly that the conversation and the project evidence
the provider reads reach that provider's service.

### 2. The Editor starts the provider's own sign-in

AI Studio settings can start the selected provider's own login flow as a child
process and relay its output, including a printed sign-in URL when the provider
prints one instead of opening a browser itself.

GameEngine still never sees, stores, or forwards the credential. The provider
performs its own flow and stores the result where the provider keeps it. The
Editor's role is to start the process, show progress, allow cancellation, and
re-probe status when it finishes. This is a launch convenience, not an
authentication mechanism, so ADR 0145's authentication ownership is unchanged.

### 3. Provider discovery runs in the background and may seed the selection

Discovery and authentication probes run provider processes, so they run on a
worker rather than the UI thread, once per Editor session and on explicit
refresh.

When the probe finishes and the user has configured nothing at all — the
provider selection is still the default generic command with no program — a
detected, signed-in first-class provider is adopted as the selection, once per
session, and the adoption is reported in the status line. An explicit selection
is never overwritten, and a generic command that has been configured is never
replaced.

### 4. The Editor may run the provider's own install command

AI Studio settings can also run the provider's published install command as a
child process, with the same output relay, cancellation, and re-probe the
sign-in step uses. This closes the last step that forced a terminal: install,
sign in, and use, all from the Editor.

Constraints that make this acceptable rather than an arbitrary command runner:

- The command is fixed by the adapter, never composed from user input, and the
  exact command is displayed before it is run. The click is the consent.
- Exactly one channel is used: the provider's npm package. GameEngine does not
  host, mirror, or fetch a provider artifact itself, and it does not pipe a
  remote script into a shell — a piped installer is opaque at the moment the
  user agrees to it, which is the property that makes displaying the command
  meaningless.
- The generic provider has no install command. Whoever supplies that command
  installs it.
- When the installer is absent the action is disabled and says so, rather than
  failing at launch with a "program not found" the user cannot act on.
- The version is `latest`, not pinned. A provider CLI is a client of a service
  that moves independently of GameEngine, and an outdated client fails against
  the provider's current models. This is the opposite of ADR 0155's pinned
  managed runtime, where GameEngine owns the build and reproducibility is the
  point.

Because an install can land beside an older copy that still comes first on
`PATH` — which looks exactly like an update that did nothing — the probe also
resolves which `PATH` entries provide the program, shows them, and says so when
more than one directory does. Those paths are machine-local display only and are
not part of the sanitized adapter status reported remotely.

## Consequences

- One signed-in provider can serve both Ask and Build, with no local model and
  no API key.
- Install and sign-in are both completable without leaving the Editor. A machine
  with the installer present needs no terminal at any point; a machine without
  it is told what to install once.
- A user who prefers local or hosted inference for Ask turns the routing
  preference off and keeps exactly the previous behavior.
- Provider-served Ask inherits the external provider's existing posture rather
  than the `ModelBackend` network-permission gate. The gate exists because a
  `ModelBackend` is a GameEngine-configured endpoint using a GameEngine-held
  credential; a first-class provider is a user-installed, user-authenticated
  program whose network use is provider-owned, exactly as it already is when the
  same provider runs a Build. Selecting the provider and keeping the routing
  preference on is the explicit act.

## Verification

- A question launch plan for each first-class provider denies the write-capable
  tools explicitly, requests no workspace-write sandbox, and carries no Editor
  MCP server name or bearer variable.
- A failure the provider stated is reported instead of an exit code.
- The generic provider is refused for Ask with an actionable message.
- Only text a provider states as assistant output becomes an answer; provider
  errors, tool activity, and non-JSON diagnostics never do.
- The routing rule rejects a status that belongs to a different provider, a
  provider that reports sign-in required, and the preference being off.
- A sign-in plan starts the provider's subscription flow, and a printed URL is
  extracted without surrounding text.
- An install plan runs the provider's own package through the launcher that
  exists on the target platform, is refused for the generic provider, and is
  rendered as text before it runs.
- A program provided by more than one `PATH` directory is reported as shadowed,
  while a shim and its launcher in one directory are not.
- Provider selection, status, sign-in, and Ask routing UI require Editor Visual
  Validation.

## Non-goals

This ADR does not give the provider write capability in Ask, does not treat a
provider as a `ModelBackend`, does not store or read provider credentials, does
not implement a sign-in flow of its own, and does not forward provider
authentication state to Remote AI Studio beyond the sanitized adapter status
ADR 0145 already defines.
