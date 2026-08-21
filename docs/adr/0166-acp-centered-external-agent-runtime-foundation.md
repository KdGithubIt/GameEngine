# ADR 0166: ACP-Centered External Agent Runtime Foundation

Status: Accepted
Date: 2026-08-21
Builds on: ADR 0121, ADR 0131, ADR 0145, ADR 0165
Relates to: ADR 0152, ADR 0153, ADR 0160, ADR 0164

## Context

GameEngine already has an Agent Host, provider-specific external process
adapters, and a native model/tool runtime. The external path works, but adding
more coding agents by extending provider-specific launch, session, event,
permission, and cancellation logic would make the central architecture depend
on a growing provider list.

ACP standardizes the editor-to-agent session boundary. It can remove duplicated
agent harness logic without replacing GameEngine authority. ADR 0131 keeps
permission policy, work claims, validation, persistence, audit, and completion
in Agent Host. ADR 0121 and ADR 0165 keep project inspection/mutation behind the
Editor MCP endpoint and distinguish read-only credentials from AgentRun-bound
write credentials.

This foundation must also support parallel migration: individual Codex, Claude,
Goose, local-agent, and future ACP adapters should be implementable without
editing each other's provider code or a central provider enum.

## Decision

### 1. ACP is the common external Agent Runtime boundary beneath Agent Host

GameEngine adds an internal `acp_agent_runtime` module. ACP runtimes own only
agent-side execution concerns: launch/connection, ACP initialization and
capability negotiation, ACP session lifecycle, prompt delivery, structured
updates, permission request transport, cancellation, and close.

Agent Host remains authoritative for:

- `AgentSession` and `AgentRun` identity and lifecycle;
- proposal snapshots and work claims;
- GameEngine permission policy and approval scopes;
- authoring and code-workspace authority;
- validation, Play, persistence, and audit; and
- final completion gates.

The native GameEngine model/tool loop remains separate during this migration. A
local agent that exposes ACP may later register through the same ACP boundary.

### 2. Registration is descriptor-driven

The foundation defines `AcpAgentDescriptor` with:

```text
id
executable
arguments
capabilities
runtime_identity
```

The descriptor ID is registry data. Agent Host does not gain `Codex`, `Claude`,
`Goose`, or similar variants in a central ACP enum.

A provider adapter implements `AcpAgentRuntime` and registers with
`AcpRuntimeRegistry`. `AcpAgentRegistry` is the host-facing lookup interface.
Adding an ACP-compatible agent must not require changing central host match
statements solely to recognize a provider name.

The existing `ExternalAgentProviderKind` remains a compatibility surface while
providers are migrated. This ADR does not remove the current provider path.

### 3. ACP SDK types terminate at this boundary

GameEngine-owned runtime contracts do not expose ACP SDK types. Wire negotiation
and SDK/schema conversion belong to concrete adapters.

The first transport implementation should use the official
`agent-client-protocol` Rust runtime crate and negotiate stable ACP protocol v1.
A Rust SDK artifact version is not a wire-version contract. Draft protocol-v2
APIs and unstable MCP-over-ACP transport are not foundation requirements.

This foundation performs no ACP wire I/O. It therefore registers the internal
module without adding an unused third-party dependency or causing parallel
`Cargo.lock` churn. The first concrete ACP transport slice owns adding and
pinning the official SDK dependency.

### 4. Capabilities and identity are normalized

`AcpCapabilities` records the negotiated capabilities that affect GameEngine
orchestration, including session load/resume/list, session configuration, and
MCP transport support. Unknown optional capabilities may remain extension data.

`AcpRuntimeIdentity` records negotiated protocol version plus agent name and
version. Identity is compatibility/diagnostic evidence, not authorization.

### 5. ACP sessions are bound to GameEngine authority

`AcpSessionBinding` binds an ACP session to the authoritative GameEngine session
and, for write-capable work, the exact AgentRun ID. It carries one ephemeral
Editor MCP connection.

Only these ACP MCP access levels are valid:

```text
ReadOnly
AgentRunBoundReadWrite
```

They map to the current Editor transport as follows:

- `ReadOnly` uses `EditorMcpAccess::ReadOnly` and carries no write-capable run
  identity.
- `AgentRunBoundReadWrite` uses `EditorMcpAccess::AgentRunBound` and requires the
  matching GameEngine run ID on mutating requests as required by ADR 0165.
