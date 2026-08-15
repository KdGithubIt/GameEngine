# ADR 0102: Built-in Component Declaration Table

Status: Accepted
Date: 2026-08-13

## Context

Adding one built-in authorable component required editing about ten files and
writing roughly 330 lines, of which only the runtime behavior was specific to
the component. The field list itself was written three times:

1. `components/schemas.rs` — name, label, description, type, default.
2. `components.rs` — a `*_FIELD_HINTS` constant restating the field names
   together with their Inspector control and numeric range.
3. `scene_bridge/spawn.rs` — the spawn callback restating the field names,
   their conversions, and their range checks.

Three sources for one fact drift. Auditing them during this change found that
`engine.damage_receiver.health` and
`engine.damage_receiver.invulnerability_seconds` declared an exclusive
`> 0` range for the Inspector and pre-Play validation, while the spawn
callback — which ADR 0054 §4 names the final authority — accepts `>= 0`. An
author could not enter a health of zero that the runtime accepts.

`ComponentRegistry` also required a matching `registry.register(...)` block per
component, and `components/tests.rs` asserted a hardcoded
`registry.len() == 34`, so the count was a further per-component edit point.

ADR 0054 §Verification already requires "parameterized tests" covering every
registered component. No parameterized harness existed; each component carried
its own hand-written copies of the same assertions, which is where most of the
per-component line count went.

## Decision

1. Every built-in authorable component is declared exactly once, as a
   `BuiltinComponent` entry in `crates/engine/src/components/builtins.rs`.
   Each of its fields is one `FieldDef` carrying the name, label, description,
   value kind, requiredness, default, Inspector control, and visibility
   condition.
2. The authoring `ComponentSchema` and the `InspectorHint` are **derived** from
   that declaration. `builtin_registry()` iterates the table; neither the set
   of built-ins nor any field is stated a second time.
3. A numeric range is declared once, on the field, as
   `InspectorFieldControl::Number`. The Inspector, pre-Play validation, and the
   scene bridge all read that one declaration. Where the previous Inspector
   range and spawn check disagreed, the spawn check wins, per ADR 0054 §4.
4. `InspectorFieldHint` is removed. `InspectorHint::Fields` carries
   `&'static [FieldDef]`; consumers look fields up by name exactly as before.
5. Defaults for scalar fields are read from the runtime type's `Default`
   through `FieldDefaultSpec::Computed`, so a schema default cannot drift from
   the value the runtime actually uses.
6. `engine.transform` and `engine.player_marker` keep taking their schema from
   the authoring built-in registry via `schema_override`. That registry is the
   format authority for them, and restating their fields in the engine would
   recreate the problem this ADR removes.
7. Component conformance is checked by tests that iterate the declaration
   table, satisfying ADR 0054's parameterized-test requirement. A new component
   is covered the moment it is declared.

## Consequences

- A new built-in component is one table entry plus its spawn callback and
  runtime behavior. The schema function, the hint constant, the registry
  block, and the count assertion are all gone.
- The persisted authoring format is unchanged. All 34 component schemas
  serialize byte-for-byte identically to their pre-migration output; this was
  verified by snapshotting `builtin_registry()` before and after.
- The only intentional behavior change is the `engine.damage_receiver` range
  alignment in §3. Scene files are unaffected: values that were already stored
  remain valid, and the Inspector now accepts the same range the runtime does.
- Fields that previously had no hint entry now appear in the table with no
  control and no visibility condition. Lookup is by name, so this is inert.
- `FieldDefaultSpec` compares by the value it produces rather than by function
  pointer, because function-pointer identity is not meaningful.

## Alternatives Considered

**Derive the schema from the runtime struct**, as `#[derive(GameComponent)]`
does for project-local components. Rejected: built-in components deliberately
publish authoring field names that do not match their runtime layout
(`direction_x/y/z` for one `Vec3`, `lifetime_min/max` for one tuple). A derive
would change persisted field names, which the format forbids.

**Leave the schemas alone and unify only the ranges.** Rejected: it would fix
the drift but leave the per-component line count — the actual complaint — and
would keep two places to edit when adding a component.

## Verification

- All 34 schemas serialize identically before and after the migration.
- Inspector controls are unchanged except for the documented
  `engine.damage_receiver` alignment.
- `cargo test --workspace` passes with no new or removed test coverage.
