# ADR 0114: Heavy Third-Party Dependency Isolation

Status: Accepted
Date: 2026-08-15
Builds on: ADR 0113

## Context

ADR 0113 split the monolithic runtime into domain crates so a domain-only
change no longer has to select unrelated high-level packages in normal PR CI.
That package boundary is necessary but not sufficient when a lightweight
consumer still reaches a domain crate whose unconditional third-party
requirements include a large backend stack.

The first measured example was `engine-import`. Its 103 unit tests execute in a
fraction of a second, while a cold affected build spends minutes compiling
transitive dependencies. Importers only need CPU animation and mesh contracts,
but the dependency graph previously reached both `wgpu` through
`engine-render-runtime` and `rapier3d` through `engine-animation ->
engine-physics`.

The same problem exists outside importing. Scene replay only needs virtual input
contracts, gameplay only needs collision/event state, and headless rendering
only needs a GPU context. None of those consumers should compile operating
system input/audio backends, a concrete physics solver, or a window surface
adapter merely because those implementations share an owning domain.

This is a general architecture issue rather than an importer-specific CI hack.
GPU, physics, windowing, audio, editor GUI, and scripting runtimes must not be
compiled by consumers that only need neutral data contracts from the same
domain.

## Decision

### 1. Heavy backends are explicit feature boundaries at their owning domain

A domain crate that exposes both neutral contracts and a heavyweight backend
must make the backend optional when the neutral contract remains useful on its
own.

The engine-wide boundaries are:

| Owner | Default backend feature | Heavy implementation excluded when disabled |
| --- | --- | --- |
| `engine-render-runtime` | `gpu` | `wgpu` and GPU presentation implementation |
| `engine-animation` | `physics` | physics-backed foot IK integration |
| `engine-physics` | `solver` | `rapier3d` / concrete solver implementation |
| `engine-platform` | `window-input` | `winit` input types/backend |
| `engine-platform` | `audio` | `rodio` native audio backend |
| `engine-platform` | `gamepad` | `gilrs` native gamepad backend |
| `engine-renderer` | `surface` | `winit` window-surface integration |

Default features preserve the existing runtime/editor behavior. Feature-off
modes are explicit contract-only or headless build modes, not alternate gameplay
semantics.

Feature-off implementations that use separate source modules must remain in a
normal compiler/Clippy path where practical so Cargo feature unification cannot
hide a broken contract-only implementation from ordinary CI.

### 2. Lower consumers request only the contracts they actually use

Known lightweight consumers must disable backend defaults deliberately:

- `engine-import` consumes `engine-animation` and `engine-render-runtime`
  without default features. Import parsing therefore does not compile `wgpu` or
  the Rapier-backed physics path.
- `engine-gameplay` consumes `engine-physics` without default features. Combat,
  hitbox, controller state, and collision-event contracts therefore do not
  compile the Rapier solver.
- `engine-scene` consumes `engine-platform` without default features. Replay and
  virtual-input persistence therefore do not compile `winit`, `gilrs`, or
  `rodio`.
- `engine-render-runtime` uses `engine-renderer` without default features in its
  headless GPU tests, so a GPU context does not imply a window surface or
  `winit`.
- `engine-scripting` remains the host-independent SDK/API boundary and must not
  gain `rhai`, `egui`, `winit`, `wgpu`, `rodio`, `gilrs`, or `rapier3d` in its
  normal dependency graph.

When the complete engine is built, Cargo feature unification enables the normal
runtime backend features through the backend owners. Shared concrete data types
therefore continue to use the same crate/module paths and do not create
duplicate ECS-visible or asset-visible types.

### 3. Composition-only backends stay at the composition boundary

The top-level `engine` facade/composition crate and editor host are allowed to
own concrete integrations that genuinely compose multiple domains. Rhai runtime
execution, egui/editor integration, a desktop winit event loop, GPU presentation,
and similar host concerns must not be pushed downward merely for convenience.

`engine-scripting` specifically owns host-independent game SDK contracts, not a
Rhai dependency. `engine-platform` owns native audio/gamepad/input adapters, not
scene or gameplay. `engine-renderer` owns the optional presentation surface,
while its GPU context remains usable headlessly.

A temporary non-default `legacy-direct-platform-backends` feature on `engine`
retains the existing lockfile dependency set while platform ownership is moved
fully into `engine-platform`. Normal runtime/editor builds do not enable that
feature; active native backend ownership remains `engine-platform`.

### 4. Prefer owner-local feature isolation before inventing tiny crates

A new crate is appropriate when a stable independent domain contract warrants
one. A crate must not be introduced solely to move a few types away from one
heavy dependency if an owner-local optional backend cleanly preserves the same
type identity and dependency direction.

Conversely, feature flags must not become a substitute for domain boundaries.
`wgpu`, `rapier3d`, `winit`, `rodio`, `gilrs`, `egui`, `rhai`, and similar
backend libraries belong only in the lowest domain or composition host that
actually implements that backend. Higher or lower neutral domains consume
engine-owned contracts rather than expose third-party handles in neutral data.

### 5. Dependency boundaries are regression-tested

Packages with an intentional lightweight graph carry `cargo tree` regression
tests. These tests fail if the forbidden backend re-enters the normal or
feature-off dependency graph.

Current checks cover:

- import: no `wgpu`, `rapier3d`, or `engine-physics` backend dependency;
- gameplay: no `rapier3d` or `parry3d`;
- platform contract-only mode: no `winit`, `gilrs`, or `rodio`;
- scene: no `winit`, `gilrs`, or `rodio` through platform;
- headless renderer: no `winit`;
- scripting SDK: no Rhai, egui, window, GPU, audio/gamepad, or physics backend.

These are architecture tests, not performance heuristics. A future backend
addition must update the owning boundary and the relevant regression test
rather than silently widen neutral compile graphs.

### 6. Heavy dependency additions require an explicit dependency-cost check

When adding or moving a heavyweight third-party dependency, review:

1. which workspace packages gain it transitively;
2. whether consumers can use the required contract without the backend;
3. whether the dependency should be optional or target-specific;
4. whether default features pull capabilities the engine does not use; and
5. whether an affected-package CI build now compiles an unrelated backend.

A dependency that is convenient but widens unrelated compile graphs must be
moved behind the owning backend boundary before the change is considered
complete.

## Consequences

- Import-only validation can compile without `wgpu` and without the
  Rapier-backed physics domain.
- Gameplay-only validation can compile collision/gameplay contracts without the
  Rapier solver.
- Scene/replay validation can compile without desktop window, gamepad, or audio
  backends.
- Headless low-level rendering can initialize GPU context without `winit`.
- The scripting SDK remains independent of Rhai, egui, platform, GPU, audio,
  gamepad, and physics backends.
- Full runtime and editor builds keep the existing behavior because backend
  features remain enabled by default at their owners.
- CI cache quality still matters, but dependency removal reduces work even on a
  cold runner and does not depend on cache hits.
- Contract-only configurations become useful for headless tooling, asset
  processing, future server/build workflows, and narrow domain validation.
- The rule applies uniformly to GameEngine domains; MMD and other source formats
  do not receive special dependency architecture.
