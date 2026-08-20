# AI-Friendly Authoring and Graph System Specification

Status: Draft
Version: 0.2.0
Canonical location: `docs/AI_FRIENDLY_AUTHORING_SPEC.md`

Rust implementation and documentation style is defined separately in
`docs/RUST_CODE_STYLE.md`.

## 1. Purpose

This document defines the shared implementation contract for the GameEngine
authoring system. It is intended to be read by human contributors, Codex,
Claude, and other tools before they modify authoring, editor, graph, CLI, or
MCP-related code.

The engine is not merely an editor that can be operated by AI. Its authoring
model is designed so that humans, AI agents, CLI tools, scripts, and a future
visual editor can safely edit the same project data through the same typed and
transactional operations.

Product statement:

> A data-oriented game engine where humans, AI agents, and code collaborate
> through the same typed, transactional editing model.

Short design principle:

> Humans and AI edit intent. The engine maintains structure, validity, and
> presentation.

## 2. Normative Language

The words MUST, MUST NOT, SHOULD, SHOULD NOT, and MAY are normative.

- MUST: required for compatibility with this specification.
- SHOULD: recommended unless a documented reason exists.
- MAY: optional and implementation-dependent.

If code and this specification disagree, contributors MUST either update the
code to match the specification or explicitly revise the specification before
depending on the new behavior.

## 3. Goals

The authoring system MUST provide:

1. Stable, text-based project data that can be reviewed in Git.
2. A shared editing API for the visual editor, CLI, scripts, MCP, and tests.
3. Typed schemas that allow tools to discover valid components, nodes, ports,
   properties, constraints, and defaults.
4. Transactional edits with validation, previewable diffs, commit, rollback,
   undo, and redo.
5. A strict separation between editable authoring data and the optimized
   runtime ECS world.
6. Graph data that is easy for AI to modify without requiring pixel-coordinate
   reasoning.
7. Human-readable graph presentation that can be regenerated and incrementally
   improved by the engine.
8. Deterministic serialization and diagnostics suitable for automation.

## 4. Non-Goals

The first implementation does not aim to provide:

1. A complete general-purpose visual programming language.
2. Real-time multi-user collaborative editing.
3. A single universal compiler for every graph domain.
4. Direct serialization of the runtime ECS world.
5. AI-specific project file formats.
6. A requirement that all project data changes go through MCP.
7. Automatic resolution of every Git merge conflict.

## 5. System Architecture

The required architecture is:

```text
 Human Editor       AI / MCP       CLI / Scripts       Tests
       \                |                |               /
        +---------------+----------------+--------------+
                                |
                     Command and Query API
                                |
                       Transaction Layer
               validate / diff / undo / migrate
                                |
                      Authoring Data Model
              scenes / entities / graphs / assets
                                |
                         Build Pipeline
                                |
                       Runtime ECS World
```

All authoring clients MUST use the same command and query semantics. An editor
button, CLI command, and MCP tool that perform the same action MUST produce the
same authoring command or equivalent command sequence.

### 5.1 Required Separation

The existing ECS `Command` type is a runtime deferred mutation mechanism. It
MUST NOT become the authoring edit API.

Use separate concepts:

```rust
// Runtime concern: fast deferred mutation of World.
trait RuntimeCommand {
    fn apply(self: Box<Self>, world: &mut World) -> Result<(), WorldError>;
}

// Authoring concern: validated, reversible mutation of project data.
enum AuthoringCommand {
    // Variants are defined by the authoring model.
}
```

The runtime trait MUST be named `RuntimeCommand` as decided in ADR 0001.
Authoring code MUST use `AuthoringCommand` or an equivalent unambiguous name.

### 5.2 Build Pipeline Boundary

The build and play pipelines convert validated authoring data into runtime ECS
state. During conversion:

- `AuthoringEntity` values are converted to runtime `Entity` values.
- `AssetId` values are resolved to `RuntimeAssetId` values.
- An explicit authoring-to-runtime mapping is maintained for the duration of
  the play or build session.

This mapping MUST be discarded when the play session ends. Runtime `Entity`
and `RuntimeAssetId` values MUST NOT be persisted in project files.

The runtime ECS crate MUST NOT depend on authoring types. Runtime-domain
ownership and the final `engine` composition boundary follow ADR 0113. Open
Decision #6 is limited to the packaged build-artifact format and related build
pipeline policy that ADR 0113 does not define.

### 5.3 Authoring-First Game Creation

When a contributor or AI agent is asked to create a game, playable sample,
demo, level, or prototype for this engine, the primary deliverable MUST be an
editor-openable project that uses the standard project layout, including
`project.json`, `asset_manifest.json` when assets are referenced, and project
assets such as `assets/scenes/*.scene.json`.

Creating only a Rust example, standalone binary, or runtime-only world setup is
not sufficient unless the user explicitly asks for a code-only experiment.

Game-specific runtime code MAY be added when the current authoring model cannot
express the required behavior yet, but that runtime code MUST be reachable from
authoring data and runnable from editor Play through a documented bridge such
as registered authorable components, marker components, scripts, graphs, or
project settings. Standalone examples MAY exist as secondary launchers, but
they MUST share the same runtime implementation and MUST NOT be the only path
to the game.

### 5.4 Project Application Lifecycle

Project selection and editor process lifetime are application concerns, not
authoring edit semantics. ADR 0117 defines the project-first desktop boundary:

- the Launcher / Project Manager selects or creates projects and starts or
  activates project-scoped Editor processes;
- an Editor workspace always has a concrete `ProjectRoot` and edits one project
  location for that process lifetime;
- `engine-project-lifecycle` owns GUI-free project acquisition, standard
  project scaffolding, exclusive editor leases, ephemeral ownership metadata,
  and project/editor compatibility checks above `engine-authoring`; and
- recent-project and editor workspace restoration state remain user data and
  MUST NOT enter canonical project authoring files.

`ProjectRoot`, `ProjectConfig`, `project.json`, authoring commands, persisted
authoring schemas, and path confinement remain owned by `engine-authoring` as
defined by ADR 0023.

## 6. Crate and Module Boundaries

The current workspace is decomposed into runtime domain packages plus
application and authoring packages. ADR 0113 defines the runtime ownership
target, ADR 0114 defines heavyweight backend isolation, ADR 0117 defines the
project application lifecycle, and ADR 0121 defines AI/MCP application
integration.

```text
crates/
  ecs/                 # Runtime ECS only
  core/                # Shared runtime primitives and small contracts
  assets/              # Runtime asset contracts and format-neutral model IR
  rig/                 # Transform, skeleton, skin, pose, and rig contracts
  animation/           # Clips, animator, animation graph, pose, retargeting
  physics/             # Collision, controller, navigation, solver boundary
  render-runtime/      # Runtime mesh/material/light/UI/render-pass behavior
  import/              # Format-neutral import orchestration and parsers
  gameplay/            # Ability, behavior, combat, hitbox, gameplay control
  platform/            # Native input, audio, gamepad, platform adapters
  scripting/           # Host-independent project scripting/game SDK contracts
  scene/               # Scene load/management, save, and replay ownership
  renderer/            # Low-level GPU context and optional surface integration
  engine/              # Final composition and compatibility facade
  authoring/           # Authoring data, schemas, commands, transactions
  project-lifecycle/   # GUI-free project application/process lifecycle
  launcher/            # Project selection/creation and Editor launch UI
  editor/              # Project-scoped human visual editor frontend
  cli/                 # Thin headless adapter over shared authoring services
  mcp/                 # Transport-agnostic AI authoring tool handlers
  game-*-macros/       # Project Rust derive/attribute support
```

The package list is not itself a complete dependency graph. The normative
ownership and dependency rules are:

- `ecs` MUST NOT depend on editor, MCP, serialized authoring types, or higher
  runtime domains.
- No runtime domain package below `engine` may depend on `engine`, and circular
  workspace package dependencies are forbidden.
- `renderer` owns low-level GPU context and optional window-surface integration.
  `render-runtime` MAY depend on it; `renderer` MUST NOT depend on
  `render-runtime`, `engine`, authoring, editor, CLI, or MCP.
- `rig` owns transform hierarchy primitives, skeleton identity, skin binding,
  layered rig poses, and rigid-body rig descriptions. It MUST NOT depend on
  `engine`, renderer presentation, model importers, audio, or a physics solver.
- `assets` and `core` MUST keep neutral contracts below higher runtime
  implementations instead of importing format-specific or backend-specific
  types to preserve an old implementation shape.
- Heavy GPU, physics, windowing, audio, gamepad, GUI, and scripting-runtime
  backends MUST remain at the owner or final composition boundary defined by
  ADR 0114. Consumers that need only neutral contracts SHOULD disable those
  backend features rather than inheriting them transitively.
- `engine` is the final runtime composition and supported compatibility facade.
  It MAY contain concrete cross-domain host adapters that would create an
  upward dependency or cycle if pushed into a lower domain.
- `authoring` owns persisted authoring semantics and MUST NOT depend on a GUI
  framework. Domain-neutral graph storage and graph transaction contracts
  remain authoring-owned rather than requiring separate graph workspace crates.
- `project-lifecycle` MAY depend on `authoring` for `ProjectRoot` and shared
  creation services. It MUST NOT own authoring command semantics, runtime ECS
  behavior, Launcher UI, or Editor GUI state.
- `launcher` owns project-selection UI, recent-project application state, and
  Editor process launch/activation. It MUST NOT implement unique authoring
  rules.
- `editor` is project-scoped after bootstrap and MUST use shared authoring and
  project-lifecycle contracts rather than duplicating them.
- `cli` and `mcp` MUST remain adapters over shared GUI-free authoring services.
  They MUST NOT contain unique editing logic.

Compatibility facade modules or cross-domain composition code may remain in
`engine` where ADR 0113 permits them. Their presence does not transfer ownership
of the underlying domain contract back into the facade.

## 7. Authoring Data Model

Authoring data is the editable source of truth. Runtime data is derived from it.

### 7.1 Stable Identifiers

All persisted authoring objects MUST use stable identifiers.

```rust
struct StableId(String);
struct ProjectId(StableId);
struct EntityId(StableId);
struct GraphId(StableId);
struct NodeId(StableId);
struct EdgeId(StableId);
struct PortId(StableId);
struct GroupId(StableId);
struct AssetId(StableId);
```

Stable identifiers use the format `<prefix>_<ULID>` where the prefix
identifies the object kind. ULID provides monotonic, time-ordered uniqueness
without coordination.

