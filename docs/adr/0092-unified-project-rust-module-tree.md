# ADR 0092: Unified Project Rust Module Tree

- Status: Accepted
- Date: 2026-07-27
- Deciders: GameEngine authoring/editor owners
- Related: ADR 0027 (component registry), ADR 0050 (game module lifecycle), ADR 0052 (game module safety contract)

## Context

Project Rust gameplay sources lived below four fixed folders:

```text
assets/scripts/rust/components/
assets/scripts/rust/resources/
assets/scripts/rust/systems/
assets/scripts/rust/shared/
```

The folder was the classifier. `crates/authoring` generated one module index per
category into `game/src/components/mod.rs`, `game/src/resources/mod.rs`, and
`game/src/systems/mod.rs`, plus a `game/src/shared.rs` bridge to a user-managed
`assets/scripts/rust/shared/mod.rs`. The editor's asset operations refused any
move that crossed a category, refused every move of a `shared` module, and
refused a `.rs` file anywhere else below the Rust root. One snake_case file name
had to be unique across all four categories.

That model made gameplay code hard to organize by feature. An author could not
keep `player/health.rs`, `player/movement/move_rule.rs`, and
`player/combat/player_attack.rs` together, and could not give a player and an
enemy a `health.rs` each. It also duplicated knowledge: the folder said one
thing, the `#[derive(engine::GameComponent)]` in the source said another, and
only the folder was enforced.

## Decision

`assets/scripts/rust/` is one free-form Rust module tree.

1. **Folder structure is the module path.** The complete tree below the Rust
   script root is walked and emitted into one generated file,
   `game/src/project_modules.rs`, which `game/src/lib.rs` pulls in with
   `include!("project_modules.rs");`. Because the index is included at the crate
   root, `assets/scripts/rust/player/movement/move_rule.rs` is reachable as
   `crate::player::movement::move_rule::MoveRule`. Directories become inline
   `pub mod` blocks and files become `#[path = "..."] pub mod` declarations.

2. **Declarations classify sources, not folders.** `rust_declarations` in
   `crates/authoring/src/game_project.rs` inspects the source text and reports
   `Component` for a `GameComponent` derive or `#[game_component(...)]`,
   `Resource` for a `GameResource` derive or `#[game_resource(...)]`, and
   `System` for `#[game_system(...)]`. Anything else is an ordinary compiled
   module. Comments are blanked before the scan and multi-line attributes are
   joined, so a commented-out or wrapped declaration is not misread.

3. **`mod.rs` below the Rust script root is prohibited.** The engine owns module
   declarations; a hand-written index would silently compete with the generated
   one, and `player/health.rs` next to `player/health/mod.rs` has no unambiguous
   meaning. Index generation rejects any `mod.rs` with a message telling the
   author to rename it to an ordinary module. Project initialization first
   deletes a `mod.rs` that only declares submodules, because that shape is the
   scaffolding earlier versions generated.

4. **The four folders stay as recommendations.** Initialization still creates
   `components/`, `resources/`, `systems/`, and `shared/`, and the create
   commands still default to them per kind. They are default destinations only.

5. **Uniqueness moves from file names to IDs.** Two folders may each hold a
   `health.rs`. Two entries in one folder that generate the same module name are
   rejected. Stable component IDs stay unique across the whole tree, and
   duplicated `game_resource`/`game_system` registration IDs are now rejected by
   the same validation pass instead of surfacing at module load.

6. **`use` paths are not rewritten on move.** Moving a source changes its module
   path, and the affected `use` lines are reported by `cargo check`. Automatic
   reference rewriting is a separate refactoring feature.

## Consequences

### Positive

- Gameplay code can be organized by feature instead of by engine concept.
- The folder and the source can no longer disagree about what a file is.
- Ordinary helper modules work anywhere, not only below `shared/`.
- One generated file replaces four generated bridges.

### Negative

- Moving a source breaks `use` paths that referenced its old module path until
  the author fixes them; the editor warns and the next build reports the lines.
- A hand-written `mod.rs` that contains code is now an error the author must
  resolve by renaming the file.

### Compatibility

Existing projects migrate in place on the next initialization or index refresh.
The generated per-category declarations are removed from `game/src/lib.rs`, the
generated bridges under `game/src/` are deleted, the generated
`assets/scripts/rust/shared/mod.rs` is deleted, and the unified index is
written. Custom lines in `lib.rs`, `Cargo.toml` settings, and every user source
are preserved byte-for-byte.

A project that keeps the recommended folders keeps its Rust paths: a component
in `components/health.rs` remains `crate::components::health::Health` and a
helper in `shared/math.rs` remains `crate::shared::math`, so no `use` line in an
unmoved project changes. Stable component IDs, sidecar contents, scenes,
prefabs, and the asset manifest are untouched.

## Alternatives considered

- **`mod project_modules;` plus `pub use project_modules::*;`** — keeps the
  index a normal module, but folder paths would only resolve through a glob
  re-export, which weakens error messages and can be shadowed. Rejected in
  favor of `include!`, which makes the folder path the literal module path.
- **Regenerating `game/src/lib.rs` wholesale** — simpler generator, but it would
  overwrite crate attributes and any code an author added to the host crate.
- **Introducing a real Rust parser (`syn`) for classification** — correct in
  every edge case, but a large dependency and a large change for a scanner that
  only has to recognize four attribute shapes. The existing line scanner was
  extended with comment stripping instead.
