# ADR 0164: Unified AI Selection, Agent Vocabulary, and Reachable Remote Access

Status: Accepted
Date: 2026-08-20
Amends: ADR 0162, ADR 0163
Relates to: ADR 0131, ADR 0133, ADR 0145, ADR 0147, ADR 0150, ADR 0155, ADR 0158, ADR 0160

## Context

ADR 0162 §5 split AI Studio into a selection tier and a configuration tier, and
ADR 0163 made a signed-in external provider able to answer Ask so that one
coding subscription serves both modes. Both records are about what the studio
does. Neither examined the vocabulary the studio hands the user, and the shipped
surface now exposes the implementation's category system as the thing the user
must reason about.

**The studio asks the user to choose on an axis the product invented.** Deciding
who handles the next message currently takes three controls in two tiers. The
composer's model list offers registered managed-local models and the remaining
`ModelBackend` entries. The external agent provider is chosen somewhere else
entirely, in the configuration tier. A third value, the Ask routing preference,
decides which of those two answers a read-only turn. Nothing on screen states
that these are one question, and the correct combination is not derivable from
the labels. The user's question is "who does this next message"; the product
answers with a taxonomy.

**"Providers" names an implementation category, not a thing the user wants.**
The section holds Claude Code, Codex, and a generic compatible-agent command:
external agent programs with their own contracts, their own credentials, and
their own model choices. ADR 0163 is explicit that these are not `ModelBackend`
entries, and that separation is right internally. Exported to the surface as a
top-level noun, it forces the user to learn a distinction whose only purpose is
to keep two internal execution paths apart. The section compounds this by
reporting discovery state, authentication state, a four-line capability matrix,
and a resolved executable path at the same depth as the one control that
matters, so the frequent reading — is this ready, and if not, what do I press —
is the hardest one to perform.

**Remote presents a URL that cannot work on the device it is for.** The Remote
section shows the gateway endpoint and the companion URL, both on `127.0.0.1`.
ADR 0133 §4 deliberately binds the gateway to loopback and states that
reachability is provided by an authenticated private overlay plus a private
reverse proxy in front of it. The URL the studio displays is therefore the
innermost hop of a four-hop path, and it is the one hop that is meaningless on
the phone: `127.0.0.1` names the phone itself. The user is shown a credential
they are told to send to their device, which their device cannot open. The
information needed to configure the proxy is presented as though it were the
information needed to use the product.

None of the three is a defect of the records that produced them. They are the
cost of publishing internal structure as user-facing structure.

## Decision

### 1. One selection axis, named AI, chooses who runs the next message

Every AI Studio presentation MUST expose exactly three selections on the
composer, and no others:

```text
Mode     Ask or Build
AI       who runs the next message
Effort   the ADR 0150 quality preference
```

The AI list MUST be the single place the executor of the next turn is chosen. It
MUST be grouped by what the user is choosing between rather than by which
internal path serves the entry:

```text
Agents        Claude Code, Codex, and a configured compatible agent program
Local models  models registered with the managed local runtime, and a
              configured external local endpoint
Cloud         hosted API and enterprise endpoints
```

Every entry MUST carry its readiness in the same words the configuration tier
uses for it. The list MUST NOT register, install, download, remove,
authenticate, or benchmark anything; it MAY carry exactly one action that leaves
the tier, and that action opens the configuration tier at the section that owns
the entry, as ADR 0162 §5 already requires.

The selected AI MUST be one value. Mode display, effective write capability, and
the executing path MUST all be derived from that value together with the
selected mode, in the manner ADR 0162 §1 requires of the mode indicator. Two
independent selections that the studio combines behind the composer are a defect
of this record.

The separation of `ExternalAgentProviderKind` from `ModelBackendPreference` is
retained internally exactly as ADR 0163 established it. This section changes
what is presented, not what exists.

The Ask routing preference is removed as a user-facing control. Selecting an
agent as the AI is the statement that the agent serves the selected mode; the
conditions ADR 0163 §1 places on provider-served Ask continue to apply
unchanged, and are reported under §2 rather than configured by a toggle.

### 2. An AI that cannot serve the selected mode is stated, never substituted

When the selected AI cannot serve the selected mode, the studio MUST state that
in one line at the composer, MUST refuse submission, and MUST NOT run the turn
on a different AI.

This covers, at least: a compatible agent program whose read-only launch
GameEngine cannot construct; an agent that is not installed or not signed in; a
model backend with no registered model, no endpoint, or no credential.

The line MUST name the selected entry, MUST state which mode it cannot serve,
and MUST offer the AI list and the owning configuration section as its actions.
It MUST NOT be the only place that state is visible; the entry carries the same
readiness inside the AI list.

Silent substitution is refused for the reason ADR 0162 §1 refuses an invisible
write capability: the surface would then report one executor while another
performed the work, and no audit of the transcript could recover which. Refusing
to send is recoverable in one interaction; a mis-attributed turn is not.

### 3. The configuration tier names agents, and subordinates diagnosis

