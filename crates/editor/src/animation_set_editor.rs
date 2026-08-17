//! Toolkit-independent Animation Set editor state and undoable mutations.

use engine_authoring::{
    replace_file_contents, AnimationBinding, AnimationSet, AnimationSetEvent, AssetId,
    AuthoringPermission, AuthoringPermissions, MotionSlot, MotionSlotId, MotionSourceRef,
    TypedDocumentAuthoringError,
    TypedDocumentAuthoringMutation, TypedDocumentAuthoringService, TypedDocumentAuthoringSnapshot,
    TypedDocumentAuthoringState, TypedDocumentAuthoringValidation,
};
use std::collections::BTreeSet;
use std::path::PathBuf;

const UNDO_LIMIT: usize = 100;

/// One open author-owned `*.animset.json` document.
pub struct AnimationSetEditorState {
    /// Asset-relative path shown in the editor title.
    pub relative_path: PathBuf,
    /// Absolute path used by atomic save.
    pub absolute_path: PathBuf,
    /// Editable Animation Set document.
    pub document: AnimationSet,
    clean_document: AnimationSet,
    undo: Vec<AnimationSet>,
    redo: Vec<AnimationSet>,
    authoring: TypedDocumentAuthoringState,
}

impl AnimationSetEditorState {
    /// Creates an editor around a loaded Animation Set.
    pub fn new(relative_path: PathBuf, absolute_path: PathBuf, document: AnimationSet) -> Self {
        Self {
            relative_path,
            absolute_path,
            clean_document: document.clone(),
            document,
            undo: Vec::new(),
            redo: Vec::new(),
            authoring: TypedDocumentAuthoringState::new(),
        }
    }

    /// Returns whether the current document differs from the last saved value.
    pub fn is_dirty(&self) -> bool {
        self.document != self.clean_document
    }

    /// Returns process-local revision/generation metadata for this working copy.
    pub fn revision_generation(&self) -> (u64, u64) {
        (self.authoring.revision(), self.authoring.generation())
    }

    /// Replaces this working copy from disk only while it is clean.
    pub fn reload_clean(&mut self, document: AnimationSet) -> bool {
        if self.is_dirty() {
            return false;
        }
        self.document = document.clone();
        self.clean_document = document;
        self.authoring = TypedDocumentAuthoringState::new();
        self.undo.clear();
        self.redo.clear();
        true
    }

    /// Returns whether an undo entry is available.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Returns whether a redo entry is available.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Inspects the Animation Set through the shared typed-document service.
    pub fn structured_inspect(&self, permissions: &AuthoringPermissions) -> Result<TypedDocumentAuthoringSnapshot<AnimationSet>, TypedDocumentAuthoringError> {
        TypedDocumentAuthoringService::new().inspect(&self.document, &self.authoring, permissions)
    }

    /// Validates the Animation Set through the shared typed-document service.
    pub fn structured_validate(&self, permissions: &AuthoringPermissions) -> Result<TypedDocumentAuthoringValidation, TypedDocumentAuthoringError> {
        TypedDocumentAuthoringService::new().validate(&self.document, &self.authoring, permissions)
    }

    /// Previews a complete Animation Set replacement without mutation.
    pub fn structured_preview(&self, permissions: &AuthoringPermissions, expected_revision: u64, expected_generation: u64, replacement: AnimationSet) -> Result<TypedDocumentAuthoringMutation<AnimationSet>, TypedDocumentAuthoringError> {
        TypedDocumentAuthoringService::new().preview(&self.document, &self.authoring, permissions, expected_revision, expected_generation, replacement)
    }

    /// Applies a complete Animation Set replacement as one undoable edit.
    pub fn structured_apply(&mut self, permissions: &AuthoringPermissions, expected_revision: u64, expected_generation: u64, replacement: AnimationSet) -> Result<TypedDocumentAuthoringMutation<AnimationSet>, TypedDocumentAuthoringError> {
        let before = self.document.clone();
        let mutation = TypedDocumentAuthoringService::new().apply(&mut self.document, &mut self.authoring, permissions, expected_revision, expected_generation, replacement)?;
        if mutation.success && !mutation.diff.is_empty() {
            self.push_undo_snapshot(before);
            self.redo.clear();
        }
        Ok(mutation)
    }