| Identifier  | Prefix    | Uniqueness scope     |
| ----------- | --------- | -------------------- |
| `ProjectId` | `project_` | Logical project      |
| `EntityId`  | `entity_` | Project-wide         |
| `AssetId`   | `asset_`  | Project-wide         |
| `GraphId`   | `graph_`  | Project-wide         |
| `NodeId`    | `node_`   | Graph-local          |
| `EdgeId`    | `edge_`   | Graph-local          |
| `PortId`    | `port_`   | Node-schema-local    |
| `GroupId`   | `group_`  | Graph-local          |

Stable identifiers:

- MUST remain valid across editor sessions, builds, and runtime entity changes.
- MUST NOT be based on runtime entity indices, memory addresses, array offsets,
  or display positions.
- MUST be unique within their documented scope.
- MUST NOT change when an object is renamed.
- MUST be treated as opaque by consumers. The format is an implementation
  detail, not a parsing contract.

Human-readable and AI-readable information is stored in mutable fields
separate from the identifier:

- `name`: short lowercase slug, e.g. `player_entity`. Used for search.
- `display_name`: UI label visible to humans.
- `description`: extended description for documentation and AI context.

AI agents and CLI tools SHOULD search by `name`, `type`, or description to
locate objects. They MUST use the `StableId` for the final edit target.

Runtime identifiers MUST use names that make their ephemeral scope explicit,
such as `Entity` and `RuntimeAssetId`. Runtime identifiers MUST NOT be exposed
as persisted `EntityId` or `AssetId` values.

### 7.2 Authoring Entity

```rust
struct AuthoringEntity {
    id: EntityId,
    name: String,
    display_name: String,
    description: String,
    parent: Option<EntityId>,
    components: BTreeMap<ComponentTypeId, Value>,
}
```

The `id` field is the stable project-wide identifier and MUST NOT change
when the entity is renamed. The remaining human-readable fields are mutable
and independent of `id`, consistent with the separation described in §7.1:

- `name`: Short lowercase slug used by AI agents and CLI tools for search.
- `display_name`: Human-readable label shown in editor UI.
- `description`: Extended documentation and AI context for the entity.

The `name`, `display_name`, and `description` fields MUST always be present
in canonical serialized output, even when empty. Per ADR 0115, input that
predates these required fields is obsolete rather than a compatibility shape;
the current reader MUST NOT infer missing values solely to load an older writer.

The runtime build or play pipeline converts `AuthoringEntity` values into
runtime ECS entities. A runtime `Entity` ID MUST NOT be persisted as an
authoring entity reference.

### 7.3 Generic Value

Authoring properties MUST use a typed generic value model or an equivalent
structured representation.

```rust
enum Value {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
    EntityRef(EntityId),
    AssetRef(AssetId),
}
```

The implementation MAY add variants, but MUST preserve explicit entity and
asset reference semantics. References MUST NOT be represented as ambiguous
plain strings after schema validation.

### 7.4 Value Serialization Policy

`EntityRef` and `AssetRef` values MUST be serialized using explicit tagged
objects. Plain strings MUST NOT be used to represent references.

Canonical tagged forms:

```json
{ "$type": "entity_ref", "id": "entity_01JZZZZZZZZZZZZZZZZZZZZZZZZ" }
{ "$type": "asset_ref",  "id": "asset_01JZZZZZZZZZZZZZZZZZZZZZZZZ" }
```

All other `Value` variants serialize as their natural JSON equivalents:
`null`, `true`/`false`, numbers, strings, arrays, and objects.

Serialization MUST:

- Write `$type` before `id` in tagged objects.
- Use only two keys in a tagged object.
- Use the `StableId` string directly as the `id` value.

Schema-guided validation SHOULD detect plain strings in positions that expect
an `EntityRef` or `AssetRef` and produce a diagnostic with actionable
suggestions when loading direct file edits.

### 7.5 Component Type Identifier

A `ComponentTypeId` identifies a component type or schema definition.
It is distinct from an authoring object [`StableId`]. Engine-owned IDs may be
readable dotted names, while newly generated project component IDs use an
opaque ULID segment as described below.

```rust
struct ComponentTypeId(String);
```

A component type ID:

- MUST be a stable string that does not change after a component type is used
  in project files.
- MUST be unique across all component types registered in a project.
- SHOULD use lowercase dotted namespace notation, for example `gameplay.health`,
  `transform.local`, or `render.mesh`. The namespace prefix groups related
  components and prevents naming conflicts between subsystems.
- MUST use `game.c_<lowercase Crockford ULID>` when generated for a new
  project-local Rust component. This is still a `ComponentTypeId`, not an
  instance `StableId`; the opaque suffix prevents distributed creation
  collisions and is generated only once.

A `ComponentTypeId` identifies the component type. It is not an identifier
for a specific component instance on an entity.

Engine-owned render-light component IDs are stable authoring contracts.
`engine.directional_light` and `engine.ambient_light` preserve their existing
global-light semantics. `engine.point_light` and `engine.spot_light` are
finite-range local lights whose runtime position comes from the owning entity's
transform; spot-light orientation is the transformed local `-Z` axis. Their
renderer budgets and shading semantics are defined by ADR 0129.

Project Rust component identity is stored in a JSON sidecar beside its source:
`player.rs.meta.json` for `player.rs`. Sidecar schema version 1 contains only
`schema_version` and `component_id` and MUST NOT contain an absolute path. The
sidecar is authoritative and moves with the source without changing its ID.
Duplicating a component MUST generate a new ID. The sidecar is the only
identity source: `#[game_component(id = "...")]` is rejected, and a missing or
invalid sidecar is an error that MUST NOT cause automatic replacement-ID
generation (ADR 0091).

### 7.6 Unified User Script Layout

User-authored code is project content and MUST appear in the same physical
Asset Browser tree as ordinary assets. Its canonical layout is:

```text
assets/scripts/rhai/**/*.rhai
assets/scripts/rust/**/*.rs
```

`game/` is an internal Cargo build host, not a second user-code root. Generated
files below `game/src` MUST bridge to the asset sources and MUST NOT be
presented as editable user assets. Rust sources and their metadata sidecars are
source assets, not `asset_manifest.json` runtime entries.

Everything below `assets/scripts/rust/` is one free-form Rust module tree. The
folder path MUST map directly onto the generated Rust module path, so every
folder and file name MUST be a usable Rust module name. `components`,
`resources`, `systems`, and `shared` are created for new projects as recommended
default destinations and MUST NOT be enforced as categories. A source's kind
MUST be decided by its declarations — a `GameComponent` derive or
`#[game_component(...)]`, a `GameResource` derive or `#[game_resource(...)]`, or
`#[game_system(...)]` — and never by its folder; a source with none of them is
an ordinary compiled module (ADR 0092).

Movement below the Rust script root MUST be free, and a Component source with
its `.rs.meta.json` sidecar MUST move as one unit so the stable component ID
survives. A `.rs` source MUST NOT leave `assets/scripts/rust/`, a `.rhai` source
MUST NOT leave `assets/scripts/rhai/`, and `mod.rs` MUST NOT exist below the
Rust script root because the module index is engine-generated. Rust `use` paths
are deliberately not rewritten on move. These rules do not change free movement
of regular Scene, Texture, Mesh, Audio, Material, Prefab, UI, or navigation
assets.

Rhai sources are recognized only under `assets/scripts/rhai`, and Rust user
sources only under `assets/scripts/rust`. Sources outside those paths are not
user code and MUST NOT be presented as such (ADR 0091).

`ComponentTypeId` appears as:

- The key in `AuthoringEntity.components`.
- The `type_id` field in `ComponentSchema`.
- The `component_type` field in `DiagnosticTarget::Component`.
- A reference in authoring commands that add, remove, or update components.

## 8. Schema and Reflection

AI-safe editing requires machine-readable meaning, not only Rust type names.

### 8.1 Component Schema

Each editable component MUST expose a schema containing at least:

- Stable component type ID
- Display name
- Description
- Category
- Field names and field types
- Required or optional status
- Default values when available
- Numeric or collection constraints when applicable
- Entity and asset reference types
- Authoring-only or runtime-applicable status
- Schema version

Example conceptual declaration:

```rust
#[derive(Component, Reflect, Serialize, Deserialize)]
#[component_schema(
    id = "gameplay.health",
    description = "An entity's hit points",
    category = "Gameplay",
    version = 1
)]
struct Health {
    #[field(min = 0, description = "Current hit points")]
    current: i32,

    #[field(min = 1, description = "Maximum hit points")]
    maximum: i32,
}
```

The exact derive and attribute implementation is not fixed by this
specification.

### 8.2 Graph Node Schema

Each graph node type MUST expose:

- Stable node type ID
- Graph domain or compatible graph kinds
- Display name
- Description
- Category and search tags
- Property schema
- Input port schemas
- Output port schemas
- Port value types
- Port multiplicity rules
- Optional default values
- Optional deprecation and migration metadata

Tools MUST be able to query schemas before creating or connecting nodes.

### 8.3 Engine-Native Secondary Motion Schema

ADR 0112 defines one engine-owned authoring component for cosmetic secondary
motion. Its current schema is:

| Property | Current contract |
| --- | --- |
| Component type ID | `engine.secondary_motion` |
| Schema version | 1 |
| Category | `Physics` |
| Field | `rig` |
| Field type | `AssetRef` constrained to `SecondaryMotionRig` |
| Default | Unassigned |

An unassigned `rig` is a valid authoring state. Assigning a Secondary Motion Rig
opts the entity into runtime Secondary Motion; merely importing a model that can
produce a rig does not make the imported character simulate automatically. The
serialized field value follows the tagged `AssetRef` form in §7.4.

When model import produces a generated Secondary Motion Rig, the imported
sub-asset kind serializes as `secondary_motion_rig`. Its deterministic imported
ID derives from prefix `secondarymotionrig`; the model-level generated rig uses
selector index 0. The former `rigid_body_rig` kind and `rigidbodyrig` derivation
namespace are not compatibility aliases and MUST NOT be silently interpreted as
the current kind (ADR 0112, ADR 0115).

### 8.4 Animation Motion Candidate and Humanoid Provenance Contract

ADR 0154 makes Animation Set motion bindings candidate-oriented and target-aware.
The current `*.animset.json` schema is version 3. Each primary binding and
overlay persists only the stable imported candidate `AssetId`; it MUST NOT
persist an Auto/Native/Humanoid route policy. `ImportedSubAssetKind::Animation`
identifies a model-bound candidate, while `HumanoidMotion` identifies the
explicit portable candidate. Schema-2 route-policy documents are obsolete
current-format input and MUST be rejected rather than silently reinterpreted.