The configuration tier's sections become:

```text
Models       GGUF registration, discovery, managed runtime setup and removal,
             residency and resource controls
Agents       external agent programs: installation, provider-owned sign-in,
             confinement requirement, and the compatible-agent command
Environment  execution environment and WSL placement
Benchmarks   benchmark tasks, campaigns, and results
Remote       reaching this studio from another device
Presentation where the studio is drawn
```

An agent entry MUST present, at its top level, the agent's name, one readiness
state, and the one action that changes that state. Discovery detail, the
authentication probe's reasoning, the capability matrix, and the resolved
executable path MUST move under a per-agent disclosure that is collapsed by
default. The compatible-agent program and its arguments MUST move under an
advanced disclosure, because it is the entry no user reaches without already
knowing what it is.

This is a presentation rename. `ExternalAgentProviderKind`, the provider
terminology of ADR 0145 and ADR 0163, the crate-internal type names, and every
accepted record keep their existing words. Nothing in this section licenses a
rename of Rust items to match the surface.

### 4. Remote access presents one reachable URL

The Remote section's primary content MUST be, in this order and with nothing
else above it:

```text
Status      whether the phone can reach this studio right now
Phone URL   the URL to open on the phone, with its token masked
Copy        copies the complete URL, token included
Note        one line stating that the phone must be on the private network
```

The phone URL MUST be composed from an external base URL the user supplies and
the access-token fragment the gateway already issues. The studio MUST validate
the supplied base as absolute, `https`, and not a loopback or `localhost` host,
and MUST reject a base that fails any of those checks with the reason it failed.

Until a valid base exists, Status MUST report that phone access is not ready and
MUST name the missing base as the reason. The studio MUST NOT present a loopback
URL as the phone URL, and MUST NOT offer one as a substitute.

The token MUST be masked wherever the phone URL is displayed, and the full URL
MUST be available only through the copy action. The section MUST state, in one
line, that the copied URL is a credential.

The gateway's loopback endpoint, its port, the raw token, and the reverse-proxy
instructions MUST move under an advanced disclosure that is collapsed by
default. They are needed once per machine, to configure the hop that produces
the base URL, and they are not the product.

GameEngine MUST NOT discover the base URL from an overlay vendor's API, address
format, or account model. ADR 0133 §4 states that no part of GameEngine depends
on a particular overlay, and a detection path would create that dependency in
the surface. The base is user-supplied because the hop is user-owned.

Nothing in this section changes the gateway's binding. It binds to loopback, as
ADR 0133 §4 requires, before and after this record.

### 5. The companion presents the same three selections, and only selections

The ADR 0133 companion MUST present Mode, AI, and Effort with the labels, the
grouping, and the readiness wording §1 defines, and MUST apply §2 unchanged: it
states that a selection cannot serve a mode and refuses to send, and it never
substitutes.

The companion MUST NOT expose registration, installation, provider sign-in,
credential entry, GGUF management, execution-environment placement, benchmark
control, or the remote base URL itself. This is ADR 0162 §5's tier split applied
to a device that cannot perform machine setup: the companion is the selection
tier and nothing else.

Two backward-compatible additions to the ADR 0133 HTTP surface are required:

```text
GET  /api/selection   the current Mode, AI, and Effort, plus the selectable
                      entries with their readiness
POST /api/selection   sets one or more of them
```

`POST /api/selection` MUST carry a `request_id` and MUST be idempotent under
ADR 0133 §7. It MUST reject an entry that is not in the list the host returned,
rather than creating or configuring it. Every existing operation is unchanged,
so a client written before this record keeps working.

### 6. Selection state is one value per presentation-independent scope

Mode remains persisted per session, as ADR 0162 §1 requires. AI and Effort are
machine-local preferences.

Every presentation — embedded, detached, and companion — MUST read and write
those same values. A change made on the phone MUST be observable on the PC
presentation, and the reverse, without a restart. The companion MUST NOT hold a
second copy of the selection that diverges from the host's.

### 7. What this record does not relax

- ADR 0133 §4 loopback binding, §5 the MCP endpoint never being the remote API,
  §6 remote authentication not replacing GameEngine authorization, and §13
  error sanitization.
- ADR 0145 provider-owned authentication. GameEngine does not receive, store, or
  proxy a provider credential, and §1's unified list does not give it one.
- ADR 0163 §1's conditions for provider-served Ask. Only the toggle is removed.
- ADR 0162 §5's rule that a control changing what exists on the machine lives in
  the configuration tier.
- ADR 0131 §8 capability escalation. Selecting an agent grants no capability.

## Consequences

The user answers one question — who runs this — instead of reconciling a
provider selection, a backend selection, and a routing preference that live in
two tiers. The internal split that ADR 0163 established survives untouched
behind that single value.

Removing the Ask routing toggle removes a state in which the surface said one
thing and the routing did another. §2 replaces it with a reported condition, so
the failure the toggle used to express is now visible at the moment it matters
instead of configured in advance.