    /// Restores the previous document snapshot through the shared semantic boundary.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        let current = self.document.clone();
        match self.apply_editor_replacement(previous.clone()) {
            Ok(true) => {
                self.redo.push(current);
                true
            }
            Ok(false) | Err(_) => {
                self.undo.push(previous);
                false
            }
        }
    }

    /// Reapplies the next document snapshot through the shared semantic boundary.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        let current = self.document.clone();
        match self.apply_editor_replacement(next.clone()) {
            Ok(true) => {
                self.push_undo_snapshot(current);
                true
            }
            Ok(false) | Err(_) => {
                self.redo.push(next);
                false
            }
        }
    }

    /// Returns bindings that are not part of `slots`.
    pub fn stale_bindings(&self, slots: &[MotionSlot]) -> Vec<MotionSlotId> {
        let valid = slots
            .iter()
            .map(|slot| slot.id.clone())
            .collect::<BTreeSet<_>>();
        self.document
            .bindings
            .keys()
            .filter(|slot| !valid.contains(*slot))
            .cloned()
            .collect()
    }

    /// Changes the target graph while preserving bindings with matching IDs.
    ///
    /// When `remove_stale` is false, bindings absent from `slots` remain in
    /// the document. The UI uses [`Self::stale_bindings`] to ask for
    /// confirmation before calling this with `remove_stale == true`.
    pub fn assign_graph(&mut self, graph: AssetId, slots: &[MotionSlot], remove_stale: bool) {
        let mut replacement = self.document.clone();
        replacement.graph = Some(graph);
        if remove_stale {
            let valid = slots
                .iter()
                .map(|slot| slot.id.clone())
                .collect::<BTreeSet<_>>();
            replacement.bindings.retain(|slot, _| valid.contains(slot));
        }
        for slot in slots {
            if let Some(binding) = replacement.bindings.get_mut(&slot.id) {
                binding.name = slot.display_name.clone();
            }
        }
        self.commit_editor_replacement(replacement)
            .expect("Animation Set graph assignment must preserve the shared document contract");
    }

    /// Clears only the graph reference or clears the graph and every binding.
    pub fn clear_graph(&mut self, clear_bindings: bool) {
        let mut replacement = self.document.clone();
        replacement.graph = None;
        if clear_bindings {
            replacement.bindings.clear();
        }
        self.commit_editor_replacement(replacement)
            .expect("Animation Set graph clearing must preserve the shared document contract");
    }

    /// Assigns or clears one explicitly tagged motion source for a graph-owned slot.
    pub fn set_binding_source(
        &mut self,
        slot: &MotionSlot,
        motion: Option<MotionSourceRef>,
    ) -> Result<(), String> {
        if self.document.graph.is_none() {
            return Err("assign an Animation Graph before editing bindings".to_owned());
        }
        let mut replacement = self.document.clone();
        match motion {
            Some(motion) => {
                let events = replacement
                    .bindings
                    .get(&slot.id)
                    .map(|binding| binding.events.clone())
                    .unwrap_or_default();
                let overlays = replacement
                    .bindings
                    .get(&slot.id)
                    .map(|binding| binding.overlays.clone())
                    .unwrap_or_default();
                replacement.bindings.insert(
                    slot.id.clone(),
                    AnimationBinding {
                        name: slot.display_name.clone(),
                        clip: motion,
                        overlays,
                        events,
                    },
                );
            }
            None => {
                replacement.bindings.remove(&slot.id);
            }
        }
        self.commit_editor_replacement(replacement).map(|_| ())
    }

    /// Appends one explicitly tagged supplemental motion source.
    pub fn add_overlay_source(
        &mut self,
        slot: &MotionSlotId,
        motion: MotionSourceRef,
    ) -> Result<(), String> {
        let Some(binding) = self.document.bindings.get(slot) else {
            return Err("assign a primary clip before adding overlays".to_owned());
        };
        if binding.clip == motion || binding.overlays.contains(&motion) {
            return Err("the selected clip is already present in this binding".to_owned());
        }
        let mut replacement = self.document.clone();
        replacement
            .bindings
            .get_mut(slot)
            .expect("binding validated before replacement")
            .overlays
            .push(motion);
        self.commit_editor_replacement(replacement).map(|_| ())
    }

    /// Removes one supplemental clip by its current list index.
    pub fn remove_overlay(&mut self, slot: &MotionSlotId, index: usize) -> Result<(), String> {
        let Some(binding) = self.document.bindings.get(slot) else {
            return Err("the animation slot has no binding".to_owned());
        };
        if index >= binding.overlays.len() {
            return Err("overlay index is out of range".to_owned());
        }
        let mut replacement = self.document.clone();
        replacement
            .bindings
            .get_mut(slot)
            .expect("binding validated before replacement")
            .overlays
            .remove(index);
        self.commit_editor_replacement(replacement).map(|_| ())
    }

    /// Moves one overlay by one position while retaining explicit priority.
    pub fn move_overlay(
        &mut self,
        slot: &MotionSlotId,
        index: usize,
        new_index: usize,
    ) -> Result<(), String> {
        let Some(binding) = self.document.bindings.get(slot) else {
            return Err("the animation slot has no binding".to_owned());
        };
        if index >= binding.overlays.len() || new_index >= binding.overlays.len() {
            return Err("overlay index is out of range".to_owned());
        }
        if index == new_index {
            return Ok(());
        }
        let mut replacement = self.document.clone();
        let overlays = &mut replacement
            .bindings
            .get_mut(slot)
            .expect("binding validated before replacement")
            .overlays;
        let clip = overlays.remove(index);
        overlays.insert(new_index, clip);
        self.commit_editor_replacement(replacement).map(|_| ())
    }

    /// Appends one timeline event to a bound slot (ADR 0116).
    ///
    /// The event is placed after the latest existing marker with a
    /// placeholder name, so the row is immediately visible and editable. The
    /// document stays valid because the placeholder is non-blank and the time
    /// is finite and non-negative.
    pub fn add_event(&mut self, slot: &MotionSlotId) -> Result<(), String> {
        let Some(binding) = self.document.bindings.get(slot) else {
            return Err("assign a clip before adding events".to_owned());
        };
        let time = binding
            .events
            .iter()
            .map(|event| event.time)
            .fold(0.0_f32, f32::max)
            + 0.1;
        let name = unused_event_name(&binding.events);
        let mut replacement = self.document.clone();
        replacement
            .bindings
            .get_mut(slot)
            .expect("binding validated before replacement")
            .events
            .push(AnimationSetEvent { time, name });
        self.commit_editor_replacement(replacement).map(|_| ())
    }

    /// Removes one timeline event by its current list index.
    pub fn remove_event(&mut self, slot: &MotionSlotId, index: usize) -> Result<(), String> {
        let Some(binding) = self.document.bindings.get(slot) else {
            return Err("the animation slot has no binding".to_owned());
        };
        if index >= binding.events.len() {
            return Err("event index is out of range".to_owned());
        }
        let mut replacement = self.document.clone();
        replacement
            .bindings
            .get_mut(slot)
            .expect("binding validated before replacement")
            .events
            .remove(index);
        self.commit_editor_replacement(replacement).map(|_| ())
    }

    /// Replaces one timeline event's time and name.
    ///
    /// Rows are kept in ascending time order so the Animation Set window and
    /// fixed-step delivery agree on event order regardless of edit sequence.
    ///
    /// # Errors
    ///
    /// Returns an error when the slot has no binding, the index is out of
    /// range, the time is not finite and non-negative, or the name is blank.
    /// The document is left untouched and no undo entry is recorded.
    pub fn set_event(
        &mut self,
        slot: &MotionSlotId,
        index: usize,
        time: f32,
        name: &str,
    ) -> Result<(), String> {
        let Some(binding) = self.document.bindings.get(slot) else {
            return Err("the animation slot has no binding".to_owned());
        };
        if index >= binding.events.len() {
            return Err("event index is out of range".to_owned());
        }
        if !time.is_finite() || time < 0.0 {
            return Err("event time must be finite and non-negative".to_owned());
        }
        let name = name.trim();
        if name.is_empty() {
            return Err("event name must not be blank".to_owned());
        }
        let mut replacement = self.document.clone();
        let events = &mut replacement
            .bindings
            .get_mut(slot)
            .expect("binding validated before replacement")
            .events;
        events[index] = AnimationSetEvent {
            time,
            name: name.to_owned(),
        };
        events.sort_by(|left, right| {
            left.time
                .total_cmp(&right.time)
                .then_with(|| left.name.cmp(&right.name))
        });
        self.commit_editor_replacement(replacement).map(|_| ())
    }

    /// Writes canonical JSON atomically and advances the clean baseline.
    pub fn save(&mut self) -> Result<(), String> {
        let json = self
            .document
            .to_canonical_json()
            .map_err(|error| error.to_string())?;
        replace_file_contents(&self.absolute_path, &json).map_err(|error| error.to_string())?;
        self.clean_document = self.document.clone();
        Ok(())
    }

    fn apply_editor_replacement(&mut self, replacement: AnimationSet) -> Result<bool, String> {
        let permissions = AuthoringPermissions::read_only()
            .with(AuthoringPermission::ProjectDataWrite);
        let revision = self.authoring.revision();
        let generation = self.authoring.generation();
        let mutation = TypedDocumentAuthoringService::new()
            .apply(
                &mut self.document,
                &mut self.authoring,
                &permissions,
                revision,
                generation,
                replacement,
            )
            .map_err(|error| error.to_string())?;
        if !mutation.success {
            let message = mutation
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(if message.is_empty() {
                "Animation Set replacement was rejected".to_owned()
            } else {
                message
            });
        }
        Ok(!mutation.diff.is_empty())
    }

    fn commit_editor_replacement(&mut self, replacement: AnimationSet) -> Result<bool, String> {
        let before = self.document.clone();
        let changed = self.apply_editor_replacement(replacement)?;
        if changed {
            self.push_undo_snapshot(before);
            self.redo.clear();
        }
        Ok(changed)
    }

    fn push_undo_snapshot(&mut self, snapshot: AnimationSet) {
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(snapshot);
    }
}