- unrestricted `EditorMcpAccess::ReadWrite` is never handed to an ACP runtime.

The MCP bearer credential is ephemeral process state. It is not serialized into
sessions, project history, provider configuration, or audit messages, and its
debug representation must be redacted.

### 6. ACP permission requests do not create authority

ACP permission requests normalize into `AcpPermissionRequest`. ACP tool-call
instance IDs are correlation identifiers only and never authorize an operation.
ACP `ToolKind` remains a coarse category; it is not a stable GameEngine tool
identity.

For GameEngine MCP calls, the ACP adapter may retain a provider-declared stable
tool name from trusted provider metadata attached to the matching tool-call
instance. The common transport normalizes that name back to the exact registered
Editor MCP inventory before Agent Host may resolve it. A claimed GameEngine MCP
name that is absent from the registered inventory fails closed. The resulting
read/write classification comes from the shared MCP tool contract rather than
from the ACP display title, opaque call ID, or a provider option name.

Non-MCP operations still classify into an existing `AgentCapability`. Provider
option names do not create new GameEngine capabilities or approval scopes.
Read-only ACP sessions may automatically select one-shot approval only for an
exact registered non-mutating GameEngine MCP tool; run-bound sessions may do so
only for an exact registered GameEngine MCP tool under the existing
AgentRun-bound MCP authority. The MCP credential, exact run identity, work claim,
and canonical authoring checks remain authoritative at execution time.

Host policy/classification rejection is distinct from an explicit user denial.
When the agent offers a reject option, a host-side rejection selects that option
and records a host diagnostic; `Cancelled` is reserved for an actual user
denial/cancellation or for a protocol request that offers no safe reject
representation. A request that cannot be safely classified always fails closed.

### 7. ACP updates are normalized before entering Agent Host

`AcpNormalizedEvent` covers semantic agent messages, progress, plans, tool-call
state, permission requests, session information, prompt stop reasons, and
protocol diagnostics. These project onto the existing `AgentEventKind`
vocabulary instead of creating a second host event model.

Malformed or unknown ACP messages become explicit protocol diagnostics and do
not invent semantic success.

### 8. ACP turn completion is not GameEngine completion

An ACP prompt response or stop reason means only that the agent returned control
for that prompt. It maps to semantic progress, not
`AgentEventKind::Completion`.

Only Agent Host may mark a run complete through the existing completion report
and `AgentHost::complete_run`. An ACP adapter cannot mark acceptance,
authoring/source validation, Play, frame capture, visual evaluation, or
interaction gates successful merely because the agent says it is done.

When an ACP provider returns control while a run is still `Executing` or
`Repairing`, Agent Host must either admit the run to managed validation because
all provider-side completion gates are satisfied, or terminate the run as
failed with the unsatisfied gate recorded. A run already in `AwaitingUser`
remains user-blocked. No timeout is used to manufacture completion, and an
`end_turn` with failed or unresolved gates must not leave the outer AI Studio
run permanently executing.

### 9. Provider adapters register at one seam

Each migrated provider implements `AcpAgentRuntime` and returns an
`AcpAgentSession` bound to its immutable `AcpSessionBinding`. Session
implementations expose prompt delivery, non-blocking normalized event polling,
permission response delivery, cancellation, and close.

Adapters may own executable discovery, provider authentication, launch
arguments, environment placement, and provider diagnostics. They do not own
authoring semantics, GameEngine permission policy, work-claim policy,
completion gates, or canonical project persistence.

### 10. Managed Local Agent Harness benchmarks normally use Goose ACP

The Benchmark Campaign surface distinguishes the model being measured from the
agent harness that drives it. For a Managed Local campaign, the normal and
recommended Agent Harness lane is:

```text
GameEngine -> Benchmark Campaign -> Goose Agent Harness -> ACP -> Managed Local model
```

The operator may explicitly choose the retained Legacy Native Harness for
compatibility or comparison evidence. Legacy is not an ACP recovery path. Once
a campaign selects Goose ACP, failure to discover Goose, failure to negotiate
ACP, an exact negotiated-runtime identity mismatch, an unavailable selected
Managed Local runtime/model, or a permission request outside the frozen budget
must fail closed and must not execute or record a Native run as ACP evidence.

