# ADR 0050: Native Rust Game Module Boundary

Status: Accepted
Date: 2026-07-13

ADR 0052 supersedes Decision 6's whole-world snapshot callback with ABI v3
query-scoped input and deferred output. The remaining native-library safety,
loading, component-storage, host-parity, and packaging decisions stay active.

## Context

Iroha projects are currently data-only. Rust-native components and systems can
only be added by editing the engine workspace, while ADR 0037 explicitly keeps
high-performance reusable gameplay in Rust. Editor Play is in-process (ADR
0024), and packaged games use the generic player (ADR 0045).

Rust has no stable language ABI. Passing trait objects, references, strings, or
other layout-dependent Rust values across a dynamically loaded library boundary
would therefore be unsound. Rebuilding the editor for every gameplay edit would
also prevent the required edit-build-play loop.

## Decision

1. Each project may contain a separate `game/` Cargo project that builds one
   native `cdylib`. Engine source files are never copied into the project.
2. The library exposes one versioned C ABI descriptor. Descriptor fields use
   fixed-width scalars, byte slices, and function pointers only. Rust values do
   not cross the descriptor boundary by value.
3. The game project is compiled against the exact engine SDK selected by the
   editor build invocation. The module ABI version is checked before any
   callback runs. A mismatch is a blocking diagnostic.
4. Component and system declarations use engine-provided derive/attribute
   macros. Registration is collected automatically into the exported module;
   users do not edit an engine registry or startup file.
5. Component authoring values cross callbacks as canonical JSON bytes. The
   host stores project component values in an engine-owned ECS component keyed
   by stable `ComponentTypeId`; plugin-defined Rust layouts never enter host
   archetype storage.
6. Game systems receive a typed `GameWorld` snapshot containing runtime entity
   handles as scalar `(id, generation)` pairs and project component values as
   canonical data. A system decodes values into its Rust component types,
   updates them, and returns the snapshot. The host applies values through
   validated ECS query access. ABI v2 exports stable IDs, display metadata, and
   before/after constraints as JSON. Each callback is registered as an
   individual `Update` or `FixedUpdate` entry; default registration still puts
   Game entries after Engine entries.
7. Editor Play and the generic player load the same module and use the same
   component spawn and system dispatch paths. The module remains loaded until
   every runtime world containing its Rust component values has been dropped.
8. On desktop, build artifacts are copied to generation-specific shadow paths
   before loading. This permits rebuilding the original output on Windows.
9. Native modules are desktop-only in this ADR. wasm32 game-code loading
   requires a separate ABI and packaging decision.

## Safety Invariants

- No callback may unwind across the C ABI. Export helpers catch panics and
  return an error message to the host.
- The library handle outlives all copied function pointers while callbacks may
  run. Host ECS storage never contains plugin-owned Rust layouts or vtables.
- A module built for a different ABI version is rejected before schemas or
  callbacks are used.
- Game callbacks never receive `World`, storage, archetypes, Rust references,
  trait objects, or allocator-owned host values across the ABI.

## Consequences

- Rust gameplay can be rebuilt without restarting the editor, but Play must be
  stopped before adopting a newer module generation.
- Full hot reload and state migration are not provided.
- Game systems are sequential snapshot transforms in the MVP. Structural ECS
  mutation and direct built-in component queries remain future API work.
- Packaging gains a platform-native library next to the generic player.
- Component IDs remain authoring strings and are independent from Rust type,
  file, and display names.

## Alternatives Considered

- Recompile the editor with each project: rejected because it breaks iteration
  and makes the editor project-specific.
- Make an out-of-process player primary: rejected because ADR 0024 deliberately
  selected in-process Play and Game View integration.
- Pass Rust trait objects through a dynamic library: rejected because Rust does
  not guarantee their ABI.
- Use Rhai for this requirement: rejected because ADR 0037 assigns native ECS
  components and high-performance systems to Rust; Rhai remains complementary.

## Compatibility and Migration

Existing data-only projects and packages continue to
work without a `game/` directory. Existing scene and prefab schemas do not
change; game components use the existing `ComponentTypeId` and `Value` forms.
ABI v1 and v2 native libraries must be rebuilt for ABI v3 scoped gameplay I/O.
The loader reports an ABI mismatch without crashing the editor.