For a concrete target skeleton, tools MUST use the shared GUI-free motion
planner. Model-bound candidates resolve in the fixed order Native -> explicit
Retarget Map -> logical Humanoid fallback -> Failed. An explicitly selected
HumanoidMotion resolves only Humanoid or Failed. Target Preview state and the
computed Native/Retarget/Humanoid/Failed result are transient derived state and
MUST NOT be serialized into the Animation Set. A Failed route is an authoring
error for each affected scene/controller target, is surfaced through Problems
before Play, and blocks Play/package preflight without blocking Animation Set
save.

Motion-only import settings keep concrete Native output targets in
`motion_model_sources`. The optional `motion_humanoid_source_model` is a
different stable provenance field: it names one associated model with a usable
Humanoid profile that import/reimport uses to generate the logical,
target-independent HumanoidMotion sibling. Humanoid is never inserted as a fake
`motion_model_sources` target. Reordering or adding concrete output targets MUST
NOT silently change persisted Humanoid provenance, and changing provenance MUST
regenerate portable motion content and invalidate dependent derived bakes while
keeping the logical Humanoid candidate identity target-independent. Runtime,
Editor preview/Problems, and packaging MUST consume imported portable motion;
they MUST NOT create it lazily as a side effect of resolving a binding.

## 9. Domain-Neutral Graph Model

Shader Graph, Behavior Tree, Animation Graph, and future graph types MUST share
a common storage and editing foundation without sharing domain-specific
validation or compilation logic.

```rust
struct Graph {
    id: GraphId,
    kind: GraphKind,
    schema_version: u32,
    nodes: BTreeMap<NodeId, Node>,
    edges: BTreeMap<EdgeId, Edge>,
    groups: BTreeMap<GroupId, Group>,
    annotations: Annotations,
}

struct Node {
    id: NodeId,
    node_type: NodeTypeId,
    name: Option<String>,
    properties: Value,
    annotations: Annotations,
}

struct Edge {
    id: EdgeId,
    from: PortRef,
    to: PortRef,
    annotations: Annotations,
}

struct PortRef {
    node: NodeId,
    port: PortId,
}
```

### 9.1 Semantic Data and Presentation Data

Graph semantics and graph presentation MUST be stored separately.

Semantic graph data includes:

- Nodes
- Edges
- Node properties
- Domain kind
- Groups that have semantic meaning
- Comments or annotations that explain intent

Presentation data includes:

- Node position
- Group bounds
- Collapsed state
- Zoom or viewport state
- Edge routing hints
- Pinned state

```rust
struct GraphView {
    graph: GraphId,
    layout_policy: LayoutPolicyId,
    nodes: BTreeMap<NodeId, NodeLayout>,
    groups: BTreeMap<GroupId, GroupLayout>,
}

struct NodeLayout {
    position: Vec2,
    pinned: bool,
    collapsed: bool,
}
```

Semantic graph data MUST remain valid if all presentation data is deleted.
Presentation data MUST be regenerable by an auto-layout implementation.

AI tools SHOULD edit semantic graph data and layout intent. They SHOULD NOT be
required to calculate pixel coordinates.

### 9.2 Domain Contract

Each graph domain MUST implement behavior equivalent to:

```rust
trait GraphDomain {
    fn node_schema(&self, node_type: &NodeTypeId) -> Option<NodeSchema>;
    fn validate(&self, graph: &Graph) -> Vec<Diagnostic>;
    fn compile(&self, graph: &Graph) -> Result<CompiledGraph, Vec<Diagnostic>>;
    fn layout_policy(&self) -> LayoutPolicyId;
}
```

Domain-neutral graph code owns:

- Node, port, edge, group, and view storage
- Stable IDs
- Serialization
- Commands and diffs
- Undo and redo support
- General structural validation
- Layout constraints and layout invocation

Each graph domain owns:

- Available node types
- Connection rules
- Type checking
- Domain-specific validation
- Compilation or interpretation
- Domain-specific diagnostics
- Default layout policy

### 9.3 Runtime Graph Debugging

ADR 0138 keeps runtime Graph Debug presentation aligned with the same
domain-neutral foundation without moving runtime semantics into `Graph`.
Play-mode Graph Debug owns common source-graph resolution, Graph Canvas
interaction, `NodeId`/`EdgeId` source mapping, target selection, framing,
read-only highlights, and stale-source presentation. Each runtime graph domain
supplies a provider backed by a GUI-free runtime observation snapshot.

Behavior Tree Running/Success/Failure/Abort semantics and Animation Graph
state/transition/parameter/motion-resolution semantics remain domain-owned.
Debug snapshots, runtime target handles, active highlights, and debug history
MUST NOT be serialized into Graph or GraphView documents.

## 10. Graph Layout System

The graph layout system exists for human readability. It MUST preserve semantic
data and SHOULD preserve the user's spatial memory.

### 10.1 Layout Rules

Auto-layout MUST:

1. Never change nodes, edges, properties, or other semantic behavior.
2. Respect pinned nodes.
3. Prefer local or incremental movement after a local graph edit.
4. Penalize movement of existing nodes.
5. Reduce node overlap and edge crossings where practical.
6. Place newly created nodes near their semantic neighbors.
7. Produce deterministic output for identical graph, view, and policy inputs.

Auto-layout SHOULD support:

- `place_before`
- `place_after`
- `near`
- `same_lane`
- `inside_group`
- `align`
- `layout_scope`

These are layout constraints or hints, not semantic graph edges.

### 10.2 Domain Defaults

Default layout direction SHOULD be:

| Graph Domain | Default Layout |
| --- | --- |
| Behavior Tree | Parent above children; execution order left to right |
| Shader Graph | Inputs on the left; final outputs on the right |
| Animation Graph | Cluster states; reduce transition crossings |
| Event or Logic Graph | Events on the left; control flow toward the right |

A single layout engine MAY support multiple policies, but one policy MUST NOT be
assumed to be correct for every graph domain.

## 11. Authoring Commands

All mutations to loaded authoring data MUST be represented as commands.

Example command surface:

```rust
enum AuthoringCommand {
    CreateEntity {
        id: EntityId,
        parent: Option<EntityId>,
    },
    AddComponent {
        entity: EntityId,
        component_type: ComponentTypeId,
        value: Value,
    },
    SetProperty {
        target: PropertyPath,
        value: Value,
    },
    CreateGraphNode {
        graph: GraphId,
        node: Node,
    },
    ConnectGraphPorts {
        graph: GraphId,
        edge: Edge,
    },
    AddLayoutConstraint {
        graph: GraphId,
        constraint: LayoutConstraint,
    },
}
```

This enum is illustrative. Implementations MAY split commands by domain or use
versioned command envelopes.

Each applied command MUST produce a result equivalent to:

```rust
struct CommandResult {
    changes: Vec<Change>,
    diagnostics: Vec<Diagnostic>,
    inverse: Option<AuthoringCommand>,
}
```

Commands MUST:

- Be deterministic for the same input state.
- Validate references and schemas.
- Return structured diagnostics instead of relying only on log text.
- Describe resulting changes in semantic terms.
- Support undo when the operation is committed to an interactive editing
  history.

Commands MUST NOT:

- Depend on GUI state.
- Silently discard invalid fields.
- Store runtime ECS entity IDs in project data.
- Require MCP-specific types.

## 12. Transactions

Multiple commands MUST be applicable as one transaction.

Required lifecycle:

```text
begin
  -> apply commands
  -> validate
  -> preview diff
  -> commit or rollback
```

A transaction:

- MUST be isolated from persisted project data until commit.
- MUST roll back completely if commit is rejected.
- MUST return all known diagnostics, not only the first error, when practical.
- MUST create one undo history entry when committed.
- MUST expose a semantic diff before commit.
- SHOULD support optional labels such as `Create enemy chase behavior`.

Validation errors MUST block commit unless the relevant command explicitly
supports an allowed incomplete state. Warnings MAY allow commit.

CLI, MCP, and editor clients MUST NOT implement their own transaction
semantics. They MUST call the shared Authoring Transaction API. The
transaction lifecycle above is the only supported path for mutations.

The persistent undo storage strategy for committed transactions is deferred to
Open Decision #4. Phase 2 uses session-level in-memory undo and redo as defined
by ADR 0005; persistent history MUST NOT be exposed until a future ADR resolves
the storage strategy.

## 13. Queries

Queries are read-only and MUST NOT mutate authoring data.

Required query capabilities:

```text
project.describe
schema.list_component_types
schema.describe_component_type
schema.list_graph_node_types
schema.describe_graph_node_type

scene.list
scene.inspect
scene.validate

entity.inspect
entity.find

graph.inspect
graph.find_nodes
graph.validate
graph.suggest_connections

asset.search
asset.inspect

diagnostics.list
```

Query results SHOULD support filtering and pagination before project size makes
them necessary. Large unbounded responses SHOULD be avoided.

## 14. Diagnostics

Diagnostics MUST be structured and actionable.

```rust
struct Diagnostic {
    severity: Severity,
    code: String,
    message: String,
    target: Option<DiagnosticTarget>,
    related: Vec<DiagnosticTarget>,
    suggestions: Vec<Suggestion>,
}
```

Example:

```json
{
  "severity": "error",
  "code": "graph.type_mismatch",
  "message": "Vector3 output cannot connect to Float input",
  "target": {
    "graph": "enemy_shader",
    "node": "distance",
    "port": "output"
  },
  "related": [
    {
      "graph": "enemy_shader",
      "node": "greater_than",
      "port": "a"
    }
  ],
  "suggestions": [
    {
      "kind": "insert_node",
      "node_type": "math.vector_length"
    }
  ]
}
```

Diagnostic codes MUST be stable enough for tests and tools to depend on them.
Messages MAY improve without being treated as an API break.

Compiler and script diagnostics MAY target a project-relative source file and
an optional one-based line. Editors SHOULD retain these diagnostics in the
Problems surface and navigate to the source without allowing the relative path
to escape its owning project source root.

ADR 0137 defines Editor diagnostic presentation ownership. Problems is the
authoritative detailed repair surface; Hierarchy and Inspector may show concise
context indicators; Scene View directly owns long-form prose only for failures
that prevent the preview itself; Console owns runtime/internal logging. One
semantic issue SHOULD NOT be copied as the same long message across every
surface.

Multiple repair actions MAY be an Editor-only projection over stable
`DiagnosticTarget` identities. Window names, tab routing, display labels, and
callbacks MUST NOT become persisted diagnostic identity merely to support UI
navigation.

## 15. Serialization

The initial canonical authoring format SHOULD be JSON or RON. The chosen format
MUST use a shared parser and serializer rather than ad hoc string editing.

Until an ADR changes this decision, use JSON examples and design APIs so that
the in-memory model is not coupled to JSON.

### 15.1 File Separation

Semantic and presentation data SHOULD use separate files:

```text
assets/behavior/enemy_ai.graph.json
assets/behavior/enemy_ai.graph.view.json
```

### 15.2 Canonical Output

Serialization MUST:

- Use deterministic field and map ordering.
- Use stable formatting.
- Avoid persisting derived runtime-only data.
- Preserve unknown forward-compatible fields only when explicitly supported.
- Include schema versions for versioned documents.
- Write files atomically.

The current-format save pipeline MUST be equivalent to:

```text
parse
  -> require current schema version
  -> validate schema
  -> validate semantics
  -> canonicalize ordering
  -> format
  -> atomic write
```

Direct code-based file edits are supported, but loading and saving through the
engine MUST normalize them through the same validation pipeline. Per ADR 0115,
normal load/save paths do not silently migrate obsolete schema versions.

`AuthoringScene::to_canonical_json` performs semantic validation and
deterministic JSON serialization; persistence is owned by the document or
service that writes the validated canonical result. Historical phase sequencing
must not be used to reintroduce a compatibility migration path.

### 15.3 Editor Working Copy and Explicit Save

ADR 0139 defines one authoritative in-memory working copy per opened/edited
document identity in an Editor project process. Accepted edits are visible to
Inspector, validation, Problems, Scene View, preview, Editor Play snapshotting,
and debug/source navigation immediately. A subsystem MUST NOT reread an older
disk copy while a working copy exists merely because the document is dirty.

Save remains explicit persistence. `Save`, `Ctrl+S`, or `Save All` writes the
current canonical working copy atomically and advances its clean baseline; every
edit is not autosaved to canonical project files. Undo/redo changes the same
consumer-visible working copy. Temporarily invalid working copies are diagnosed
or rejected by strict operations rather than silently replaced with the last
saved valid copy.

ADR 0136 preview asset residency is a separate derived-resource layer. Mutable
authoring working copies are not mesh/texture/model caches, and resident preview
resources are not authoring source of truth.

Process-local working-copy revisions, dirty flags, and recovery metadata are
application state and are not new persisted authoring IDs.

## 16. CLI, MCP, and Conversational Agent Adapters

CLI and MCP are adapters over the command and query API. They MUST NOT implement
their own authoring rules.

For a project open in the Editor, MCP is the standard structured AI authoring
interface. CLI remains the headless scripting and automation adapter. ADR 0121
defines the MCP lifecycle: the active project-scoped Editor owns the initial
write-capable loopback endpoint, while `engine-mcp` remains a transport-agnostic
tool-handler crate. Endpoint discovery and credentials are ephemeral
application state and MUST NOT enter canonical project files.

Each MCP mutation call is one authoring transaction. Tools SHOULD accept bulk
command sequences when an edit must commit atomically; the initial contract does
not keep transactions open across multiple MCP calls. Preview/apply flows MUST
carry document revision or generation identity so stale applies are rejected.

ADR 0132 makes the authoring-owned capability registry the canonical discovery
contract for semantic authoring operations. A capability is declared once at the
shared service, command, query, or schema boundary with a stable capability ID,
machine-readable input/output contract, required permission, and transaction or
stale-revision requirements when applicable. Rust visibility, reflection, or an
Editor widget does not by itself make an operation externally callable.

MCP MUST provide a registry-driven generic structured surface for capability
discovery and common inspect, validate, preview, and apply operations. CLI MUST
provide an equivalent generic path where headless use is meaningful. A new
component or schema already expressible through an existing generic authoring
command therefore becomes available to structured AI clients through shared
schema discovery without a component-specific MCP handler. A new authoring
domain still requires one explicit shared semantic service and capability
registration before adapters may expose it.

Specialized tools such as domain compilation, layout, import, build, runtime
control, or frame capture MAY remain explicit ergonomic extensions. They MUST
reuse authoritative shared services and MUST NOT become a second authoring
implementation. Runtime interaction remains under ADR 0035, while agent, code,
shell, network, and provider lifecycle remain under ADR 0131.

ADR 0131 adds AI Studio above MCP as a project-scoped conversational frontend.
ADR 0152 permits several non-terminal `AgentRun` values to coordinate writes only through explicit non-overlapping Agent Host work claims while preserving one authoritative project writer host. Claims are run-control metadata rather than canonical project data, MUST NOT bypass authoring revision/generation checks, and MUST NOT lock out human editing. Conflicting scope expansion MUST wait, re-plan, or be explicitly reconciled; managed source writes MUST acquire path ownership before modifying the isolated workspace and managed source apply MUST retain its common-baseline stale-file guard in addition to claim ownership. Native AgentRuntime live MCP mutations MUST acquire conservative canonical-authoring ownership before dispatch, while read-only MCP calls remain claim-free; governed external/generated asset acquisition MUST acquire hierarchical destination ownership before provider execution and project import, with the synthetic `project_assets` claim root covering imports into the assets root. The authoritative authoring and asset-management services still perform the final revision/generation/path and import checks. Claim acquisition/release, conflicts, waits, cross-run dependencies, and reconciliation MUST remain auditable per run, and every run retains independent proposal, permission, validation, and completion evidence.
AI Studio MUST be a client of a GUI-free agent host rather than a second
authoring implementation. A session owns conversation history and a versioned
structured proposal. Starting a run snapshots the exact proposal version; the
resulting run has a resumable state and structured event timeline. Continuing
the conversation MAY revise the live proposal without mutating an existing run
snapshot.

AI Studio presentation is not required to remain inside one Editor egui window.
A detached native OS window/viewport uses the same Agent Host and authoritative
Editor writer. A future same-machine process or loopback local-web frontend may
use a versioned local host protocol without duplicating provider orchestration,
project mutation, or ADR 0135 inference-resource arbitration. ADR 0133
separately governs remote-device/private-network reachability, remote
authentication, reconnect/idempotency, and companion UX.

External coding-agent runtimes and native model backends are separate
abstractions. An external `AgentRuntime` MAY own its own model/tool loop and
provider-managed authentication, while a native runtime uses a `ModelBackend`
for inference and owns the GameEngine tool loop itself. Provider credentials,
subscription tokens, local model handles, and Editor MCP credentials MUST NOT be
serialized into session records or canonical project data. External runtimes
receive only the ephemeral MCP connection needed for the active run.

Per ADR 0166, ACP is the provider-neutral common runtime contract for external
coding agents beneath the Agent Host. ACP-capable agents MUST be registered by
descriptor identity rather than added as provider variants to central Agent Host
architecture. ACP session updates, permission requests, and stop reasons MUST be
normalized before entering the host timeline. ACP permission requests remain
subject to the existing GameEngine permission broker, and an ACP stop reason
MUST NOT satisfy Agent Host completion gates. Read-oriented ACP sessions receive
only the Editor read-only MCP credential; write-capable ACP sessions receive only
the AgentRun-bound credential from ADR 0165 and MUST carry the matching run
identity on mutating MCP requests. The unrestricted Editor read-write credential
MUST NOT be exposed to an ACP runtime. ACP SDK types and artifact versions are
adapter implementation details and MUST NOT become Agent Host or persisted
session contracts.

Per ADR 0144, hosted and enterprise `ModelBackend` adapters remain inference-only
inside the native AgentRuntime. Hosted processing requires `NetworkAccess`,
exposes a sanitized remote-processing posture without credentials or secret paths,
and stores GameEngine-owned API credentials only in an OS-protected machine-local
secret store. Provider failures, rate limits, safety refusals, authentication
failures, transport interruption, context rejection, and unsupported capabilities
remain explicit backend evidence and cannot satisfy Agent Host completion gates.
The same backend seam is used for read-only questions and ADR 0141 write-capable
native runs; project mutation continues through the existing governed services.

The ADR 0035 AI Agent Bridge remains the input, frame-observation, and visual
interaction path. It MUST NOT be treated as a substitute for semantic authoring
parity when a capability changes persisted project data.

ADR 0151 adds a GUI-free MCP host without adding a second authoring model. A
write-capable headless host MUST acquire the same per-location OS-backed project
writer authority used by the Editor before it loads authoritative saved project
state. Editor and headless writer ownership are mutually exclusive. A read-only
headless host MAY coexist with an Editor only as an explicitly reported saved-file
snapshot; it cannot observe or claim parity with the Editor's dirty in-memory
working copy. Both modes expose the canonical `engine-mcp` inventory and route
semantic operations directly to shared authoring services rather than through CLI
argv/stdout or host-specific mutation logic.

Example CLI:

```text
engine entity set player gameplay.health.current 80
engine scene validate main
engine graph connect enemy_ai detect_player.success chase_player.start
engine graph auto-layout enemy_ai --scope changed
```

Example MCP tools:

```text
project.describe
scene.inspect
scene.apply_commands
scene.validate
scene.preview_diff

graph.describe_schema
graph.inspect
graph.apply_commands
graph.validate
graph.auto_layout

behavior_tree.schemas
behavior_tree.validate
behavior_tree.compile
behavior_tree.layout
behavior_tree.nodes
behavior_tree.edges
behavior_tree.apply

asset.search
asset.inspect
```

MCP tools SHOULD expose meaningful bulk operations. A tool design that requires
hundreds of single-property calls for a normal edit SHOULD be redesigned.

MCP resources MAY expose read-only data such as:

```text
engine://schemas/components
engine://schemas/graph-nodes
project://scenes/main
project://assets/materials/player
project://diagnostics
```

## 17. Permissions and Safety

Authoring clients SHOULD support permission levels equivalent to:

```text
ReadOnly
PreviewOnly
ProjectDataWrite
AssetWrite
CodeWrite
ExecuteCommands
```

The following operations SHOULD require explicit approval or elevated
permission:

- File deletion
- Large or project-wide changes
- Rust source changes
- External command execution
- Asset overwrite
- Schema migration

Permission checks belong at a shared boundary, not only in the MCP adapter.

ADR 0131 adds an application-level permission broker for operations outside the
structured authoring transaction boundary. At minimum the broker MUST be able to
represent network access, external asset acquisition, runtime launch, runtime
input/control, frame capture, raw workspace filesystem access, arbitrary command
execution, and managed code-workspace apply. Approvals MUST distinguish
`Allow once`, `Allow for this run`, `Allow for this project`, and `Deny`.
Persistent project decisions are policy, not credentials.

Managed services are the default. Semantic project data goes through MCP/shared
authoring services; code goes through an isolated code workspace and reviewed
apply; assets go through acquisition/import services; validation and Play use
engine-managed paths. Raw filesystem and arbitrary command execution are escape
hatches and MUST NOT silently become the normal provider path.

GameEngine application permissions are not an operating-system sandbox. An
external process started under the user's OS identity may technically possess
more authority than the application broker models. UI and audit output MUST NOT
describe such a process as sandboxed unless an independent OS sandbox is really
in use.

