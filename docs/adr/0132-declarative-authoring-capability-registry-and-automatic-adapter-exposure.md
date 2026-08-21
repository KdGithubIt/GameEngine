# ADR 0132: Declarative Authoring Capability Registry and Automatic Adapter Exposure

Status: Accepted
Date: 2026-08-16
Amends: ADR 0121
Relates to: ADR 0015, ADR 0016, ADR 0035, ADR 0131

## Context

ADR 0121 establishes semantic parity between the human Editor, MCP, and CLI by
requiring all of them to use shared GUI-free authoring services, commands,
queries, validation, and transactions. The current implementation already shows
two useful shapes. Scene mutation is generic enough that a newly registered
component schema can be discovered and edited through existing Scene commands,
while some domains such as Behavior Tree still expose manually declared MCP and
CLI operations for domain-specific behavior.

Manually adding an Editor path, then separately adding an MCP tool descriptor,
handler, CLI command, schema mapping, and coverage test for every future
semantic feature does not scale. It also turns AI parity into a convention that
can drift: a feature may work in the Editor while the structured AI surface is
forgotten until later.

The opposite extreme is also unsafe. Automatically exporting every Rust
function, reflected type, Editor widget, or public method would expose
implementation details without a stable semantic contract, permission model,
transaction boundary, or machine-readable schema. AI accessibility must follow
from an explicit authoring contract, not from raw code reflection.

A project-wide decision is therefore required for the single registration point
of authoring capabilities, the generic MCP and CLI exposure model, the boundary
between automatically exposed operations and specialized adapter extensions,
and the parity checks required when new features are added.

## Decision

### 1. The authoring layer owns one canonical capability registry

`engine-authoring` owns a GUI-free **Authoring Capability Registry** describing
semantic authoring operations that external clients may discover and invoke.
The registry is the canonical discovery contract for Editor orchestration, MCP,
CLI, tests, and future structured clients.

Each registered capability MUST have a stable capability identifier and enough
machine-readable metadata to describe its semantic contract. A capability
description MUST identify, directly or through referenced shared schemas:

- its stable capability ID;
- its authoring domain or document kind;
- whether it is a read query, validation operation, previewable mutation, or
  committed mutation;
- its input and output schema or shared command/query type;
- the permission required to execute it;
- its transaction and stale-revision requirements when it mutates data; and
- a human- and AI-readable description.

The registry describes authoring meaning. It MUST NOT contain GUI toolkit types,
MCP protocol types, CLI argument parser types, provider credentials, or runtime
ECS mutation behavior.

### 2. A semantic authoring capability is declared once

A new semantic feature MUST NOT require separate business-meaning declarations
for Editor, MCP, and CLI. The authoring owner declares the capability once at
the shared service, command, query, or schema boundary and adapters consume that
declaration.

This means that adding a new component type to the existing component schema and
Scene command model does not require a new MCP handler for that component. Once
the component is registered in the shared schema registry, generic schema
discovery and Scene mutation capabilities make it available to structured AI
clients automatically.

A genuinely new authoring domain still requires an explicit shared service or
command/query contract and one capability registration. The required work is to
define the engine's authoring semantics, not to reproduce those semantics in an
AI-specific path.

### 3. MCP and CLI provide registry-driven generic authoring surfaces

MCP MUST expose a generic structured surface that can discover registered
capabilities and invoke the common query, validate, preview, and apply shapes
without requiring one handwritten MCP tool per ordinary authoring operation.
CLI MUST provide an equivalent generic structured path where headless use is
meaningful.

The exact transport spelling may evolve, but the generic contract MUST support
operations equivalent to:

```text
authoring.list
authoring.capabilities
authoring.describe
authoring.inspect
authoring.validate
authoring.preview
authoring.apply
```

`authoring.list` is the preferred context-efficient discovery operation. It
returns only registry-derived selection metadata such as stable ID, domain,
kind, exposure, and description. Once a client selects an operation,
`authoring.describe` returns that capability's full schema, permission,
transaction, and document contract. `authoring.capabilities` retains the full
registry response as a compatibility surface for existing clients; new agent
flows SHOULD use `authoring.list` -> `authoring.describe` instead of loading
every schema eagerly.

Tool descriptors, input schemas, capability descriptions, and permission
requirements for this generic surface MUST be derived from the authoring-owned
registry or the shared schemas referenced by it. Compact summaries MUST also be
projected from the same registry rather than maintained as an adapter-specific
catalog. MCP MUST NOT maintain a second handwritten capability catalog that can
disagree with the authoring registry.

Existing domain-specific MCP and CLI commands remain valid compatibility and
ergonomic surfaces. They MAY delegate to the same registered capability rather
than being removed immediately.

### 4. Automatic exposure does not mean raw reflection

Rust visibility, `Reflect` metadata, derive output, Editor widgets, menu items,
or public methods are not sufficient evidence that an operation is safe to
expose. Only an explicitly registered authoring capability is eligible for the
generic structured adapter surface.

Reflection and derives MAY help construct schemas or registration metadata, but
they MUST resolve to the same explicit capability contract and permission
boundary. Internal helpers and implementation-only functions MUST NOT become AI
tools merely because they are discoverable in code.

### 5. Specialized operations remain explicit extensions

Some operations have domain meaning that is not usefully represented as a
generic document inspect/validate/preview/apply cycle. Examples include Behavior
Tree compilation, graph auto-layout, asset import, build/package operations,
runtime control, and frame capture.

A specialized MCP or CLI operation MAY exist when it provides meaningful
semantics or ergonomics beyond the generic authoring surface. Such an extension
MUST:

