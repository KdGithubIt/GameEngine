# ADR 0014: Behavior Tree Runtime Executor

Status: Accepted
Date: 2026-06-05

## Context

Phase 4 added the production Behavior Tree graph domain and a deterministic
`CompiledBehaviorTree` artifact. Phase 5 exposed that domain through the CLI
for schema discovery, examples, validation, compilation, layout, and command
application.

The next vertical slice needs to prove that a compiled Behavior Tree can be
executed by runtime code. This execution boundary must not move Behavior Tree
runtime semantics into the domain-neutral graph foundation, and it must not
make CLI or authoring code own gameplay action implementations.

## Decision

The runtime vertical slice adds a minimal Behavior Tree runtime executor in
`crates/engine` under `engine::behavior_tree`.

The executor:

- Executes `CompiledBehaviorTree` values produced by `engine-authoring`.
- Does not execute `Graph` authoring documents directly.
- Dispatches action and condition behavior identifiers through a runtime
  `BehaviorTreeContext` trait.
- Exposes `BehaviorStatus` with `Success`, `Failure`, and `Running`.
- Exposes `BehaviorTreeExecutor::new(tree)` and `tick(&mut context)`.
- Returns `BehaviorTreeRuntimeError` instead of panicking for invalid compiled
  tree shapes or unknown behavior dispatch.
- Keeps context-owned dispatch failures in `BehaviorTreeContext::Error` and
  wraps them as runtime dispatch failures so gameplay causes are not collapsed
  into executor structural errors.
- Enforces a runtime depth limit for directly constructed or deserialized
  compiled trees.

Node semantics are:

- Root returns its single child's status.
- Sequence ticks children in order and short-circuits on `Failure` or
  `Running`.
- Selector ticks children in order and short-circuits on `Success` or
  `Running`.
- Condition calls `BehaviorTreeContext::check_condition` and returns the
  status directly. Synchronous conditions return `Success` or `Failure`; polled
  conditions may return `Running`.
- Action calls `BehaviorTreeContext::tick_action` and returns the status
  directly.
- Decorator is pass-through in the initial runtime executor.

The initial runtime traversal is intentionally stateless. If a child returns
`Running`, the next tick starts from the root again. Advanced running-state
resume policy is future work.

## Consequences

- The runtime vertical slice is complete: authoring graphs can compile into a
  deterministic artifact that runtime code can tick.
- The runtime executor depends on `engine-authoring` for the compiled Behavior
  Tree DTO. This is acceptable for the current minimal implementation because
  `engine` already owns authoring-to-runtime bridge code and there is no
  reverse dependency from authoring into engine.
- The domain-neutral graph foundation remains free of Behavior Tree runtime
  logic.
- Gameplay action and condition implementations remain outside authoring and
  CLI code.
- Gameplay dispatch failures preserve their real source error through the
  context associated error type.
- `Running` behavior is simple and deterministic, but does not yet resume from
  the previously running child.

## Alternatives Considered

### New `engine-runtime` crate

Deferred. A separate runtime crate may become useful once multiple runtime
graph artifacts exist, but adding it now would broaden workspace structure for
a single minimal executor.

### Move compiled Behavior Tree DTOs out of authoring

Deferred. A shared artifact crate would avoid runtime depending on authoring,
but it is a larger design change than the runtime executor slice needs. ADR
0013 already assigns compiled representation ownership to concrete domains.

### Execute authoring `Graph` directly

Rejected. Runtime execution must use compiled representation only. Direct
graph execution would couple runtime behavior to editable authoring storage and
weaken the compile boundary.

### Store running child indices in the executor

Deferred. This can improve resume behavior for long-running Sequence and
Selector nodes, but it is not required to prove the initial runtime executor.
The public executor owns the compiled tree and can grow state later without
changing the context dispatch contract.

## Compatibility and Migration

No persisted project data changes. No CLI output changes. Existing Behavior
Tree graph JSON, compiled tree JSON, diagnostics, schema discovery, validation,
compilation, and layout commands remain compatible.

Future work includes blackboard support, ECS integration, async actions,
parallel nodes, advanced decorators, and running-state resume policy.