Asset loading and writing paths MUST be resolved below their configured root.
Absolute paths, parent traversal, and resolved symlink escapes MUST be rejected
unless a separately approved operation explicitly grants broader filesystem
access.

## 18. Multi-Agent, Session, and Git Collaboration

The project is expected to be edited by multiple humans and AI agents over
time. The implementation MUST favor reviewable and mergeable changes.

At most one write-capable AI run owns the initial Editor project writer slot at
a time, while read-only sessions and normal human editing MAY continue. Human
edits are not blocked by an AI run; stale authoring applies and stale managed
code applies MUST fail and require re-read/reconciliation rather than forcing an
older snapshot over newer work.

AI sessions are private application data by default and MUST survive Editor
restart. The storage key MUST distinguish both stable project identity and
canonical project location. A user MAY explicitly publish portable history to
`.gameengine/ai/sessions/<session-id>/`. Project-shared history MAY contain
conversation, proposal versions, run summaries, and sanitized audit events, but
MUST NOT contain credentials, ephemeral MCP endpoints, process IDs, absolute
machine paths, full code workspaces, build outputs, or caches.

Agent code work MUST use a session/run-scoped working copy or equivalent
isolated source state. Checkpoints and diffs are agent lifecycle data, not Git
commits. Applying code back to the project MUST be an explicit reviewed host
operation with path confinement and stale-source checks. Git remains the
human/team collaboration boundary and MUST NOT be silently rewritten as an
internal agent checkpoint mechanism.

Contributors MUST:

- Keep serialized IDs stable.
- Avoid unrelated formatting churn.
- Avoid changing semantic and view files when only one is necessary.
- Add migrations for persisted schema changes.
- Add tests for command semantics and diagnostic codes.
- Update this specification or an ADR for cross-cutting decisions.

Contributors SHOULD:

- Make small commits organized by responsibility.
- Prefer deterministic generated output.
- Treat the specification as the shared source of truth rather than relying on
  chat history.

## 19. First Graph Domain

Phase 4 introduces the first concrete domain built on the common graph
foundation from Phase 3. Its purpose is to prove that the common foundation
is sufficient for a real domain without requiring domain-specific changes to
the shared layer.

### 19.1 Domain Selection

The first domain MUST be selected and its design recorded in an ADR before
Phase 4 begins. The ADR MUST document why the chosen domain was selected and
which requirements it exercises.

**Behavior Tree is the recommended choice** because:

- Its connection model is strictly hierarchical, which is simpler to validate
  than a general directed graph.
- Its compilation target is a well-understood tree structure, making it a
  good first test of `GraphDomain::compile`.
- It exercises typed ports, structural validation, child ordering, and a
  domain-specific layout policy without the full complexity of Shader Graph
  type inference or Animation Graph blending.

Other candidates, such as Shader Graph, Animation Graph, or Blueprint-like
Visual Scripting, are equally valid. The selection must be made explicitly.

### 19.2 First Domain Requirements

Whichever domain is chosen, its Phase 4 implementation MUST:

- Define node schemas with typed input and output ports (§8.2).
- Define connection rules: which port types may connect to which.
- Implement domain-specific validation producing structured diagnostics
  with stable codes.
- Implement a deterministic compiled or interpreted runtime representation.
- Define a default layout policy appropriate to the domain's visual
  structure.
- Be buildable entirely through authoring commands and the shared graph
  layer without modifying shared code in `authoring::graph`.

### 19.3 Recommended: Behavior Tree

If Behavior Tree is selected, the first implementation SHOULD support:

- Root node, Sequence node, Selector node.
- Condition node, Action node, Decorator node.
- Explicit child ordering.
- Validation: exactly one semantic root.
- Validation: disconnected or unreachable nodes.
- Validation: invalid child counts.
- A deterministic runtime tree representation.
- Top-down auto-layout.

The first version does not need arbitrary scripting inside nodes.

## 20. Implementation Phases

The phase descriptions below preserve implementation history and sequencing.
For completed phases, later Accepted ADRs and the current normative sections of
this specification override historical migration, compatibility, crate-location,
and temporary-ownership statements in these phase notes.

Implementation SHOULD proceed in this order. Phase numbers are sequential
except for Phase 2.5, which is a deliberate runtime validation slice
inserted before the Graph phases.

### Phase 0: Architecture Guardrails

- Establish `authoring` and `graph` ownership boundaries.
- Keep runtime ECS commands separate from authoring commands.
- Add ADR location and test conventions.

Completion criteria:

- New authoring code does not depend on GUI or MCP types.
- Runtime `ecs` does not depend on authoring types.

### Phase 1: Stable Authoring Core

- Stable IDs
- Generic `Value`
- Authoring entity model
- Component schemas
- Deterministic serialization
- Structured diagnostics

Completion criteria:

- An authoring entity can be loaded, validated, modified, saved, and loaded
  again without semantic changes.

### Phase 2: Commands and Transactions

- Authoring commands
- Change records
- Transactions
- Preview diff
- Undo and redo

Design decisions that MUST be resolved during Phase 2:

1. Load-time identifier validation. `EntityId`, `AssetId`, and other typed
   identifiers currently use transparent `Serde` deserialization, which does
   not call `from_stable_id` and therefore does not validate the prefix or
   ULID suffix. Phase 2 MUST decide whether validation runs at load time, at
   command execution time, or through the validation pipeline.

2. Malformed tagged reference behavior. A `Value::EntityRef` or
   `Value::AssetRef` with an invalid stable ID currently causes a hard
   deserialization error. Phase 2 MUST decide whether malformed references
   produce a structured diagnostic that allows the remainder of the file to
   continue loading, or remain a hard deserialization failure.

Completion criteria:

- A multi-command entity edit can be previewed, committed, undone, and redone.
- A failed transaction leaves persisted data unchanged.

### Phase 2.5: Minimal Playable Runtime Slice

Phase 2.5 is a deliberate vertical cut between the authoring tooling phases
and the Graph and Behavior Tree phases. Its purpose is to confirm that the
authoring data model produces a working runtime before the system grows more
complex. Without this phase, the authoring pipeline could be fully specified
but never validated against an actual running game loop.

Inserting this phase before Graph systems also provides a concrete runtime
target that future Behavior Tree and Shader Graph outputs can connect to.

Scope:

- Minimal authoring-to-runtime conversion: load a validated scene from
  `AuthoringEntity` data, spawn runtime ECS entities with matching component
  values, and maintain an explicit `AuthoringEntity` to `Entity` and `AssetId`
  to `RuntimeAssetId` mapping for the duration of the play session.
- Runtime components used: `Transform`, `GlobalTransform`, `Camera3D`, `Mesh`,
  `Material`. These already exist in `crates/engine`.
- `InputState` resource for per-frame keyboard state.
- `PlayerMarker` component to identify the player entity. The Phase 2.5
  authoring representation is an `engine.player_marker` component whose value
  is an empty object. Other values are invalid.
- WASD movement system: reads `InputState` and updates the player entity's
  `Transform` each frame.
- Fixed camera or simple follow camera aimed at the player entity.
- A runnable sample `examples/minimal_playable.rs` that demonstrates an
  authoring scene (player and static objects) converted to a runtime world,
  rendered on screen, and moved with keyboard input.

Not in scope:

- Graph systems: Shader Graph, Behavior Tree, Animation Graph.
- Editor GUI, MCP adapter, CLI adapter.
- Full build pipeline: asset baking, format optimization, WASM export.
- Physics, animation, prefabs, scene editor.
- Persistent undo/redo (deferred per ADR 0005).

Completion criteria:

- At least one authoring entity with a `Transform` and `Mesh` component is
  converted to a runtime ECS entity and rendered on screen.
- WASD keyboard input moves the player entity's transform.
- `authoring` crate does not import runtime ECS or renderer types.
- `ecs` crate does not import `authoring` types.
- The conversion code initially lived in `engine` as the minimal build
  integration prototype. Current runtime-domain ownership and final composition
  follow ADR 0113 rather than this historical temporary-placement note.
- `engine` uses `engine-renderer` for GPU and surface initialization without
  re-implementing it (ADR 0003).
- Authoring validation and conversion planning complete before runtime world
  mutation. Invalid authoring data MUST NOT leave partially spawned entities
  or partially added runtime assets.
- The example compiles and runs with no Graph or Behavior Tree crates present.

Note: The authoring-to-runtime conversion here is a historical minimal
prototype. ADR 0113 now defines runtime-domain and composition ownership;
the packaged artifact format and build optimization policy remain open.

#### Built-in Transform Authoring Contract

The built-in `engine.transform` component is governed by ADR 0059. Its
translation fields are `x`, `y`, and `z`; its XYZ Euler rotation fields are
`rotation_x_degrees`, `rotation_y_degrees`, and `rotation_z_degrees`; and its
scale fields are `scale_x`, `scale_y`, and `scale_z`. Missing rotation fields
MUST default to zero degrees and missing scale fields MUST default to one so
that schema-v1 component values remain valid. The runtime bridge MUST convert
the authored Euler degrees to the runtime quaternion representation without
changing the serialized scene document.

### Phase 3: Common Graph Foundation

The common graph foundation is domain-neutral. It MUST be usable by
Behavior Tree, Shader Graph, Animation Graph, and future graph domains
without modification to the shared layer.

Implementation sequencing begins with Phase 3-A in `authoring::graph`.
Phase 3-A implements the semantic graph document, schema model, structural
validation, graph commands, diffs, single-document transactions, and
deterministic semantic serialization. `GraphView`, auto-layout, concrete graph
domains, and multi-document transactions are deferred to later Phase 3 work.
The Phase 3 placement, document boundary, serialization, and schema
compatibility decisions are recorded in ADRs 0006 through 0009.

Scope:

- Domain-neutral `Graph`, `Node`, `Edge`, `PortRef`, `Group`, and
  `Annotations` models (§9).
- `GraphView` and `NodeLayout` presentation models stored separately from
  semantic data (§9.1).
- Typed port definitions in `NodeSchema`: port value types, input/output
  direction, and multiplicity (arity) rules (§8.2).
- `GraphDomain` trait interface: node schema registry, connection rules,
  domain validation, compiled representation interface, and layout policy
  (§9.2). The compiled graph representation contract is resolved by ADR 0013.
- Graph authoring commands: `CreateNode`, `DeleteNode`, `ConnectPorts`,
  `DisconnectEdge`, `SetNodeProperty`, `AddLayoutConstraint`, and
  equivalents.
- Graph `Change` records and integration with the existing `Transaction`
  lifecycle from Phase 2.
- Structural validation independent of any domain: referenced nodes and
  ports must exist, edge endpoints must be valid, no dangling references.