- call the authoritative shared service rather than duplicate business rules;
- reference or compose registered capability IDs when it performs authoring
  work;
- preserve shared permissions, validation, stale-revision, and transaction
  rules; and
- have adapter-equivalence tests when another adapter exposes the same intent.

Runtime input and frame observation remain ADR 0035 AI Agent Bridge concerns.
Agent orchestration, code workspace mutation, shell execution, network access,
and provider lifecycle remain ADR 0131 concerns and MUST NOT be smuggled into
the authoring capability registry merely to make them automatically callable.

### 6. Registry metadata is descriptive, not authority

Discovering a capability does not grant permission to execute it. Permission
checks remain enforced at the shared authoring or application boundary defined
by ADR 0121 and ADR 0131.

Adapters MAY hide capabilities that are impossible in the current host, but they
MUST NOT weaken shared authorization because a capability appears in the
registry. A mutation that requires `ProjectDataWrite`, for example, remains
rejected for a read-only MCP session even if its schema is discoverable.

### 7. Capability coverage becomes a CI contract

Parity guardrails MUST iterate the canonical registry rather than compare
independently maintained lists of Editor buttons and MCP tools.

For each semantic authoring capability intended for interactive Editor use,
tests MUST prove one of the following:

- it is reachable through the standard registry-driven structured AI surface;
- it is covered by an explicitly declared specialized MCP path over the same
  shared semantics; or
- an Accepted ADR explicitly scopes that capability out of structured AI
  authoring.

New authoring capabilities that lack one of those outcomes MUST fail parity
coverage validation. The same registry SHOULD drive capability reporting from
`project.describe` or its successor so runtime discovery and CI coverage use the
same source of truth.

### 8. Stable capability identity is an adapter contract, not project data

Capability IDs are stable external authoring API identifiers. Renaming an
implementation function, moving a crate module, or changing Editor presentation
MUST NOT silently rename a capability ID.

Capability registry metadata is engine/application metadata and MUST NOT be
serialized into Scene, Graph, Material, Prefab, UI, Animation Set, project
settings, or other canonical project documents merely to support adapter
discovery. Project-defined schemas may contribute capabilities at runtime, but
canonical project data continues to store the authored content rather than a
copy of the engine's adapter registry.

### 9. Implementation proceeds from shared semantics outward

Implementation order is:

1. define the authoring-owned capability descriptor and registry;
2. register the existing generic Scene/entity/schema capabilities first;
3. add registry-driven MCP discovery and generic invocation;
4. add the equivalent generic CLI path where headless operation is meaningful;
5. migrate existing domain-specific tools to reference the registry without
   breaking their current names;
6. register Prefab, generic Graph, UI, Material, Project Settings, Animation Set,
   and future typed document services as those shared services become complete;
   and
7. enable registry-based parity coverage as a required CI/test guardrail.

A domain MUST NOT be made "AI compatible" by adding an MCP-only mutation path
before its shared authoring service exists.

## Consequences

Most future authoring additions become AI-accessible as a consequence of being
implemented correctly at the shared authoring boundary. New component schemas
and other extensions already expressible through an existing generic command
model require no parallel MCP implementation. New domains pay one semantic
integration cost in `engine-authoring`, after which generic adapters discover
them from the registry.

MCP and CLI code become thinner and less likely to drift from the Editor. AI
agents can inspect the live capability inventory instead of relying on a fixed
prompt-time tool list maintained separately from the engine.

The registry itself becomes a public internal contract that requires stable IDs,
schema quality, permission metadata, deterministic discovery, and tests. Some
specialized operations will continue to need explicit adapter endpoints, so the
design reduces handwritten adapter work rather than claiming every possible
operation can be represented by one generic function.

The authoring layer gains registration metadata, but it does not gain MCP
transport, CLI parsing, provider, network, shell, GUI, or runtime-control
responsibilities.

## Alternatives Considered

### Continue adding one MCP and CLI handler for every Editor capability

Rejected. It duplicates registration work and makes parity dependent on every
future contributor remembering several adapter-specific follow-up changes.

### Export every reflected or public Rust operation automatically

Rejected. Rust visibility and reflection do not define semantic stability,
permissions, transactions, diagnostics, or safe external inputs. This would
turn implementation details into accidental external APIs.

### Generate AI tools from Editor widgets and menus

Rejected. Widget structure is presentation state. One semantic operation may be
reachable from several widgets, and some authoring operations have no dedicated
button. AI parity is semantic rather than widget-for-tool mirroring per ADR
0121.

### Make MCP call the CLI to avoid duplicate handlers

Rejected. ADR 0121 already forbids routing MCP authoring through argv/stdout.
Both adapters must converge on the same shared authoring registry and services.

### Keep only domain-specific ergonomic tools and omit a generic surface

Rejected as the default. Specialized tools remain useful, but making them the
only path would preserve the requirement to write AI-specific code for every
new domain and would prevent true capability discovery.

## Compatibility and Migration

This decision does not change canonical project serialization, stable project
IDs, existing authoring command semantics, or the project-scoped MCP lifecycle.

Existing MCP and CLI tool names remain valid during migration. The
registry-driven generic surface is additive. Existing domain-specific handlers
may be reimplemented as thin wrappers over registered capabilities without
changing their external names or result meaning.

Capability IDs introduced by the registry become stable adapter contracts once
published. Migration from current handwritten capability lists must preserve
existing behavior and prove adapter equivalence before removing duplicated
registration data.

ADR 0121 remains authoritative for MCP lifecycle, live Editor ownership,
transaction boundaries, and semantic parity. This ADR strengthens ADR 0121's
capability-registry guidance into the required default mechanism for future
semantic authoring exposure.
