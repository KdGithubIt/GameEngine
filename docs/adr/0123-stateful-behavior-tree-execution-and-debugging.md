# ADR 0123: Stateful Behavior Tree Execution and Debugging

Status: Accepted
Date: 2026-08-16
Builds on: ADR 0013, ADR 0014, ADR 0057, ADR 0113, ADR 0121

## Context

Behavior Tree authoring, deterministic compilation, runtime dispatch, project
Rust behavior registration, blackboard data, and basic Editor debug views are
already present. The remaining runtime limitation is fundamental for ordinary
game AI: ADR 0014 intentionally made traversal stateless. When an action or
condition returns `Running`, the next tick starts again from the root.

That behavior is sufficient for an initial vertical slice but is awkward for
real enemies. A Sequence containing `MoveToTarget`, `PlayAttack`, and
`WaitCooldown` should resume the running child rather than repeatedly executing
every preceding child. Long-running actions also need well-defined entry,
completion, and cancellation semantics so navigation, animation, and effects can
clean up when a higher-priority branch interrupts them.

The Editor must make this state understandable. A graph that can run over many
frames needs more than a final success/failure label: authors need to see the
active path, the running node, elapsed state, recent transitions, and why an
abort occurred without introducing an Editor-only interpreter.

## Decision

### 1. Compiled trees remain immutable; execution state is per runner

`CompiledBehaviorTree` remains the immutable product of authoring compilation
and may be shared by many entities. Mutable execution state is moved into a
separate `BehaviorTreeInstance` owned per running entity/component.

The instance contains only runtime state:

- current composite child cursors;
- active/running node state;
- decorator timing/counters when required;
- one execution generation;
- bounded recent debug transitions; and
- bookkeeping needed to issue lifecycle callbacks exactly once.

No runtime cursor or ECS entity is serialized into the graph document. Two
enemies using one Behavior Tree asset never share execution memory.

### 2. Prepare a compact runtime plan without changing authoring graph semantics

The recursive compiled DTO remains useful for deterministic authoring output.
At runtime it is prepared once into a compact indexed plan with deterministic
node order and a mapping from runtime node index back to the source `NodeId`.

`BehaviorTreeInstance` stores compact indices for hot-path state. Editor and
diagnostics use the retained `NodeId` mapping. The runtime MUST NOT key hot
state by graph-view coordinates, display names, or behavior strings.

Prepared plans are cached by compiled-asset identity/generation. Replacing or
hot-reloading a compiled tree invalidates instances in a defined way: the first
implementation resets execution cleanly at the root and emits a debug reset
reason instead of attempting an unsafe partial state remap.

### 3. Sequence and Selector become memory composites

The existing `Sequence` and `Selector` semantics are refined for `Running`:

- Sequence resumes its running child. A successful child advances to the next
  child in the same tick until another child runs/fails or the Sequence
  succeeds.
- Selector resumes its running child. A failed child advances to the next child
  in the same tick until another child runs/succeeds or the Selector fails.
- when the composite returns a terminal status, its child cursor resets.

A per-tick node-step budget prevents malformed/stateful combinations from
spinning indefinitely in one frame.

Reactive "restart from child zero every tick" behavior is a distinct semantic
and, if needed, will be represented by explicit ReactiveSequence/
ReactiveSelector node types. It will not remain an undocumented side effect of
stateless execution.

### 4. Long-running leaves get lifecycle hooks without exposing engine internals

`BehaviorTreeContext` remains the gameplay dispatch boundary. It is extended
with defaulted lifecycle hooks so existing simple contexts do not need custom
cleanup merely to compile:

- action enter/start;
- action tick (the existing gameplay decision point);
- action completion notification; and
- action abort/cancel.

Lifecycle calls include the source node identity as well as the stable behavior
ID so two nodes using the same behavior implementation remain distinguishable.
The exact Rust method names are implementation details, but the semantic
contract is normative: enter occurs once, terminal completion occurs once, and
abort occurs once for a running action that is interrupted/reset.

Contexts continue to own gameplay meaning. The Behavior Tree runtime does not
directly depend on navigation, animation, combat, or audio implementations.
Project-Rust adapters translate the lifecycle into the existing deferred
command model rather than receiving mutable ECS access.

### 5. Aborts are explicit and deterministic

An instance may be reset because of at least:

- runner disable/removal;
- entity despawn;
- tree asset generation replacement;
- an explicit gameplay reset command; or
- a future reactive/higher-priority branch interruption.

Reset walks only currently active/running state and dispatches required abort
hooks in deterministic deepest-first order before clearing memory. Dropping an
instance must not leave navigation targets, animation state, or other
context-owned long-running work silently active when the context registered
cleanup behavior.

The first implementation does not invent implicit parallel execution. Parallel
nodes, services, and event-driven observer aborts require explicit node
semantics in a later schema extension.

### 6. Stateful decorators are typed behavior, not hidden timing flags

The current generic Decorator can continue to dispatch a stable decorator
behavior where appropriate, but common engine-level temporal semantics should
be explicit node types when they become authorable. The first stateful set is
planned around:

- Inverter (stateless result transform);
- Wait/Delay;
- Timeout; and
- Cooldown.

Their state belongs to `BehaviorTreeInstance`, never to the compiled asset or
graph-view data. Additional decorators register through the Behavior Tree
domain schema and compiler; the Editor does not hard-code graph mutation rules
for them.

### 7. Runtime observation is read-only and keyed back to authoring identity

The runtime publishes a bounded `BehaviorExecutionSnapshot` for the existing
Behavior Tree state view. It contains, as applicable:

- runner/tree identity and execution generation;
- current overall status;
- active node stack/path using source `NodeId`s;
- currently running leaf;
- per-active-node elapsed time where meaningful;
- last terminal status/reason;
- recent enter/exit/abort transitions; and
- the existing blackboard/error information.

The snapshot is observation data, not persisted authoring data. Editor
selection, graph overlays, MCP inspection, and project diagnostics consume the
same snapshot shape rather than peeking into executor internals independently.

### 8. The Behavior Tree Editor gets a dedicated runtime-debug mode

While Play is running and an entity with `engine.behavior_tree_runner` is
selected, the graph editor can attach to that runner read-only and display:

- the active path with status styling;
- a distinct Running node highlight;
- last Success/Failure transitions;
- elapsed time for stateful nodes;
- abort/reset reasons;
- blackboard values already exposed by the runtime state view; and
- errors directly on the source node where identity is known.

The authoring graph remains editable according to the normal Editor/Play policy;
runtime debug highlighting is transient presentation state and is never written
to the graph view document.

ADR 0138 supersedes only the top-level presentation shape: Behavior Tree runtime
debugging is a domain provider inside the shared Play-mode Graph Debug shell, not
a permanently privileged `Behavior Tree` tab. The `BehaviorExecutionSnapshot`,
source `NodeId` mapping, active-path semantics, blackboard data, and abort/reset
meaning defined here remain Behavior Tree-owned and are not generalized into a
universal graph runtime status type.

A future pause/step/breakpoint debugger may build on the same snapshot and
execution-generation boundary. It is not required for the first stateful
implementation. The first UX must still make "why is this enemy stuck on this
node?" answerable without log spelunking.

### 9. Authoring UX stays schema-driven and parity-safe

New node types appear through the existing Behavior Tree schema catalog and
shared graph commands. The node palette groups/searches nodes by semantic
category (composite, decorator, condition, action) and supplies valid defaults.
Invalid child counts, missing behavior IDs, and invalid decorator parameters
produce inline diagnostics and Problems entries from the shared validator.

Editor, CLI, and MCP all mutate the same graph document through the shared
Behavior Tree authoring service per ADR 0121. No Editor-only node insertion,
property normalization, or validation rule is introduced.

### 10. Runtime cost is bounded and deterministic

The executor allocates instance storage when a tree/runner is initialized or
its plan changes, not on every node visit. Child order stays the compiled order.
Per-tick execution has a configurable hard step budget in addition to the
existing maximum tree depth.

Debug history is bounded and may be disabled or reduced in packaged release
builds without changing Behavior Tree semantics. Disabling history must not
remove the current-state snapshot needed by gameplay queries.

### 11. Implementation proceeds from semantics to UX

Implementation slices are:

1. prepared indexed plan + `BehaviorTreeInstance` state;
2. memory Sequence/Selector semantics and exhaustive runtime tests;
3. lifecycle/abort hooks and project-Rust adapter behavior;
4. explicit stateful decorators and compiler/schema validation;
5. execution snapshot/state-view extension; and
6. Editor live graph overlay and diagnostic UX.

Navigation, animation, or combat behavior implementations are not folded into
the Behavior Tree crate. They remain consumers/context implementations.

## Verification

The accepted implementation must cover at least:

- Sequence resumes a Running child without re-running successful predecessors;
- Selector resumes a Running child without re-running failed predecessors;
- two entities sharing one compiled tree have completely independent cursors;
- enter/completion/abort hooks fire exactly once in all terminal/reset paths;
- a tree generation replacement aborts active work and restarts cleanly;
- the step budget terminates pathological traversal with a structured error;
- source `NodeId` mapping remains correct after runtime plan preparation;
- project-Rust behavior dispatch still respects deferred-command safety;
- Editor, MCP, and CLI see the same validation semantics for new node types;
- runtime snapshots do not mutate the graph document; and
- the selected Play-mode runner's highlighted path matches the executor state.

The Play-mode graph overlay requires Visual Validation when implemented.

## Consequences

Behavior Trees become suitable for multi-frame enemy behavior without forcing
all state into project action implementations. Runtime state is compact and per
entity, while compiled assets remain shareable and deterministic. Cancellation
becomes a first-class correctness contract instead of cleanup by convention.

The executor and context contract become more sophisticated, and debugging data
must be bounded. In exchange, gameplay code gains predictable long-running
semantics and the Editor can explain the current AI decision directly on the
same graph an author edits.

## Alternatives Considered

### Keep restarting from the root and require every action to self-deduplicate

Rejected. It pushes composite execution semantics into every gameplay action,
causes repeated conditions/actions, and makes interruption behavior impossible
to reason about locally.

### Store running state inside `CompiledBehaviorTree`

Rejected. Compiled trees are immutable/shareable artifacts. Mutable state there
would couple every entity that uses the same asset.

### Key runtime state only by behavior ID

Rejected. Behavior IDs identify implementations and may be reused by multiple
nodes. Source `NodeId` plus compact runtime index is the correct instance-level
identity.

### Build a separate Editor interpreter for debugging

Rejected. It would inevitably drift from packaged runtime semantics. The Editor
observes the real executor through a read-only snapshot.

## Compatibility and Migration

Behavior Tree graph identity and existing stable behavior IDs remain unchanged.
If explicit new stateful node types require graph/compiled schema changes, the
current schema version is advanced and in-repository graphs/fixtures are updated
under ADR 0115; compatibility-only parsing of obsolete engine revisions is not
added.

The existing `BehaviorTreeContext` surface should be extended with defaulted
lifecycle hooks where possible. If a signature must change to carry source-node
identity safely, that public API change is made explicitly in the implementation
PR and all current callers are updated together. The `engine` compatibility
facade remains intact under ADR 0113.