- Structured graph diagnostics with stable codes (§14).
- Deterministic serialization: separate semantic and view files (§15.1).

Completion criteria:

- A graph can be created, edited with commands, validated structurally, and
  serialized without entering any node coordinates.
- Deleting the view file does not invalidate the semantic graph data.
- The `GraphDomain` trait can be implemented by a minimal test stub with
  three or four node types without modifying any shared code in `authoring::graph`.
- Graph commands participate in `Transaction` with commit, rollback, and
  preview diff.
- Structural validation produces stable diagnostic codes that tests depend on.

#### Phase 3-A Completion Note

Phase 3-A is complete as the semantic graph foundation in `authoring::graph`.
The completed scope includes:

- Semantic `Graph`, `Node`, `Edge`, `PortRef`, `Group`, and `Annotations`
  storage.
- `NodeSchema`, `PortSchema`, `PortDirection`, `PortArity`, stable
  `NodeTypeId`, `PortId`, and `PortValueTypeId`.
- Structural validation for referenced node existence, referenced port
  existence, endpoint direction, duplicate edges, port arity, group member
  existence, and graph/schema key-to-embedded-ID consistency.
- Deterministic semantic graph JSON serialization without any graph view data.
- Graph commands, graph change records, preview diffs, rollback, commit, and
  revision-based single-document conflict detection.
- Private transaction snapshots for `Graph`. `Graph` does not expose public
  `Clone` because its in-memory document instance identity is not persisted
  semantic data.

Phase 3-A intentionally does not implement:

- `GraphView` mutation or graph view serialization.
- Auto-layout or layout mutation.
- Behavior Tree, Shader Graph, Animation Graph, Blueprint-like visual
  scripting, or any other concrete graph domain.
- Runtime graph execution, Runtime ECS integration, renderer integration, or
  bridge changes.
- Port value type compatibility diagnostics. The foundation stores and exposes
  `PortValueTypeId` only; compatibility remains owned by concrete graph
  domains as described in ADR 0009.
- Multi-document transactions.

Deferred follow-up items that are not Phase 3-A blockers:

- Include `GraphId` in `GraphTransactionError::Conflict` for clearer logs and
  diagnostics.
- Clarify `preview_diff()` documentation for transactions that already contain
  blocking diagnostics.
- Decide the inverse command and undo strategy for `GraphChange::NodeDeleted`
  before adding graph session-level undo.

#### Phase 3-B Start Conditions

Phase 3-B may build on the completed semantic graph foundation, but it must
preserve the ADR 0006 through ADR 0009 boundaries:

- Do not embed `GraphView` into `Graph`.
- Do not require pixel coordinates for semantic graph commands.
- Do not introduce runtime graph execution or renderer/ECS coupling.
- Do not move port value type compatibility into the foundation.
- Do not add a concrete Behavior Tree, Shader Graph, Animation Graph, or
  visual scripting domain as part of the shared foundation.
- Keep graph transactions single-document unless a new ADR explicitly accepts
  multi-document transaction behavior.

#### Phase 3-B Completion Note

Phase 3-B is complete as the GraphView presentation foundation in
`authoring::graph_view`.

The completed scope includes:

- `GraphView` as an optional presentation document.
- `GraphView` references the semantic `Graph` by `GraphId`.
- `Graph` does not reference `GraphView`.
- Presentation storage for node layout, group layout, viewport pan and zoom,
  selected nodes, selected edges, selected groups, layout policy identifier,
  and presentation annotations.
- `GraphView::validate(&Graph)` validates presentation references against the
  semantic graph without mutating the graph.
- `GraphTransaction` and `GraphViewTransaction` remain separate
  single-document transaction boundaries.
- Private transaction snapshots for `GraphView`. `GraphView` does not expose
  public `Clone` because its in-memory document instance identity is not
  persisted presentation data.
- Deterministic graph view serialization.
- Presentation annotation validation rejects non-finite floating-point values.

Phase 3-B intentionally does not implement:

- Auto-layout execution.
- Runtime graph execution.
- Runtime ECS, renderer, or bridge integration.
- Behavior Tree, Shader Graph, Animation Graph, Blueprint-like visual
  scripting, or any other concrete graph domain.
- Port selection.
- Port value type compatibility diagnostics.
- Atomic multi-document transactions across `Graph` and `GraphView`.
- Repository or resolver based multi-document validation.
- Editor, CLI, or MCP adapters.

Deferred follow-up items that are not Phase 3-B blockers:

- Auto-layout execution.
- Repository or resolver based graph view validation.
- Multi-document transactions.
- Port selection, if a later editor workflow requires it.
- Editor, CLI, or MCP adapters.
- Common helper for validated dotted identifiers.
- Include `GraphId` in graph and graph view conflict errors for clearer logs
  and diagnostics.
- Clarify `preview_diff()` documentation for transactions that already contain
  blocking diagnostics.

#### Phase 3-C Completion Note

Phase 3-C is complete as the domain validation stub in
`authoring::graph_domain`.

The completed scope includes:

- `GraphDomain` trait.
- `validate_graph_with_domain` helper.
- `TestGraphDomain` fixture.
- Deterministic fixed schema `PortId`s for `TestGraphDomain`.
- Separate validation layers for foundation structural validation and domain
  validation.
- `validate_graph_with_domain` runs foundation validation first.
- Domain validation is skipped when foundation validation produces blocking
  structural diagnostics.
- Port value type compatibility diagnostics are emitted only by domain
  validation.
- The foundation still stores and exposes `PortValueTypeId`, but does not
  decide compatibility.
- `GraphTransaction` remains domain-agnostic.
- `GraphTransaction::commit` does not run domain validation.
- `GraphView` is not required for domain validation.
- The legacy `GraphDomainValidator` hook was removed in favor of
  `authoring::graph_domain::GraphDomain`.

Implemented `TestGraphDomain` diagnostics:

- `test_domain.unsupported_graph_kind`
- `test_domain.port_type_mismatch`
- `test_domain.missing_root`
- `test_domain.multiple_roots`
- `test_domain.cycle_not_allowed`
- `test_domain.missing_required_property`

Phase 3-C intentionally does not implement:

- Behavior Tree production domain.
- Shader Graph production domain.
- Animation Graph production domain.
- Blueprint-like visual scripting production domain.
- Runtime graph execution.
- Compiled graph artifact.
- Runtime ECS, renderer, or bridge integration.
- Editor UI.
- CLI or MCP adapter.
- `Graph` / `GraphView` repository.
- Multi-document transactions.
- Auto-layout execution.

Deferred follow-up items that are not Phase 3-C blockers:

- Production graph domains.
- Repository or resolver validation.
- Graph compilation or interpretation.
- Editor, CLI, or MCP adapters.
- Common helper for validated dotted identifiers.
- Stronger stable diagnostic code coverage.
- Include `GraphId` in graph and graph view conflict errors for clearer logs
  and diagnostics.
- Clarify `preview_diff()` documentation for transactions that already contain
  blocking diagnostics.

### Phase 4: First Graph Domain

Phase 4 implements the first concrete graph domain on the common foundation
from Phase 3. The domain MUST be selected in an ADR before this phase
begins. See §19 for selection guidance; Behavior Tree is the recommended
choice.

Scope:

- Node schemas with typed ports for the chosen domain.
- Connection rules: which port value types may connect to which.
- Domain-specific validation producing structured diagnostics with stable
  codes.
- A deterministic compiled or interpreted runtime representation for the
  domain.
- A default layout policy appropriate to the domain's visual structure.

Completion criteria:

- A non-trivial graph in the chosen domain can be built entirely through
  authoring commands without providing pixel coordinates.
- Domain-specific validation produces structured diagnostics with stable
  codes that tests depend on.
- The domain compiles to a deterministic runtime representation.
- The domain's auto-layout policy produces a readable result.
- No changes to shared code in `authoring::graph` were required to implement
  the domain.

### Phase 5: CLI Adapter

- Query commands
- Transactional edit commands
- Validation and diff output

Completion criteria:

- The Phase 4 first-domain scenario can be performed from the CLI without
  any CLI-unique editing logic.

#### Phase 5 Completion Note

Phase 5 is complete as the file-based Behavior Tree CLI adapter in
`crates/cli`.

The completed scope includes:

- `behavior-tree schemas` for Behavior Tree schema discovery.
- `behavior-tree example` for the reference chase-or-patrol graph scenario.
- `behavior-tree validate <graph.json>` for structural and domain diagnostics.
- `behavior-tree compile <graph.json>` for deterministic compiled output.
- `behavior-tree layout <graph.json>` for deterministic top-down `GraphView`
  output.
- `behavior-tree nodes <graph.json>` for stable node ID, type, name, and
  property queries.
- `behavior-tree edges <graph.json>` for stable edge and `PortRef` endpoint
  queries.
- `behavior-tree preview <graph.json> <commands.json>` for applying a JSON
  array of `GraphCommand` values to a private working copy, returning semantic
  diff and diagnostics without writing the graph file.
- `behavior-tree apply <graph.json> <commands.json>` for applying the same
  shared authoring command path and replacing the graph file only after
  structural and domain validation have no blocking diagnostics.
- `behavior-tree commit <graph.json> <commands.json>` for applying the same
  shared authoring command path and replacing the graph file only after
  structural and domain validation have no blocking diagnostics.

The CLI owns argument parsing, file selection, and JSON output formatting.
Graph command application, structural transaction handling, domain validation,
diff generation, and target-file replacement are delegated to shared
`engine-authoring` APIs so MCP and editor adapters can share the same behavior.

Phase 5 intentionally does not implement:

- MCP tools or transport lifecycle.
- Visual graph editing.
- Persistent undo storage.
- Multi-document graph and graph-view transactions.
- Writing graph view files from `commit`; layout remains an explicit query.

#### Behavior Tree Runtime Executor Completion Note

The Behavior Tree runtime executor is complete as a minimal runtime vertical
slice in `engine::behavior_tree`. This runtime prerequisite is recorded in ADR
0014 and is not assigned a roadmap phase number after the Phase 6 MCP adapter
and Phase 7 ECS Behavior Tree integration numbering cleanup.

The completed scope includes:

- `BehaviorStatus` with `Success`, `Failure`, and `Running`.
- `BehaviorTreeContext` as the runtime dispatch boundary for action and
  condition behavior identifiers, with a context-owned associated error type.
- `BehaviorTreeExecutor` executing only `CompiledBehaviorTree`, not authoring
  `Graph` documents.
- Runtime `Result` errors for invalid compiled tree shapes, missing behavior
  identifiers, excessive compiled depth, and context dispatch failures.
