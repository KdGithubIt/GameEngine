# ADR 0116: Animation Events Owned by Animation Set Bindings

Status: Accepted
Date: 2026-08-15
Amends: ADR 0082, ADR 0085
Follows: ADR 0115

## Context

ADR 0085 moved clip selection off the entity-level Animation Controller and
onto Animation Set bindings, and stated that a binding "may add author-owned
timeline events without modifying generated import data". The runtime followed
that decision: `spawn_animation_controller_component` reads clip events only
from the resolved Animation Set and installs them with
`Animator::set_clip_events`.

The pre-0085 authoring surface, listed among the controller's playback settings
by ADR 0082, was never removed. `engine.animation_controller`
still declares an `events` field, the builtin registry still gives it the
`InspectorFieldControl::AnimationEvents` control, `validate_builtin_component_values`
still validates its rows, and the Inspector still renders an "Events" section
for it. Nothing reads the resulting data. An author who adds an event there
sees valid-looking rows, a passing validation pass, and no event at runtime.

The replacement surface is also incomplete. `AnimationBinding::events` is
persisted, validated by `AnimationSet::validate`, resolved by the scene bridge,
and preserved by `AnimationSetEditorState::set_binding`, but the Animation Set
window offers no way to author it. Today the only way to create a working
animation event is to hand-edit `*.animset.json`.

So the engine has one surface that is editable but ignored, and one surface
that is honored but not editable.

## Decision

### 1. Animation events are authored on the Animation Set binding

`AnimationBinding::events` is the single authoring surface for clip timeline
events. The Animation Set window edits those rows directly: add, remove, event
time in seconds, and event name, per bound motion slot.

`AnimationSet::validate` remains the validation owner. A blank event name or a
non-finite/negative time is rejected there, before the document is written,
rather than being reported later as a scene diagnostic.

### 2. The component-level event surface is removed

The `events` field of `engine.animation_controller` is removed from the builtin
component schema, together with the machinery that existed only to serve it:

- the `InspectorFieldControl::AnimationEvents` variant of the public engine
  Inspector-control enum;
- `validate_animation_event_rows` and the `scene.animation_event_invalid`
  diagnostic code;
- the Inspector "Events" section and its event-row editor.

Per ADR 0115 section 5, the tests that asserted the removed rows validate are
removed rather than retargeted at the component.

An `events` key left in an existing scene by an earlier editor build is inert:
the scene bridge reads only the fields it names, and builtin validation is
driven by the current field hints. Such a key is ignored on load, is not
rewritten, and produces no diagnostic. Authors who had entered rows there must
re-enter them on the Animation Set binding, where they had no runtime effect
before.

### 3. Per-event clip filtering is not carried over

The removed component editor exposed an optional `clip` filter per row, because
one controller-level list had to address several clips. A binding targets
exactly one motion slot and one primary clip, so the filter has no meaning on
the new surface and is not reintroduced. `AnimationSetEvent` keeps its current
`time` and `name` shape.

## Consequences

- Animation events become authorable in the editor for the first time, and every
  authorable event is one the runtime actually emits.
- The Animation Controller Inspector loses a section that never affected play.
- `InspectorFieldControl` loses a public variant. The enum is non-exhaustive to
  external matches only by convention, so this is a breaking change for any
  out-of-tree code matching on it; ADR 0115 section 3 accepts that for
  surfaces the current product no longer uses.
- One less validation path runs over scene data. Event validity is checked when
  the Animation Set is saved and when it is loaded during conversion.
