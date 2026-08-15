# ADR 0070: Entity Enabled Flag

Status: Accepted
Date: 2026-07-20

## Context

Editors such as Unity offer a per-object "active" checkbox that removes an
object (and its children) from the running game without deleting it. The
authoring model had no equivalent: `AuthoringEntity` carried only identity,
naming, parent, and components, so temporarily excluding an entity required
deleting it or hand-editing components. The editor-side "hidden entities"
set is presentation-only state and never affects Play or packaging.

## Decision

1. `AuthoringEntity` gains a `enabled: bool` field.
   - Serialized as `enabled`, defaulting to `true` on load
     (`#[serde(default)]`), and omitted from output while `true`
     (`skip_serializing_if`). Scene files authored before this ADR keep
     their exact bytes, and older readers keep loading newer files that
     never disable anything.
2. A new `AuthoringCommand::SetEntityEnabled { entity, enabled }` toggles
   the flag through the ordinary transaction pipeline, producing a
   `Change::EntityEnabledChanged` record and an inverse command so undo
   works like every other entity edit.
3. Runtime conversion (`prepare_conversion_plan` in `scene_bridge`) skips
   an entity when it or any ancestor is disabled. The cascade matches the
   SetActive mental model: disabling a parent disables the whole subtree
   regardless of child flags.
4. The editor Hierarchy shows a checkbox per row bound to the flag and
   renders disabled rows with weak text. The scene-view edit-mode preview
   uses the same conversion path and therefore reflects the flag
   immediately.

## Consequences

- Serialized format change is additive and backward compatible in both
  directions for scenes that never use the flag; a round-trip test guards
  the default-omission rule.
- Runtime-side `SetActive`-style toggling during Play is intentionally out
  of scope; the flag is an authoring-time construct. A future runtime
  equivalent would be a separate component/command and a new ADR.
- Prefab instantiation and duplication copy the flag like any other
  entity field because they clone `AuthoringEntity` values.