- Root, Sequence, Selector, Condition, Action, and pass-through Decorator node
  semantics.
- Conditions return `BehaviorStatus`, allowing synchronous `Success` /
  `Failure` and polled `Running` conditions without changing the public trait.
- A compile-to-runtime test proving the flow from Behavior Tree authoring graph
  to deterministic compiled tree to runtime tick.

This runtime slice intentionally does not implement:

- Running child resume policy. Traversal is stateless; if a child
  returns `Running`, the next tick starts from the root again.
- Blackboard storage.
- ECS system integration.
- Async actions.
- Parallel nodes.
- Advanced decorator semantics.
- CLI output changes or new CLI commands.

### Phase 6: MCP Adapter

- Schema discovery
- Queries
- Bulk command application
- Validation, diff, and layout tools

Completion criteria:

- An AI agent can discover available node types and build a non-trivial
  graph in the Phase 4 first domain without knowing Rust source code or
  providing pixel coordinates.

#### Phase 6 Completion Note

Phase 6 is complete as the Behavior Tree MCP tool-handler crate in
`crates/mcp`.

The completed scope includes:

- `engine-mcp` as a thin adapter over `engine-authoring`.
- Tool descriptors for MCP transport registration.
- `behavior_tree.schemas` for Behavior Tree schema discovery.
- `behavior_tree.validate` for structural and domain diagnostics.
- `behavior_tree.compile` for deterministic compiled output.
- `behavior_tree.layout` for deterministic top-down `GraphView` output.
- `behavior_tree.nodes` for node queries.
- `behavior_tree.edges` for edge queries.
- `behavior_tree.apply` for bulk `GraphCommand` transaction application,
  returning semantic diff, diagnostics, and the updated graph on success.

Phase 6 intentionally does not implement:

- MCP server process lifecycle or transport binding.
- File persistence policy for MCP calls.
- Multi-call transaction identity.
- Visual graph editing.
- Runtime ECS execution.

ADR 0015 records the adapter boundary. ADR 0121 resolves the project-scoped MCP
transport/process lifecycle and the cross-call transaction policy; implementing
that lifecycle remains follow-up work beyond the completed Phase 6 handler crate.

### Phase 7: ECS Behavior Tree Integration

- Runtime ECS component that owns a compiled Behavior Tree runner.
- Runtime ECS resource that dispatches stable action and condition behavior
  identifiers for the minimal integration path.
- ECS system that ticks Behavior Tree runners once per update.
- Tests proving a compiled Behavior Tree can be attached to an ECS entity,
  ticked by the ECS schedule, and report status or dispatch errors.

Completion criteria:

- A compiled Behavior Tree can be stored on a runtime ECS entity.
- The ECS schedule can tick the tree through a runtime context resource.
- Gameplay behavior dispatch remains outside authoring, CLI, and MCP code.
- The runtime ECS crate still does not depend on authoring or MCP crates.

#### Phase 7 Completion Note

Phase 7 is complete as the minimal ECS Behavior Tree integration in
`engine::behavior_tree`.

The completed scope includes:

- `BehaviorTreeRunner` as an ECS component over `BehaviorTreeExecutor`.
- `BehaviorTreeBehaviorRegistry` as a minimal runtime dispatch resource.
- `behavior_tree_tick_system` to tick all runners.
- `register_behavior_tree_system` to install the registry and system into an
  `engine_ecs::App`.
- A schedule-level test proving authoring graph compilation, runtime runner
  component storage, registry dispatch, ECS ticking, and status recording.

Phase 7 intentionally does not implement:

- Blackboard storage.
- Async actions.
- Running child resume policy.
- Gameplay-specific behavior implementations.
- Persistence of runtime ECS entities or Behavior Tree runner state.

### Phase 8: Human Visual Editor

- Graph rendering
- Selection and property editing
- Command-backed interactions
- Pinning and incremental layout

Completion criteria:

- Editor interactions produce the same commands and results as CLI and MCP.

Phase 8 is governed by ADR 0016.

The human editor is an adapter over existing authoring contracts. Semantic
interactions MUST emit `GraphCommand` values. Presentation interactions MUST
emit `GraphViewCommand` values. The editor MUST NOT mutate `Graph` or
`GraphView` directly outside those transaction APIs.

Editor operations that affect both documents, such as creating a node at a
pointer position, MUST keep semantic and presentation results separate. Phase 8
does not add atomic multi-document transactions. If the semantic transaction
commits and the presentation transaction fails, the semantic edit remains
committed and the editor reports presentation diagnostics.

Persisted human editor state is limited to `GraphView` presentation data:
selection, node layout, collapsed state, pinned state, group bounds, viewport,
layout policy, and presentation annotations. Hover state, drag previews,
context menu state, unsaved text buffers, and clipboard contents are transient
UI state and MUST NOT be serialized.

Incremental layout in Phase 8 uses the selected domain layout service to
produce candidate positions, then merges those positions into the current
`GraphView`. Pinned node positions are preserved. Collapsed state and
presentation annotations are preserved for existing nodes. New nodes use
candidate positions. Stale presentation entries are dropped before graph view
commit.

#### Phase 8-A: Human Editor GUI Toolkit Prototype

Phase 8-A is governed by ADR 0017.

The initial human editor prototype uses `egui` and `eframe` as the editor
frontend implementation. This is not an authoring model contract. `Graph`,
`GraphView`, `GraphCommand`, `GraphViewCommand`, and `engine-authoring` MUST
NOT depend on `egui`, `eframe`, or any GUI toolkit type.

The Phase 8-A prototype belongs in a future `crates/editor` crate. That crate
owns the `eframe` app entry point, egui UI panels, graph canvas, property
inspector, editor session orchestration, and transient UI state.

The required module boundary is:

- `crates/authoring`: semantic graph documents, graph view documents,
  commands, validation, and domain authoring services. No GUI dependencies.
- `crates/editor/src/session.rs`: `EditorSession`, kept egui-free so session
  behavior can be unit-tested without a GUI runtime.
- `crates/editor/src/ui/`: egui and eframe-specific UI code.
- `crates/editor/src/canvas/`: egui canvas code, hit-test caches, drag
  previews, temporary edge previews, and other transient canvas state.
- `crates/editor/src/adapter.rs`: UI gesture and inspector edit conversion to
  `GraphCommand` or `GraphViewCommand`.

The first graph canvas MUST be a thin editor-owned canvas over egui painting,
hit-testing, and pointer input. Existing egui node graph crates MAY be spiked,
but Phase 8-A MUST NOT make them the owner of graph storage, command
semantics, validation, or layout policy.

An external node graph crate MAY be adopted later only if it does not own the
canonical graph model, does not bypass `GraphCommand` or `GraphViewCommand`,
can use authoring node and edge IDs as displayed identities, and preserves the
`Graph` / `GraphView` boundary from ADR 0016.

Phase 8-A prototype tasks are listed in ADR 0017.

#### Phase 8-A Completion Note

Phase 8-A is complete as an eframe-based Behavior Tree graph editor. The canvas
renders nodes and edges using egui primitives. Note: the implementation used
eframe's built-in winit integration rather than a separate adapter-layer bridge;
the `GraphCanvasAdapter` abstraction was not adopted (deviation from ADR 0017's
prototype sketch, which was exploratory).

#### Phase 8-B Completion Note

Phase 8-B is complete as JSON-snapshot undo/redo (up to 100 steps) managed by
`EditorSession`. Documented in ADR 0018.

#### Phase 8-C Completion Note

Phase 8-C is complete as single-file combined JSON persistence
(`EditorSession::save_to_path` / `load_from_path`). Documented in ADR 0019.
ADR 0022 supersedes ADR 0019 by returning to the separate-file layout (ADR 0008)
as part of Phase 9. Read support for the combined format is retained until
Phase 10 ships.

### Deferred: Additional Graph Domains

The engine roadmap (see `docs/IMPLEMENTATION_PLAN_PHASE9_ONWARDS.md`) owns
phases 9–20 and does not include additional graph domains in that range. Shader
Graph, Animation Graph, and other domain-specific graph systems are planned for
Phase 22 and later.

Completion criteria (when scheduled):

- New domains reuse the common graph foundation without copying its storage,
  transaction, or layout contracts.

## 21. Required Test Strategy

Tests MUST cover behavior, not only type construction.

Required categories:

1. Stable ID and deterministic serialization tests.
2. Schema discovery tests.
3. Command apply and inverse-command tests.
4. Transaction commit and rollback tests.
5. Diagnostic code tests.
6. Graph structural validation tests.
7. Domain validation tests.
8. Layout determinism and pinned-node tests.
9. CLI and MCP adapter equivalence tests when those adapters exist.
10. Authoring-to-runtime build conversion tests.

Golden file tests MAY be used for serialization and diffs, but SHOULD be kept
small and reviewable.

## 22. Definition of Done for New Authoring Features

A new authoring feature is complete only when:

- Its data has a documented owner and stable schema.
- It can be queried without GUI access.
- It can be changed through an authoring command.
- Its invalid states produce structured diagnostics.
- Its changes can participate in transactions and diffs.
- Its serialization is deterministic if persisted.
- It has focused tests.
- Its semantic capability is registered once in the authoring-owned capability
  registry when it is intended for structured external authoring.
- The standard structured AI surface can reach it through registry-driven
  generic exposure or an explicitly declared specialized path.
- CLI, MCP, and editor adapters do not duplicate its business rules.
- This specification or an ADR documents any new cross-cutting contract.

## 23. Open Decisions Requiring ADRs

The following decisions are intentionally not fixed yet and MUST be recorded in
an Architecture Decision Record before broad implementation depends on them:

1. Canonical persisted format: JSON, RON, or another structured format.
2. Reflection implementation: custom derive, existing crate, or hybrid.
   Include the impact on crate-level compile-time dependencies.
3. Generic `Value` numeric precision policy.
   In particular, how schema-less deserialization distinguishes `I64` from
   `U64` for values in the overlapping range.
4. Persistent undo storage strategy and undo history limits. Phase 2 scope
   (commands, transactions, rollback, preview diff) is defined in ADR 0005.
   The persistent undo storage strategy (inverse-command, snapshot, patch, or
   hybrid) and history limits require a separate ADR before persistent undo
   history is implemented.
5. Graph layout library versus custom layout implementation for general graph
   layout beyond the Phase 8 editor merge policy accepted in ADR 0016.
6. Packaged authoring-to-runtime build artifact format and build optimization
   policy not already fixed by ADR 0113's runtime-domain ownership and
   composition boundary.

ADRs SHOULD be stored under:

```text
docs/adr/
```

Resolved authoring, adapter, and runtime decisions:

- ADR 0012 selects Behavior Tree as the first Phase 4 graph domain.
- ADR 0013 assigns compiled graph representation ownership to concrete graph
  domains through a domain-owned associated type contract.
- ADR 0014 defines the minimal Behavior Tree runtime executor over
  `CompiledBehaviorTree`.
- ADR 0015 defines the Phase 6 MCP Behavior Tree tool adapter boundary.
- ADR 0016 defines the Phase 8 human visual editor command and presentation
  boundary.
- ADR 0121 makes MCP the standard structured AI authoring interface for an
  active Editor project, defines the project-scoped Editor-owned loopback
  lifecycle, and uses bulk single-call transactions instead of cross-call
  transaction identity.
- ADR 0132 makes an authoring-owned capability registry the canonical semantic
  discovery contract and requires registry-driven generic MCP/CLI exposure so
  ordinary new authoring features do not need parallel AI-specific handlers.
- ADR 0017 defines the Phase 8-A human editor GUI toolkit and crate boundary.
- ADR 0018 defines the Phase 8-B undo/redo snapshot strategy (JSON snapshots, 100-step limit).
- ADR 0019 records the historical Phase 8-C combined-file persistence format;
  ADR 0022 supersedes it, and ADR 0115 does not retain the obsolete combined
  format as a current compatibility reader.
- ADR 0020 defines the scene document `schema_version` field policy.
- ADR 0021 defines the asset reference model (`asset_ref` only) and the `asset_manifest.json` format.
- ADR 0022 returns the editor to the ADR 0008 separate-file layout (`.graph.json` / `.graph.view.json`).
- ADR 0023 places `ProjectConfig` / `ProjectRoot` in `engine-authoring` (`crates/authoring/src/project.rs`).
- ADR 0024 defines the editor Play process model (in-process, wgpu 22→29 Track W) and its fallback.
- ADR 0025-0027 define inspector staging, virtual input/observation, and the
  component definition registry.
- ADR 0028 defines the advanced authoring roadmap boundaries.
- ADR 0029-0033 define manifest v2 import settings, prefab v1, project
  settings, glTF static mesh import, and Animation Graph schema.
- ADR 0034-0035 define desktop packaging and the AI-agent filesystem IPC
  boundary.
- ADR 0036-0044 define shadow/environment lighting, Rhai scripting, gamepad,
  navigation, post-processing, wasm32 strategy, instancing/LOD, skeletal
  animation, and particles.
- ADR 0045 defines the generic player binary and runnable package layout.
- ADR 0046-0049 define declarative UI, runtime scene management, save data,
  and Script API v2's deferred command boundary.
- ADR 0050-0052 define the native Rust GameModule boundary, project-wide stable
  ECS system identity and ordering, and query-scoped Rust gameplay I/O.
- ADR 0053-0056 define input-to-character motor flow, authorable runtime
  components and materials, and the fixed-step collision/combat contract.
  `engine.damage_receiver` is the stable authoring ID for health, team, and
  invulnerability state. Static compound collision is authored as child
  primitive-collider entities; Editor Ready v1 does not serialize mesh
  colliders. Collision transitions and accepted hit results are engine-owned
  fixed-step streams shared by Editor Play and packaged Player.
- ADR 0057 defines `engine.nav_mesh_surface`, scene-owned bake documents,
  authorable agent status/repath/avoidance, project Rust navigation commands,
  and stable Behavior Tree result registration and inspection.
- ADR 0058 defines OS-writable distribution data, corrupt-slot metadata,
  duplicate-scene suppression, editor prefab instance operations, Play
  pause/step diagnostics, and deterministic package reports/notices. Its
  proving project `examples/busters_lite` no longer exists;
  `examples/coin_collision_loop` is the only in-tree project.
- ADR 0060 defines UI document schema v2 and migration, command-backed visual
  UI authoring, scene reparent commands, editor recovery snapshots,
  recoverable asset trash, and the read-only runtime inspection boundary.
- ADR 0065 layers sealed, parameter-derived typed gameplay APIs over the ABI v3
  transfer format. Raw generic values and access manifests remain host/macro
  implementation details; maintained game systems use typed queries,
  resources, views, input actions, events, and deferred commands.
- ADR 0066 stores project Rust component identity in versioned `.rs.meta.json`
  sidecars and keeps opaque IDs out of ordinary Inspector editing. ADR 0091
  makes the sidecar the only identity source.
- ADR 0067 defines the unified `assets/scripts` user-code layout, category-safe
  creation and movement, and the generated internal `game/` Cargo host.
- ADR 0082 defines `engine.animation_controller` as the single public
  animation authoring component. The build pipeline expands it into separate
  runtime skeleton-pose, clip-playback, and graph-player components. ADR 0091
  deletes the legacy animation authoring components, so the controller is the
  only animation authoring shape.
- ADR 0085 keeps glTF, GLB, and FBX files as atomic registered import sources
  while treating their imported animation clips as independently addressable
  stable sub-assets. New Animation Graph states reference stable
  `MotionSlotId` values, `*.animset.json` assets bind those slots to clips from
  any number of model sources, and `engine.animation_controller` references an
  Animation Graph plus Animation Set instead of one source-local clip table.
  Runtime conversion resolves and retargets every binding independently;
  generated meshes and clips remain derived catalog/cache data unless a future
  explicit extraction workflow creates an author-owned copy.
- ADR 0098 extends each Animation Set binding with a default-empty ordered
  overlay list. The primary clip and overlays form one logical motion before
  graph playback; later layers replace earlier conflicting bone-property or
  morph-name channels. This source composition is distinct from crossfade,
  which transitions between already-composed graph states. VMD model/scene
  routing uses populated binary sections rather than file-name conventions,
  and scene-only camera/light/self-shadow VMDs are never PMX-paired.
- ADR 0099 lets one registered VMD source target multiple PMX model sources.
  Each baked clip ID includes the stable PMX source ID and source clip index;
  picker labels resolve the target's current display name dynamically, so
  renaming a model changes presentation without changing references. ADR 0115
  removes the pre-0099 singular model-source field and hidden old-ID alias;
  the current contract uses only target-specific IDs.
- ADR 0110 adds model-owned `HumanoidProfile` metadata and a distinct
  skeleton-independent `HumanoidMotion` variant while preserving every
  skeleton-bound Native `AnimationClip`. Asset Browser and animation pickers
  group Native, Humanoid, and automatic selection under one logical animation
  and keep both fidelity paths explicitly selectable. Automatic resolution is
  lossless-first: same-skeleton Native, then Native through an explicit
  ADR 0079 Retarget Map, then Humanoid adaptation, otherwise unsupported.
  Humanoid conversion may exclude source-specific channels with a warning but
  must never discard the Native clip or implicitly match extra bones by name.
  Humanoid adaptation bakes to target-specific ordinary `AnimationClip` data
  before runtime playback; it is a biped compatibility layer, not the
  universal retarget canonical space.
- ADR 0083 defines `engine.static_mesh_renderer` and
  `engine.skinned_mesh_renderer` as the public mesh-rendering authoring
  components. Scene conversion expands them into separate runtime mesh,
  material, material-slot, and skinning components. ADR 0091 deletes the
  legacy rendering components, so the unified pair is the only way to draw.
- ADR 0103 adds a selectable Play Mode Scene View that renders the live runtime
  world through an editor-owned explicit camera. It MUST NOT add, remove, or
  modify runtime camera entities, and MUST NOT write the runtime `ViewportSize`.
  Runtime-space selection uses `GlobalTransform` and resolves back to stable
  authoring entity identity. Game View remains the default Play presentation.
- ADR 0104 separates edit-time feedback by cost. Pure scene, schema, reference,
  and component-value checks remain synchronous; filesystem-backed checks run
  in a debounced worker and retain their existing diagnostic identities.
  Imported sources persist an optional modified-time-and-length `source_stamp`
  only as an editor staleness hint; content fingerprints remain authoritative
  for import and packaging. Preview GPU uploads, Inspector-derived catalogs,
  imported bone choices, and manifest hashes are cached by their owning
  revision, and scene dirty state compares authoring revisions rather than
  serializing canonical JSON after every edit.

### Single-version document policy

ADR 0115 establishes the current-format-only baseline. A versioned document
whose `schema_version` differs from the current version is an error, not an
input to migrate:

| Document | Accepted `schema_version` |
| --- | --- |
| `*.ui.json` | 3 |
| `*.material.json` | 3 |
| `asset_manifest.json` | 2 |
| `project.json` | 2 |

Versioned documents MUST state the current schema version required by their
owner. Scene, prefab, project settings, graph, graph view, animation set, and
component-sidecar documents remain on their current owner-defined versions;
this specification MUST NOT be read as permission to accept an older or missing
version.

Authoring components have exactly one shape each. The removed authoring
components `engine.mesh`, `engine.material`, `engine.material_slots`,
`engine.skinned_mesh`, `engine.skeleton`, `engine.animator`, and
`engine.animation_graph_player` are unregistered; a document naming one gets
the ordinary unknown-component treatment. An Animation Graph state binds motion
through `motion_slot` only.

### Editor-ready document extensions

`*.ui.json` schema version 2 historically added image/nine-slice, progress,
stack, grid, overlay, and scroll-view nodes. The current UI document is schema
version 3; per ADR 0115, versions 1 and 2 are obsolete inputs rather than
normal-load migration sources. UI mutations MUST use `UiDocumentCommand` and
whole-document validation before commit.

Scene hierarchy reparenting MUST use `AuthoringCommand::SetEntityParent`.
Adapters MUST NOT assign `AuthoringEntity.parent` directly. Missing parents,
self-parenting, and descendant cycles are blocking diagnostics.

Editor `.autosave` siblings and `.engine/asset_trash` are recovery data, not
authoring assets. They MUST NOT be packaged, registered in the asset manifest,
or treated as the saved document baseline.

## 24. Reference Scenario

This scenario is the minimum end-to-end target for the system.

An AI agent receives the request:

> Create an enemy behavior that chases the player when the player is visible,
> otherwise patrols.

The AI:

1. Queries available Behavior Tree node schemas.
2. Begins a transaction.
3. Creates a root selector, a sequence, a visibility condition, a chase action,
   and a patrol action using stable IDs.
4. Connects ports using node and port IDs.
5. Requests validation.
6. Receives actionable diagnostics if the tree is invalid.
7. Requests a semantic diff.
8. Commits the transaction.
9. Requests auto-layout without providing pixel coordinates.

A human opens the same tree and sees a readable top-down layout. The human
moves and pins one node. A later AI edit adds a nearby node without moving the
pinned node or unnecessarily rearranging the rest of the tree.

The CLI, MCP adapter, and visual editor all operate on the same commands,
schemas, validation, and transaction implementation.
