//! Animation Graph states, transitions, and graph-owned motion slots.
//!
//! Motion slots are stable identifiers stored in a graph annotation, never
//! derived from State properties, so renaming a slot cannot silently
//! reassign the clip a State plays.

use super::errors::EditorSessionError;
use super::{EditorGraphDomain, EditorSession};
use engine_authoring::{
    animation_graph_motion_slots, motion_slots_annotation_value, Diagnostic, Edge, EdgeId,
    GraphCommand, MotionSlot, MotionSlotId, Node, NodeId, NodeLayout, PortRef, Value, Vec2,
    MOTION_SLOTS_ANNOTATION,
};
use std::collections::{BTreeMap, BTreeSet};

/// Animation state-machine node variants offered by the visual editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationNodeInsertKind {
    /// The unique graph entry point that selects the initial state.
    Entry,
    /// A playable state whose stable motion slot resolves through an Animation Set.
    State,
}

impl EditorSession {
    /// Adds one schema-defined Animation Graph node.
    ///
    /// Entry and State nodes are constructed from the active Animation Graph
    /// domain so the editor never duplicates stable node type identifiers.
    /// Semantic creation commits before optional presentation placement, in
    /// accordance with ADR 0016.
    pub fn add_animation_node(
        &mut self,
        kind: AnimationNodeInsertKind,
        position: Option<Vec2>,
    ) -> Result<NodeId, EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "add Animation Graph node",
            });
        };
        let node_id = NodeId::generate();
        let properties = match kind {
            AnimationNodeInsertKind::Entry => Value::Object(BTreeMap::new()),
            AnimationNodeInsertKind::State => Value::Object(BTreeMap::from([(
                engine_authoring::ANIMATION_STATE_PLAYBACK_MODE_PROPERTY.to_owned(),
                Value::String(
                    engine_authoring::AnimationStatePlaybackMode::Loop
                        .persisted_name()
                        .to_owned(),
                ),
            )])),
        };
        let node_type = match kind {
            AnimationNodeInsertKind::Entry => domain.entry_type().clone(),
            AnimationNodeInsertKind::State => domain.state_type().clone(),
        };
        let node = Node::new(node_id.clone(), node_type, properties);

        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddNode { node })?;
        if let Some(position) = position
            && let Err(error) = self.set_node_layout(node_id.clone(), NodeLayout::new(position)) {
                self.diagnostics.push(Diagnostic::warning(
                    "editor.presentation_after_semantic_failed",
                    format!(
                        "semantic animation node was added but presentation placement failed: {error}"
                    ),
                ));
            }
        if let Err(error) = self.select_node(Some(node_id.clone())) {
            self.diagnostics.push(Diagnostic::warning(
                "editor.presentation_after_semantic_failed",
                format!("semantic animation node was added but selection failed: {error}"),
            ));
        }
        Ok(node_id)
    }

    /// Assigns or clears one Animation State's graph-owned Motion Slot.
    ///
    /// A chosen slot is always a complete value, so this commits on its own as
    /// a single undo step rather than waiting for a separate confirmation. Use
    /// [`EditorSession::set_node_name`] to rename the same State; the two edits
    /// are independent and undo separately.
    ///
    /// The State's `motion_name` label is dropped because the stable slot
    /// replaces it.
    ///
    /// # Errors
    ///
    /// Returns an error when the active document is not an Animation Graph,
    /// when `node` is not an Animation State, or when `motion_slot` is not
    /// owned by the active graph.
    pub fn set_animation_state_motion_slot(
        &mut self,
        node: NodeId,
        motion_slot: Option<MotionSlotId>,
    ) -> Result<(), EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph state",
            });
        };
        let Some(existing) = self.graph.nodes.get(&node) else {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "state node `{}` no longer exists",
                node.as_str()
            )));
        };
        if existing.node_type != *domain.state_type() {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph state",
            });
        }
        let mut properties = match existing.properties.clone() {
            Value::Object(properties) => properties,
            _ => BTreeMap::new(),
        };
        if let Some(slot) = motion_slot {
            let slots = self.motion_slots()?;
            if !slots.iter().any(|candidate| candidate.id == slot) {
                return Err(EditorSessionError::InvalidGraphConnection(format!(
                    "motion slot `{}` is not owned by the active Animation Graph",
                    slot.as_str()
                )));
            }
            properties.insert(
                "motion_slot".to_owned(),
                Value::String(slot.as_str().to_owned()),
            );
        } else {
            properties.remove("motion_slot");
        }
        properties.remove("motion_name");
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetNodeProperty {
            node,
            value: Value::Object(properties),
        })
    }

    /// Assigns one Animation State's clip completion behavior.
    ///
    /// [`engine_authoring::AnimationStatePlaybackMode::Loop`] and
    /// [`engine_authoring::AnimationStatePlaybackMode::Once`] are persisted
    /// directly on the State.
    ///
    /// # Errors
    ///
    /// Returns an error when the active document is not an Animation Graph or
    /// when `node` is not an Animation State.
    pub fn set_animation_state_playback_mode(
        &mut self,
        node: NodeId,
        playback_mode: engine_authoring::AnimationStatePlaybackMode,
    ) -> Result<(), EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph state playback mode",
            });
        };
        let Some(existing) = self.graph.nodes.get(&node) else {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "state node `{}` no longer exists",
                node.as_str()
            )));
        };
        if existing.node_type != *domain.state_type() {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph state playback mode",
            });
        }
        let mut properties = match existing.properties.clone() {
            Value::Object(properties) => properties,
            _ => BTreeMap::new(),
        };
        properties.insert(
            engine_authoring::ANIMATION_STATE_PLAYBACK_MODE_PROPERTY.to_owned(),
            Value::String(playback_mode.persisted_name().to_owned()),
        );
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetNodeProperty {
            node,
            value: Value::Object(properties),
        })
    }

    /// Returns the stable motion slots owned by the active Animation Graph.
    ///
    /// A graph without the slot annotation owns no slots and returns an empty
    /// list; slots are never derived from State properties.
    pub fn motion_slots(&self) -> Result<Vec<MotionSlot>, EditorSessionError> {
        if !self.is_animation_graph() {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "query Animation Graph motion slots",
            });
        }
        animation_graph_motion_slots(&self.graph)
            .map_err(EditorSessionError::InvalidGraphConnection)
    }

    /// Adds one graph-owned motion slot and returns its stable ID.
    pub fn add_motion_slot(
        &mut self,
        display_name: impl Into<String>,
    ) -> Result<MotionSlotId, EditorSessionError> {
        let display_name = display_name.into().trim().to_owned();
        if display_name.is_empty() {
            return Err(EditorSessionError::InvalidGraphConnection(
                "motion slot display name must not be blank".to_owned(),
            ));
        }
        let mut slots = self.motion_slots()?;
        if slots
            .iter()
            .any(|slot| slot.display_name.eq_ignore_ascii_case(&display_name))
        {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "motion slot display name `{display_name}` is already in use"
            )));
        }
        let id = MotionSlotId::generate();
        slots.push(MotionSlot {
            id: id.clone(),
            display_name,
        });
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetGraphAnnotation {
            key: MOTION_SLOTS_ANNOTATION.to_owned(),
            value: Some(motion_slots_annotation_value(&slots)),
        })?;
        Ok(id)
    }

    /// Renames one graph-owned motion slot without changing its stable ID.
    pub fn rename_motion_slot(
        &mut self,
        id: MotionSlotId,
        display_name: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        let display_name = display_name.into().trim().to_owned();
        if display_name.is_empty() {
            return Err(EditorSessionError::InvalidGraphConnection(
                "motion slot display name must not be blank".to_owned(),
            ));
        }
        let mut slots = self.motion_slots()?;
        if slots
            .iter()
            .any(|slot| slot.id != id && slot.display_name.eq_ignore_ascii_case(&display_name))
        {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "motion slot display name `{display_name}` is already in use"
            )));
        }
        let Some(slot) = slots.iter_mut().find(|slot| slot.id == id) else {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "motion slot `{}` is not owned by the active Animation Graph",
                id.as_str()
            )));
        };
        slot.display_name = display_name;
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetGraphAnnotation {
            key: MOTION_SLOTS_ANNOTATION.to_owned(),
            value: Some(motion_slots_annotation_value(&slots)),
        })
    }

    /// Returns State nodes that currently select `id`.
    pub fn states_using_motion_slot(&self, id: &MotionSlotId) -> Vec<(NodeId, String)> {
        self.graph
            .nodes
            .iter()
            .filter_map(|(node_id, node)| {
                if node.node_type.as_str() != "anim.state" {
                    return None;
                }
                let Value::Object(properties) = &node.properties else {
                    return None;
                };
                let uses_slot = matches!(
                    properties.get("motion_slot"),
                    Some(Value::String(value)) if value == id.as_str()
                );
                uses_slot.then(|| {
                    (
                        node_id.clone(),
                        node.name
                            .clone()
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or_else(|| node_id.as_str().to_owned()),
                    )
                })
            })
            .collect()
    }

    /// Deletes one graph-owned slot and clears every State that used it.
    ///
    /// The caller is responsible for confirming the affected States shown by
    /// [`Self::states_using_motion_slot`] before invoking this operation.
    pub fn delete_motion_slot(&mut self, id: MotionSlotId) -> Result<(), EditorSessionError> {
        let mut slots = self.motion_slots()?;
        let previous_len = slots.len();
        slots.retain(|slot| slot.id != id);
        if slots.len() == previous_len {
            return Err(EditorSessionError::InvalidGraphConnection(format!(
                "motion slot `{}` is not owned by the active Animation Graph",
                id.as_str()
            )));
        }

        let affected = self
            .states_using_motion_slot(&id)
            .into_iter()
            .map(|(node, _)| node)
            .collect::<BTreeSet<_>>();
        let mut commands = vec![GraphCommand::SetGraphAnnotation {
            key: MOTION_SLOTS_ANNOTATION.to_owned(),
            value: Some(motion_slots_annotation_value(&slots)),
        }];
        for node_id in affected {
            let Some(node) = self.graph.nodes.get(&node_id) else {
                continue;
            };
            let Value::Object(mut properties) = node.properties.clone() else {
                continue;
            };
            properties.remove("motion_slot");
            properties.remove("motion_name");
            commands.push(GraphCommand::SetNodeProperty {
                node: node_id,
                value: Value::Object(properties),
            });
        }
        self.push_undo_checkpoint();
        self.apply_graph_commands(commands)
    }

    /// Connects two Animation Graph nodes with a directed transition edge.
    ///
    /// Entry may connect to State and State may connect to State. State to
    /// Entry and Entry to Entry are rejected before a graph command is issued,
    /// producing an actionable editor error instead of a low-level port
    /// diagnostic.
    pub fn connect_animation_transition(
        &mut self,
        source: NodeId,
        target: NodeId,
    ) -> Result<EdgeId, EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "connect Animation Graph transition",
            });
        };
        let source_node = self.graph.nodes.get(&source).ok_or_else(|| {
            EditorSessionError::InvalidGraphConnection(format!(
                "source node `{}` no longer exists",
                source.as_str()
            ))
        })?;
        let target_node = self.graph.nodes.get(&target).ok_or_else(|| {
            EditorSessionError::InvalidGraphConnection(format!(
                "target node `{}` no longer exists",
                target.as_str()
            ))
        })?;
        if target_node.node_type != *domain.state_type() {
            return Err(EditorSessionError::InvalidGraphConnection(
                "Animation Graph transitions must target a State node".to_owned(),
            ));
        }
        let source_is_entry = source_node.node_type == *domain.entry_type();
        if source_is_entry
            && self
                .graph
                .edges
                .values()
                .any(|edge| edge.from.node == source)
        {
            return Err(EditorSessionError::InvalidGraphConnection(
                "Entry already has an initial State; delete its existing connection first"
                    .to_owned(),
            ));
        }
        let source_port = if source_is_entry {
            domain.entry_out_port().clone()
        } else if source_node.node_type == *domain.state_type() {
            domain.state_out_port().clone()
        } else {
            return Err(EditorSessionError::InvalidGraphConnection(
                "Animation Graph transitions must start from Entry or State".to_owned(),
            ));
        };
        let edge_id = EdgeId::generate();
        let edge = Edge::new(
            edge_id.clone(),
            PortRef::new(source, source_port),
            PortRef::new(target, domain.state_in_port().clone()),
        );
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddEdge { edge })?;
        Ok(edge_id)
    }

    /// Replaces one State-to-State transition's condition and fade override.
    ///
    /// A blank condition remains the domain's explicit unconditional form.
    /// `fade_duration == None` delegates to the Animation Controller default.
    /// The existing edge ID and unrelated annotations are preserved.
    pub fn set_animation_transition(
        &mut self,
        edge: EdgeId,
        condition: impl Into<String>,
        fade_duration: Option<f64>,
    ) -> Result<(), EditorSessionError> {
        let EditorGraphDomain::Animation(domain) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "edit Animation Graph transition",
            });
        };
        if fade_duration.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(EditorSessionError::InvalidAnimationTransition(
                "fade duration must be a finite non-negative number".to_owned(),
            ));
        }
        let existing = self.graph.edges.get(&edge).cloned().ok_or_else(|| {
            EditorSessionError::InvalidAnimationTransition(format!(
                "transition `{}` no longer exists",
                edge.as_str()
            ))
        })?;
        let source_is_state = self
            .graph
            .nodes
            .get(&existing.from.node)
            .is_some_and(|node| node.node_type == *domain.state_type());
        let target_is_state = self
            .graph
            .nodes
            .get(&existing.to.node)
            .is_some_and(|node| node.node_type == *domain.state_type());
        if !source_is_state || !target_is_state {
            return Err(EditorSessionError::InvalidAnimationTransition(
                "Entry connections do not have runtime transition conditions".to_owned(),
            ));
        }

        let mut replacement = existing.clone();
        let condition = condition.into().trim().to_owned();
        if condition.is_empty() {
            replacement.annotations.remove("condition");
        } else {
            replacement
                .annotations
                .insert("condition".to_owned(), Value::String(condition));
        }
        if let Some(fade_duration) = fade_duration {
            replacement
                .annotations
                .insert("fade_duration".to_owned(), Value::F64(fade_duration));
        } else {
            replacement.annotations.remove("fade_duration");
        }

        self.push_undo_checkpoint();
        self.apply_graph_commands([
            GraphCommand::DeleteEdge { edge },
            GraphCommand::AddEdge { edge: replacement },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::GraphNodeInsertKind;

    #[test]
    fn empty_animation_graph_contains_one_entry_and_view_layout() {
        let session = EditorSession::empty_animation_graph();

        assert!(session.is_animation_graph());
        assert_eq!(session.graph().kind.as_str(), "anim.graph");
        let entries = session
            .graph()
            .nodes
            .values()
            .filter(|node| node.node_type.as_str() == "anim.entry")
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        assert!(session
            .graph_view()
            .is_some_and(|view| view.nodes.contains_key(&entries[0].id)));
        assert_eq!(
            session.available_graph_node_kinds(),
            vec![GraphNodeInsertKind::Animation(
                AnimationNodeInsertKind::State
            )]
        );
    }

    /// Reproduces the graph view that a node deletion used to strand.
    ///
    /// Presentation validation inspects the whole document, so a view left
    /// naming the deleted node rejected every later command — clicking any
    /// node or transition reported a blocked transaction until the document
    /// was reopened.

    #[test]
    fn animation_transition_annotations_are_command_backed_and_undoable() {
        let mut session = EditorSession::empty_animation_graph();
        let entry = session
            .graph()
            .nodes
            .values()
            .find(|node| node.node_type.as_str() == "anim.entry")
            .expect("new graph must have Entry")
            .id
            .clone();
        let walk = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(220.0, 0.0)))
            .expect("walk State should be added");
        let run = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(440.0, 0.0)))
            .expect("run State should be added");

        session
            .connect_animation_transition(entry, walk.clone())
            .expect("Entry should connect to a State");
        let transition = session
            .connect_animation_transition(walk, run)
            .expect("State should connect to State");
        session
            .set_animation_transition(transition.clone(), "is_running", Some(0.15))
            .expect("typed transition settings should apply");

        let annotations = &session.graph().edges[&transition].annotations;
        assert_eq!(
            annotations.get("condition"),
            Some(&Value::String("is_running".to_owned()))
        );
        assert_eq!(annotations.get("fade_duration"), Some(&Value::F64(0.15)));

        assert!(
            session.undo(),
            "transition edit should create one undo step"
        );
        assert!(session.graph().edges[&transition].annotations.is_empty());
    }

    #[test]
    fn animation_state_name_and_graph_owned_slot_undo_independently() {
        let mut session = EditorSession::empty_animation_graph();
        let motion_slot = session
            .add_motion_slot("Walk")
            .expect("Motion Slot should be added");
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("State should be added");

        session
            .set_node_name(state.clone(), "Walk")
            .expect("state name should apply");
        session
            .set_animation_state_motion_slot(state.clone(), Some(motion_slot.clone()))
            .expect("state slot should apply");

        assert_eq!(session.graph().nodes[&state].name.as_deref(), Some("Walk"));
        assert_eq!(
            session.graph().nodes[&state].properties,
            Value::Object(BTreeMap::from([
                (
                    "motion_slot".to_owned(),
                    Value::String(motion_slot.as_str().to_owned()),
                ),
                ("playback_mode".to_owned(), Value::String("loop".to_owned()),),
            ]))
        );

        assert!(session.undo(), "slot edit should be its own undo step");
        assert_eq!(
            session.graph().nodes[&state].properties,
            Value::Object(BTreeMap::from([(
                "playback_mode".to_owned(),
                Value::String("loop".to_owned()),
            )]))
        );
        assert_eq!(
            session.graph().nodes[&state].name.as_deref(),
            Some("Walk"),
            "undoing the slot must not revert the separately committed name"
        );

        assert!(session.undo(), "name edit should be its own undo step");
        assert_eq!(session.graph().nodes[&state].name, None);
    }

    #[test]
    fn assigning_motion_slot_drops_the_state_motion_label() {
        let mut session = EditorSession::empty_animation_graph();
        let motion_slot = session
            .add_motion_slot("Idle")
            .expect("Motion Slot should be added");
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("State should be added");
        session
            .set_node_property(
                state.clone(),
                Value::Object(BTreeMap::from([(
                    "motion_name".to_owned(),
                    Value::String("Idle".to_owned()),
                )])),
            )
            .expect("state properties should be writable");

        session
            .set_animation_state_motion_slot(state.clone(), Some(motion_slot.clone()))
            .expect("state slot should apply");

        assert_eq!(
            session.graph().nodes[&state].properties,
            Value::Object(BTreeMap::from([(
                "motion_slot".to_owned(),
                Value::String(motion_slot.as_str().to_owned()),
            )]))
        );
    }

    #[test]
    fn motion_slot_not_owned_by_active_graph_is_rejected() {
        let mut session = EditorSession::empty_animation_graph();
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("State should be added");
        let foreign = MotionSlotId::generate();

        let error = session
            .set_animation_state_motion_slot(state.clone(), Some(foreign))
            .expect_err("a slot from another graph must be rejected");

        assert!(matches!(
            error,
            EditorSessionError::InvalidGraphConnection(_)
        ));
        assert_eq!(
            session.graph().nodes[&state].properties,
            Value::Object(BTreeMap::from([(
                "playback_mode".to_owned(),
                Value::String("loop".to_owned()),
            )])),
            "a rejected slot must not mutate the State"
        );
    }

    #[test]
    fn deleting_motion_slot_unassigns_using_states_reports_diagnostic_and_undo_restores_all() {
        let mut session = EditorSession::empty_animation_graph();
        let motion_slot = session
            .add_motion_slot("Idle")
            .expect("Motion Slot should be added");
        let first = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("first State should be added");
        let second = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("second State should be added");
        for (node, name) in [(first.clone(), "Idle A"), (second.clone(), "Idle B")] {
            session
                .set_node_name(node.clone(), name)
                .expect("State should be renamed");
            session
                .set_animation_state_motion_slot(node, Some(motion_slot.clone()))
                .expect("State should select graph slot");
        }

        let usages = session.states_using_motion_slot(&motion_slot);
        assert_eq!(usages.len(), 2);
        session
            .delete_motion_slot(motion_slot.clone())
            .expect("confirmed slot deletion should succeed");

        assert!(session
            .motion_slots()
            .expect("slot list should remain valid")
            .is_empty());
        for node in [&first, &second] {
            let Value::Object(properties) = &session.graph().nodes[node].properties else {
                panic!("State properties must remain an object");
            };
            assert!(!properties.contains_key("motion_slot"));
        }
        assert!(session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "anim.state_no_motion"));

        assert!(session.undo(), "slot deletion should be one undo step");
        assert_eq!(
            session
                .motion_slots()
                .expect("restored slot list should be valid")
                .len(),
            1
        );
        assert_eq!(session.states_using_motion_slot(&motion_slot).len(), 2);
    }

    #[test]
    fn animation_state_playback_mode_defaults_to_loop_and_is_undoable() {
        let mut session = EditorSession::empty_animation_graph();
        let state = session
            .add_animation_node(AnimationNodeInsertKind::State, None)
            .expect("State creation must succeed");

        let playback_property = |session: &EditorSession| {
            let Value::Object(properties) = &session
                .graph()
                .nodes
                .get(&state)
                .expect("State must exist")
                .properties
            else {
                panic!("Animation State properties must be an object");
            };
            properties
                .get(engine_authoring::ANIMATION_STATE_PLAYBACK_MODE_PROPERTY)
                .cloned()
        };

        assert_eq!(
            playback_property(&session),
            Some(Value::String("loop".to_owned()))
        );
        session
            .set_animation_state_playback_mode(
                state.clone(),
                engine_authoring::AnimationStatePlaybackMode::Once,
            )
            .expect("playback mode edit must succeed");
        assert_eq!(
            playback_property(&session),
            Some(Value::String("once".to_owned()))
        );

        assert!(session.undo(), "playback mode edit must be one undo step");
        assert_eq!(
            playback_property(&session),
            Some(Value::String("loop".to_owned()))
        );
    }
}