/// Returns an event name that no existing row in `events` already uses.
fn unused_event_name(events: &[AnimationSetEvent]) -> String {
    let taken = events
        .iter()
        .map(|event| event.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut suffix = 1_u32;
    loop {
        let candidate = if suffix == 1 {
            "event".to_owned()
        } else {
            format!("event_{suffix}")
        };
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
        suffix = suffix.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(name: &str) -> MotionSlot {
        MotionSlot {
            id: MotionSlotId::generate(),
            display_name: name.to_owned(),
        }
    }

    fn editor(document: AnimationSet) -> AnimationSetEditorState {
        AnimationSetEditorState::new(
            PathBuf::from("set.animset.json"),
            PathBuf::from("set.animset.json"),
            document,
        )
    }

    #[test]
    fn graph_change_preserves_matching_bindings_and_removes_confirmed_stale_bindings() {
        let old_graph = AssetId::generate();
        let retained = slot("Idle");
        let stale = slot("Attack");
        let mut document = AnimationSet::new(old_graph);
        for motion in [&retained, &stale] {
            document.bindings.insert(
                motion.id.clone(),
                AnimationBinding {
                    name: motion.display_name.clone(),
                    clip: MotionSourceRef::native(AssetId::generate()),
                    overlays: Vec::new(),
                    events: Vec::new(),
                },
            );
        }
        let retained_clip = document.bindings[&retained.id].clip.clone();
        let mut state = editor(document);

        assert_eq!(
            state.stale_bindings(std::slice::from_ref(&retained)),
            vec![stale.id]
        );
        state.assign_graph(AssetId::generate(), std::slice::from_ref(&retained), true);

        assert_eq!(state.document.bindings.len(), 1);
        assert_eq!(state.document.bindings[&retained.id].clip, retained_clip);
    }

    #[test]
    fn clear_graph_can_preserve_or_remove_bindings_and_is_undoable() {
        let motion = slot("Idle");
        let mut document = AnimationSet::new(AssetId::generate());
        document.bindings.insert(
            motion.id,
            AnimationBinding {
                name: motion.display_name,
                clip: MotionSourceRef::native(AssetId::generate()),
                overlays: Vec::new(),
                events: Vec::new(),
            },
        );
        let mut state = editor(document.clone());

        state.clear_graph(false);
        assert!(state.document.graph.is_none());
        assert_eq!(state.document.bindings.len(), 1);
        assert!(state.undo());
        assert_eq!(state.document, document);

        state.clear_graph(true);
        assert!(state.document.graph.is_none());
        assert!(state.document.bindings.is_empty());
    }

    #[test]
    fn overlay_order_mutations_are_explicit_and_undoable() {
        let motion = slot("Dance");
        let mut state = editor(AnimationSet::new(AssetId::generate()));
        state
            .set_binding_source(
                &motion,
                Some(MotionSourceRef::native(AssetId::generate())),
            )
            .expect("primary clip must bind");
        let first = AssetId::generate();
        let second = AssetId::generate();
        state
            .add_overlay_source(&motion.id, MotionSourceRef::native(first.clone()))
            .expect("first overlay must be added");
        state
            .add_overlay_source(&motion.id, MotionSourceRef::native(second.clone()))
            .expect("second overlay must be added");
        state
            .move_overlay(&motion.id, 1, 0)
            .expect("overlay must move");
        assert_eq!(
            state.document.bindings[&motion.id].overlays,
            vec![
                MotionSourceRef::native(second),
                MotionSourceRef::native(first)
            ]
        );
        assert!(state.undo());
        assert_eq!(state.document.bindings[&motion.id].overlays.len(), 2);
        state
            .remove_overlay(&motion.id, 0)
            .expect("overlay must be removed");
        assert_eq!(state.document.bindings[&motion.id].overlays.len(), 1);
    }

    #[test]
    fn added_events_are_named_uniquely_and_stay_in_ascending_time_order() {
        let motion = slot("Attack");
        let mut state = editor(AnimationSet::new(AssetId::generate()));
        state
            .set_binding_source(
                &motion,
                Some(MotionSourceRef::native(AssetId::generate())),
            )
            .expect("primary clip must bind");
        for _ in 0..2 {
            state.add_event(&motion.id).expect("event must be added");
        }
        assert_eq!(
            state.document.bindings[&motion.id]
                .events
                .iter()
                .map(|event| event.name.as_str())
                .collect::<Vec<_>>(),
            vec!["event", "event_2"]
        );

        state
            .set_event(&motion.id, 1, 0.05, "attack.active")
            .expect("event must be edited");

        let events = &state.document.bindings[&motion.id].events;
        assert_eq!(events[0].name, "attack.active");
        assert_eq!(events[0].time, 0.05);
        assert_eq!(events[1].name, "event");
    }

    #[test]
    fn an_invalid_event_edit_is_rejected_without_touching_the_document() {
        let motion = slot("Attack");
        let mut state = editor(AnimationSet::new(AssetId::generate()));
        state
            .set_binding_source(
                &motion,
                Some(MotionSourceRef::native(AssetId::generate())),
            )
            .expect("primary clip must bind");
        state.add_event(&motion.id).expect("event must be added");
        let before = state.document.clone();

        assert!(state.set_event(&motion.id, 0, -1.0, "hit").is_err());
        assert!(state.set_event(&motion.id, 0, 0.5, "   ").is_err());
        assert!(state.set_event(&motion.id, 7, 0.5, "hit").is_err());

        assert_eq!(state.document, before);
        assert!(state.undo());
        assert!(
            state.document.bindings[&motion.id].events.is_empty(),
            "a rejected edit must not record an undo entry, so one undo must reach the add"
        );
    }

    #[test]
    fn events_survive_a_clip_reassignment_and_removal_is_undoable() {
        let motion = slot("Attack");
        let mut state = editor(AnimationSet::new(AssetId::generate()));
        state
            .set_binding_source(
                &motion,
                Some(MotionSourceRef::native(AssetId::generate())),
            )
            .expect("primary clip must bind");
        state.add_event(&motion.id).expect("event must be added");

        state
            .set_binding_source(
                &motion,
                Some(MotionSourceRef::native(AssetId::generate())),
            )
            .expect("clip must be reassigned");
        assert_eq!(state.document.bindings[&motion.id].events.len(), 1);

        state
            .remove_event(&motion.id, 0)
            .expect("event must be removed");
        assert!(state.document.bindings[&motion.id].events.is_empty());
        assert!(state.undo());
        assert_eq!(state.document.bindings[&motion.id].events.len(), 1);
    }

    #[test]
    fn events_require_a_bound_clip() {
        let motion = slot("Attack");
        let mut state = editor(AnimationSet::new(AssetId::generate()));

        assert!(state.add_event(&motion.id).is_err());
    }

    #[test]
    fn gui_edits_and_undo_advance_the_shared_authoring_revision() {
        let motion = slot("Idle");
        let mut state = editor(AnimationSet::new(AssetId::generate()));
        let read = AuthoringPermissions::read_only();
        let baseline = state
            .structured_inspect(&read)
            .expect("initial inspect must succeed");

        state
            .set_binding_source(
                &motion,
                Some(MotionSourceRef::native(AssetId::generate())),
            )
            .expect("GUI binding edit must commit");
        let edited = state
            .structured_inspect(&read)
            .expect("edited inspect must succeed");
        assert_eq!(edited.generation, baseline.generation);
        assert_eq!(edited.revision, baseline.revision + 1);

        let write = AuthoringPermissions::read_only()
            .with(AuthoringPermission::ProjectDataWrite);
        let stale = state.structured_apply(
            &write,
            baseline.revision,
            baseline.generation,
            baseline.document.clone(),
        );
        assert!(matches!(stale, Err(TypedDocumentAuthoringError::Stale { .. })));

        assert!(state.undo());
        let undone = state
            .structured_inspect(&read)
            .expect("undo inspect must succeed");
        assert_eq!(undone.document, baseline.document);
        assert_eq!(undone.revision, edited.revision + 1);
    }
}