Refusing to send is a new way for the composer to be blocked, and a user with a
Build-only agent selected will meet it the first time they switch to Ask. That
is the intended trade: the alternative is a turn answered by an AI the user did
not choose.

The Remote section stops publishing an unusable URL as its headline, and the
setup information moves to where setup happens. The cost is that a user who has
not yet configured a proxy now sees "not ready" where they previously saw a URL.
That is an accurate report of the state ADR 0133 §4 describes, and the advanced
disclosure still carries everything the proxy configuration needs.

Because the base URL is user-supplied, GameEngine cannot verify that the phone
can actually reach it; Status reports what the host knows, which is that a valid
base exists and the gateway is running. Any wording that implies the host
verified end-to-end reachability is a defect of this record.

`ai_studio.rs` is past seven thousand nine hundred lines. §1 concentrates
selection into one derived value and §4 rewrites a section that is currently
four lines; both are natural points to continue the extraction ADR 0162 noted
but did not require.

## Verification

- A visual validation scenario `adr0164-ai-selection` captures the composer with
  the grouped AI list open, showing agents, local models, and cloud entries with
  readiness in one list.
- A visual validation scenario `adr0164-agents-section` captures the Agents
  section with diagnosis collapsed.
- A visual validation scenario `adr0164-remote-phone-url` captures the Remote
  section with a valid base, a masked token, and the advanced disclosure
  collapsed.
- Unit tests cover: mapping a pre-existing preference file to exactly one
  selected AI; §2's refusal for each unavailable case; base-URL rejection for
  non-`https`, relative, and loopback bases; and `POST /api/selection` rejecting
  an entry absent from the host's list.

## Alternatives Considered

**Keep the Providers section and improve only its density.** Cheaper, and needs
no record. Rejected as the target state: the density is a symptom, and the
selection would still be split across two tiers with a third preference deciding
between them.

**Fall back to a configured model when the selected agent cannot serve Ask.**
Nothing blocks, and the user always gets an answer. Rejected: the composer would
name one AI while another answered, which is the exact divergence ADR 0162 §1
was written to prevent, and the transcript could not report which one ran.

**Change the AI selection automatically when the mode changes.** Also never
blocks, and never mis-attributes a turn. Rejected: it silently overwrites an
explicit user choice, and the user who switches back to Build finds a different
AI selected than the one they picked.

**Detect the overlay and fill the base URL automatically.** Convenient for the
reference deployment. Rejected: it makes the surface depend on a specific
overlay vendor, which ADR 0133 §4 explicitly refuses, and it fails silently for
every other topology the ADR permits.

**Keep showing the loopback URL with an explanation of why it is not the phone
URL.** Rejected: the explanation does not make the URL usable, and the section's
most prominent element would remain the one thing the reader must not use.

**Present a QR code instead of a copy action.** A QR code is a strictly better
transfer for the phone, and it is additive to §4 rather than a replacement.
Deferred: the workspace has no QR encoder dependency, and the user-stated
minimum is a URL that can be copied and pasted. §4 does not forbid adding one.

**Rename the Rust types to match the surface vocabulary.** Rejected: it would
churn ADR 0145 and ADR 0163 terminology across the crate for a presentation
change, and the internal distinction those records draw is real and still
load-bearing.

## Compatibility and Migration

No serialized project data changes. `AgentSession`, `AgentRun`, `AgentEvent`,
`ConversationMessage`, and `AgentProposal` keep their schemas and their schema
versions.

`AiStudioPreferences` keeps every existing field. A preference file written
before this record MUST map to exactly one selected AI on load: when the Ask
routing preference is set and the recorded external agent provider is one whose
read-only launch GameEngine can construct, the selection resolves to that agent;
otherwise it resolves to the recorded `ModelBackend`. The Ask routing field
remains readable with `serde(default)` so older and newer files both load, and
stops being consulted for routing.

`AiStudioPreferences` gains a remote base URL field, added with `serde(default)`
in the manner of its existing fields and defaulting to empty. An empty base is
the not-ready state §4 describes, so an existing installation loses nothing it
previously had: the loopback endpoint it used to display is still present under
the advanced disclosure.

No public API across crate boundaries changes. `AiStudioPanel` remains the
Editor's entry point, and no crate dependency is added.

The companion additions in §5 are additive, and every operation ADR 0133 and
ADR 0162 defined keeps its path, its request shape, and its idempotency rules.

`docs/REMOTE_AI_STUDIO_DEPLOYMENT.md` describes the host setup in terms of the
loopback URL AI Studio shows. Its host-setup steps are updated by the
implementing change to name the advanced disclosure as the source of the port,
and to name the base URL field as where the published origin is entered.

ADR 0131, ADR 0133, ADR 0145, ADR 0158, ADR 0162, and ADR 0163 remain Accepted.
This record refines ADR 0162 §5's section list, removes the user-facing control
introduced by ADR 0163 §1 while keeping its conditions, and refines how ADR 0133
§4's topology is presented. No other section of any of them is changed.