The harness choice is part of campaign identity. Before `CampaignPolicy::freeze`
returns, a Goose selection is resolved to the existing provider-neutral
`BenchmarkRuntimeIdentity::gameengine_acp_agent_harness` identity using the
actually discovered `AcpRuntimeIdentity`. The resulting runtime identity is
frozen through `CampaignPlan`, experiment execution identity, child run spec,
actual ACP execution, and terminal `BenchmarkRecord`. Adapter/version fields
that GameEngine cannot observe remain `Unavailable`; they are not inferred.
The negotiated session identity must match the frozen identity before measured
work is admitted.

The runtime-aware campaign policy remains the single source for both
`AgentProposal.requested_capabilities` and the headless permission budget, so an
ACP harness does not weaken Agent Host's rule that undeclared capabilities are
rejected.

`read_question_v1` and `visual_evaluation_v1` are not currently valid common-ACP
Agent Harness evidence. The former belongs to the Native provenance harness and
the latter requires host-captured image content that the common ACP boundary
does not yet carry. Goose ACP campaigns remove/disable them before freeze and
freeze rejects them again if reached programmatically.
`validation_repair_v1` remains supported through the existing initial validation
failure -> ACP repair -> managed revalidation path.

### Managed Local Goose executable lifecycle

Managed Local's ACP route treats Goose as a GameEngine-owned machine-local runtime
dependency rather than as a prerequisite the user must install manually. The
existing ADR 0155 Managed Local state root and runtime-artifact policy own the
pinned Windows Goose archive, verified archive digest, staged activation, active
installation record, and executable digest. The normal UI installs or repairs
that runtime without executing a remote installer script and without requiring
PATH or environment-variable changes.

All Managed Local Goose consumers use the same resolver. Its order is the
GameEngine-managed installation, an explicit persisted machine-local executable
override, `GAMEENGINE_GOOSE_EXECUTABLE`, PATH, then legacy home locations retained
for compatibility. Explicit overrides fail closed when invalid; a corrupt managed
installation may still yield to a valid explicit override. Every selected
candidate must report a parseable Goose version and support `goose acp` before it
becomes an ACP descriptor.

AI Studio and Benchmark Campaign both resolve through `GooseLocalAcpRuntime`; no
AI-Studio-only executable path exists. A missing or broken Goose runtime is an
ACP setup failure and never authorizes a Legacy Native fallback. This lifecycle
changes only machine-local adapter availability and does not transfer Agent Host,
permission, MCP credential, completion, or persistence authority to Goose.

## Consequences

- New ACP agents are registry entries rather than central architecture variants.
- Existing providers can migrate independently while the current path remains
  functional.
- ACP does not weaken Agent Host authority over permissions, MCP writer
  identity, claims, validation, or completion.
- The ACP SDK can evolve independently because SDK types terminate at the
  adapter boundary.
- The first concrete transport slice, rather than this contract-only slice,
  owns the SDK dependency and lockfile update.

## Verification

Focused tests cover:

- arbitrary descriptor IDs without a central provider enum;
- duplicate descriptor rejection;
- read-only versus AgentRun-bound MCP bindings;
- credential redaction; and
- ACP turn-finished projection to semantic progress rather than host completion.

Concrete adapter slices add transport/protocol tests when they begin ACP I/O.

## Alternatives Considered

**Keep one custom harness per coding agent.** Rejected as the target
architecture because session, event, permission, and cancellation behavior would
remain duplicated.

**Replace Agent Host with an ACP client.** Rejected because ACP does not own
GameEngine writer identity, managed workspaces, claims, validation, persistence,
audit, or completion policy.

**Use one central provider enum.** Rejected because every future agent would
require a central architecture change.

**Give ACP agents unrestricted Editor MCP credentials.** Rejected by the
run-bound credential contract in ADR 0165.

**Treat an ACP stop reason as completion.** Rejected because it cannot prove
GameEngine completion gates.

## Compatibility and Migration

No canonical authoring schema, stable ID, MCP tool name, CLI command, or runtime
ECS contract changes.

`AgentSession`, `AgentRun`, `AgentEvent`, and their persisted schema versions do
not change in this foundation. ACP session bindings and registry entries are
process-local application contracts.

Current Claude Code, Codex, and generic external provider implementations remain
available until ACP replacements are integrated and verified. Migration must
preserve ADR 0165 read-only/run-bound MCP credentials, provider authentication
ownership, cancellation behavior, and restart recovery.

The module is internal to `engine-editor`. Moving it to a dedicated GUI-free
workspace crate later is allowed only if the Agent Host authority and
provider-neutral contracts in this ADR remain intact.
