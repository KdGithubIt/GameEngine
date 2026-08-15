# ADR 0069: Inactive Components for Unassigned Asset References

- Status: Accepted
- Date: 2026-07-20

## Context

Several builtin components require an asset reference that has no sensible
default: `engine.animator` (`clip_source`), `engine.animation_graph_player`
(`graph`, `clip_source`), `engine.behavior_tree_runner` (`graph`),
`engine.audio_emitter` (`clip`), `engine.music_controller` (`clip`),
`engine.skinned_mesh_source` (`source`), and `engine.nav_mesh_surface`
(`source`). Add Component therefore always produces a value without the
reference, and conversion rejected that value as invalid. Until ADR 0068 this
blanked the whole Scene View; even afterwards Play refused to start, and the
Problems panel reported a blocking error for a state every user passes
through while editing.

Unity, Godot, and Unreal all treat an unassigned reference as a valid inert
state: the component simply does nothing until the reference is assigned.

## Decision

An **absent** required asset-reference field converts the component to its
inactive state under every conversion policy, including strict Play/runtime
conversion:

- The spawn callback adds nothing for that component, the rest of the entity
  and scene convert normally, and one non-blocking
  `scene_bridge.component_inactive` warning is recorded per inactive
  component.
- A field that is **present but not an `AssetRef`** remains an error: that is
  corrupted data, not an editing gap. `scene_bridge` distinguishes the two
  through `ComponentFields::assignable_asset_ref`.
- Authoring-side validation reports the unassigned state as the non-blocking
  warning `scene.component_reference_unassigned` instead of the blocking
  `scene.component_field_missing`, so the Problems panel guides the user
  without blocking Play.
- References with builtin defaults (`engine.mesh`, `engine.material`,
  `engine.ui_document`, `engine.particle_emitter.mesh`, LOD levels) keep
  their defaults and are unaffected.

## Consequences

- Adding a component is never an error state; Scene View, Play, the player,
  and packaging all proceed with the component inert plus a warning.
- Serialized formats are unchanged: "field absent" is the representation the
  editor already produced.
- A user can now ship a package with an unassigned reference; the warning
  diagnostics are the guard rail, so editor surfaces (Problems, Console)
  must keep showing them.
