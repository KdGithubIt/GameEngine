//! Domain-neutral semantic graph and presentation graph view editing.
//!
//! Every semantic edit commits through [`GraphTransaction`] and every
//! presentation edit through [`GraphViewTransaction`], so the editor never
//! mutates either document directly. Semantic commits run first and repair the
//! graph view afterwards (ADR 0016), which keeps a deletion from stranding a
//! view that still names the removed node.

use super::{AnimationNodeInsertKind, BehaviorNodeInsertKind, EditorGraphDomain, EditorSession};
use super::errors::EditorSessionError;
use crate::geometry::{fallback_position_for_index, fallback_position_for_node};
use engine_authoring::{
    BehaviorTreeAuthoringService, Diagnostic, EdgeId, Graph, GraphCommand, GraphTransaction,
    GraphView, GraphViewCommand, GraphViewTransaction, NodeId, NodeLayout, Selection, Value, Vec2,
};

/// Domain-tagged node kind produced by the shared graph canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeInsertKind {
    /// A Behavior Tree node kind.
    Behavior(BehaviorNodeInsertKind),
    /// An Animation Graph node kind.
    Animation(AnimationNodeInsertKind),
}

impl GraphNodeInsertKind {
    /// Returns the concise human-facing node palette label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Behavior(BehaviorNodeInsertKind::Root) => "Root",
            Self::Behavior(BehaviorNodeInsertKind::Sequence) => "Sequence",
            Self::Behavior(BehaviorNodeInsertKind::Selector) => "Selector",
            Self::Behavior(BehaviorNodeInsertKind::Condition) => "Condition",
            Self::Behavior(BehaviorNodeInsertKind::Action) => "Action",
            Self::Behavior(BehaviorNodeInsertKind::Decorator) => "Decorator",
            Self::Animation(AnimationNodeInsertKind::Entry) => "Entry",
            Self::Animation(AnimationNodeInsertKind::State) => "State",
        }
    }
}

impl EditorSession {
    /// Returns the node kinds that the current graph domain can create.
    ///
    /// Animation Graphs offer Entry only when one is missing, preserving the
    /// domain rule that exactly one Entry node owns the initial-state edge.
    pub fn available_graph_node_kinds(&self) -> Vec<GraphNodeInsertKind> {
        match &self.domain {
            EditorGraphDomain::BehaviorTree(_) => vec![
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Root),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Sequence),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Selector),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Condition),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Action),
                GraphNodeInsertKind::Behavior(BehaviorNodeInsertKind::Decorator),
            ],
            EditorGraphDomain::Animation(domain) => {
                let has_entry = self
                    .graph
                    .nodes
                    .values()
                    .any(|node| node.node_type == *domain.entry_type());
                let mut kinds = Vec::with_capacity(2);
                if !has_entry {
                    kinds.push(GraphNodeInsertKind::Animation(
                        AnimationNodeInsertKind::Entry,
                    ));
                }
                kinds.push(GraphNodeInsertKind::Animation(
                    AnimationNodeInsertKind::State,
                ));
                kinds
            }
        }
    }

    /// Applies one semantic graph command and records current domain diagnostics.
    ///
    /// This method commits structurally valid semantic graph edits even when
    /// the resulting graph is temporarily invalid for its selected domain.
    /// This is required for interactive editing where a newly-created node may
    /// be configured or connected later.
    ///
    /// This method does not push an undo checkpoint. Callers that expose
    /// user-visible operations are responsible for checkpointing before calling
    /// this method.
    pub fn apply_graph_command(&mut self, command: GraphCommand) -> Result<(), EditorSessionError> {
        self.apply_graph_commands(std::iter::once(command))
    }

    /// Applies semantic graph commands as one private graph transaction.
    ///
    /// Callers use this for compound semantic edits such as replacing an edge
    /// while preserving its stable ID. Undo checkpointing remains the caller's
    /// responsibility so one user gesture produces one history entry.
    pub(super) fn apply_graph_commands(
        &mut self,
        commands: impl IntoIterator<Item = GraphCommand>,
    ) -> Result<(), EditorSessionError> {
        let mut transaction = GraphTransaction::begin(&self.graph);
        for command in commands {
            transaction.apply(command);
        }
        let commit = transaction
            .commit_private(self.domain.schema_registry())
            .map_err(|source| EditorSessionError::GraphTransactionValidation { source })?;
        self.graph = commit.graph;
        self.diagnostics = commit.diagnostics;
        self.diagnostics
            .extend(self.domain.validate_domain(&self.graph));
        self.prune_graph_view();
        self.mark_dirty();
        Ok(())
    }

    /// Applies one presentation graph view command.
    ///
    /// A missing graph view is created before applying the command. The command
    /// is still validated through `GraphViewTransaction`.
    ///
    /// This method does not push an undo checkpoint.
    pub fn apply_graph_view_command(
        &mut self,
        command: GraphViewCommand,
    ) -> Result<(), EditorSessionError> {
        self.apply_graph_view_commands(std::iter::once(command))
    }

    /// Applies presentation graph view commands as one transaction.
    ///
    /// `GraphViewTransaction::commit` validates the whole presentation
    /// document rather than only the commands it received, so repairs that
    /// depend on each other must share a transaction. Removing a stale node
    /// layout while the selection still names that node, for example, cannot
    /// commit on its own.
    ///
    /// This method does not push an undo checkpoint.
    fn apply_graph_view_commands(
        &mut self,
        commands: impl IntoIterator<Item = GraphViewCommand>,
    ) -> Result<(), EditorSessionError> {
        self.ensure_graph_view();
        let view = self
            .graph_view
            .as_mut()
            .expect("graph view must exist after ensure_graph_view");
        let mut transaction = GraphViewTransaction::begin(view);
        for command in commands {
            transaction.apply(command);
        }
        transaction
            .commit(view, &self.graph)
            .map_err(|source| EditorSessionError::GraphViewTransaction { source })?;
        Ok(())
    }

    /// Drops presentation state whose semantic counterpart no longer exists.
    ///
    /// Presentation validation rejects a graph view that names a missing node,
    /// edge, or group, and it inspects the whole document on every commit. A
    /// view left holding a deleted identifier therefore blocks every later
    /// graph view command, including plain selection, until the document is
    /// reopened. Running this repair wherever the semantic graph is replaced
    /// keeps the view committable instead of letting one deletion strand it.
    pub(super) fn prune_graph_view(&mut self) {
        let commands = self.graph_view_prune_commands();
        if commands.is_empty() {
            return;
        }
        if let Err(error) = self.apply_graph_view_commands(commands) {
            self.diagnostics.push(Diagnostic::warning(
                "editor.graph_view_prune_failed",
                format!("stale graph view presentation state could not be dropped: {error}"),
            ));
        }
    }

    /// Builds the commands that remove every graph view reference the semantic
    /// graph no longer defines.
    fn graph_view_prune_commands(&self) -> Vec<GraphViewCommand> {
        let Some(view) = self.graph_view.as_ref() else {
            return Vec::new();
        };
        let mut commands = Vec::new();
        for node in view.nodes.keys() {
            if !self.graph.nodes.contains_key(node) {
                commands.push(GraphViewCommand::RemoveNodeLayout { node: node.clone() });
            }
        }
        for group in view.groups.keys() {
            if !self.graph.groups.contains_key(group) {
                commands.push(GraphViewCommand::RemoveGroupLayout {
                    group: group.clone(),
                });
            }
        }
        let mut selection = view.selection.clone();
        selection
            .nodes
            .retain(|node| self.graph.nodes.contains_key(node));
        selection
            .edges
            .retain(|edge| self.graph.edges.contains_key(edge));
        selection
            .groups
            .retain(|group| self.graph.groups.contains_key(group));
        if selection != view.selection {
            commands.push(GraphViewCommand::SetSelection { selection });
        }
        commands
    }

    /// Selects one semantic node through `GraphViewCommand`.
    ///
    /// Selection is not undoable; it is a transient navigation state.
    pub fn select_node(&mut self, node: Option<NodeId>) -> Result<(), EditorSessionError> {
        let mut selection = Selection::new();
        if let Some(node) = node {
            selection.nodes.insert(node);
        }
        self.apply_graph_view_command(GraphViewCommand::SetSelection { selection })
    }

    /// Selects one semantic edge through [`GraphViewCommand`].
    ///
    /// Selection is presentation state and therefore does not enter semantic
    /// graph undo history.
    pub fn select_edge(&mut self, edge: Option<EdgeId>) -> Result<(), EditorSessionError> {
        let mut selection = Selection::new();
        if let Some(edge) = edge {
            selection.edges.insert(edge);
        }
        self.apply_graph_view_command(GraphViewCommand::SetSelection { selection })
    }

    /// Updates one node layout through `GraphViewCommand`.
    ///
    /// This method does not push an undo checkpoint.
    pub fn set_node_layout(
        &mut self,
        node: NodeId,
        layout: NodeLayout,
    ) -> Result<(), EditorSessionError> {
        self.apply_graph_view_command(GraphViewCommand::SetNodeLayout { node, layout })?;
        self.mark_dirty();
        Ok(())
    }

    /// Moves one node through `GraphViewCommand` while preserving collapsed,
    /// pinned, and annotation state.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn move_node(&mut self, node: NodeId, position: Vec2) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        let mut layout = self
            .graph_view
            .as_ref()
            .and_then(|view| view.nodes.get(&node))
            .cloned()
            .unwrap_or_else(|| NodeLayout::new(position));
        layout.position = position;
        self.set_node_layout(node, layout)
    }

    /// Sets one node's pin state through `GraphViewCommand`.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn set_node_pinned(
        &mut self,
        node: NodeId,
        pinned: bool,
    ) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        let mut layout = self
            .graph_view
            .as_ref()
            .and_then(|view| view.nodes.get(&node))
            .cloned()
            .unwrap_or_else(|| NodeLayout::new(fallback_position_for_node(&self.graph, &node)));
        layout.pinned = pinned;
        self.set_node_layout(node, layout)
    }

    /// Replaces a node property value through `GraphCommand`.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn set_node_property(
        &mut self,
        node: NodeId,
        value: Value,
    ) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetNodeProperty { node, value })
    }

    /// Replaces one node's optional human-readable name through the semantic
    /// graph command boundary.
    pub fn set_node_name(
        &mut self,
        node: NodeId,
        name: impl Into<String>,
    ) -> Result<(), EditorSessionError> {
        let name = name.into().trim().to_owned();
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::SetNodeName {
            node,
            name: (!name.is_empty()).then_some(name),
        })
    }

    /// Adds a node offered by the active graph domain.
    ///
    /// `behavior` is used only for Behavior Tree action and condition nodes;
    /// Animation Graph nodes ignore it because their clip is edited through
    /// the typed state Inspector after creation.
    pub fn add_graph_node(
        &mut self,
        kind: GraphNodeInsertKind,
        behavior: impl Into<String>,
        position: Option<Vec2>,
    ) -> Result<NodeId, EditorSessionError> {
        match kind {
            GraphNodeInsertKind::Behavior(kind) => self.add_behavior_node(kind, behavior, position),
            GraphNodeInsertKind::Animation(kind) => self.add_animation_node(kind, position),
        }
    }

    /// Deletes a node through `GraphCommand`.
    ///
    /// The node's layout and any selection entry naming it, or naming an edge
    /// the deletion cascaded away, are dropped by the presentation repair that
    /// every semantic graph edit runs.
    ///
    /// Pushes one undo checkpoint before any mutation.
    pub fn delete_node(&mut self, node: NodeId) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::DeleteNode { node })
    }

    /// Deletes one semantic edge and clears presentation selection.
    ///
    /// This operation is used for both Behavior Tree child edges and Animation
    /// Graph transitions and records one undo checkpoint.
    pub fn delete_edge(&mut self, edge: EdgeId) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::DeleteEdge { edge })
    }

    /// Connects nodes using the active graph domain's edge semantics.
    pub fn connect_nodes(
        &mut self,
        source: NodeId,
        target: NodeId,
    ) -> Result<EdgeId, EditorSessionError> {
        if self.is_animation_graph() {
            self.connect_animation_transition(source, target)
        } else {
            self.connect_child(source, target)
        }
    }

    /// Applies deterministic incremental layout through `GraphViewCommand`.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn apply_incremental_layout(&mut self) -> Result<(), EditorSessionError> {
        self.push_undo_checkpoint();
        let candidate_view = if self.is_animation_graph() {
            fallback_graph_view(&self.graph)
        } else {
            let candidate = BehaviorTreeAuthoringService::new().layout(&self.graph);
            if let Some(candidate_view) = candidate.graph_view {
                self.extend_diagnostics(candidate.diagnostics);
                candidate_view
            } else {
                self.extend_diagnostics(candidate.diagnostics);
                self.push_diagnostic(Diagnostic::warning(
                    "editor.layout_fallback_used",
                    "domain layout did not produce a graph view; using deterministic editor fallback",
                ));
                fallback_graph_view(&self.graph)
            }
        };
        let current = self.graph_view.as_ref();
        let commands = incremental_layout_commands(&self.graph, current, &candidate_view);
        let changed = !commands.is_empty();
        self.apply_graph_view_commands(commands)?;
        if changed {
            self.mark_dirty();
        }
        Ok(())
    }

    fn ensure_graph_view(&mut self) {
        if self.graph_view.is_none()
            || self
                .graph_view
                .as_ref()
                .is_some_and(|v| v.graph != self.graph.id)
        {
            self.graph_view = Some(GraphView::new(self.graph.id.clone()));
        }
    }
}

fn fallback_graph_view(graph: &Graph) -> GraphView {
    let mut view = GraphView::new(graph.id.clone());
    for (index, node_id) in graph.nodes.keys().enumerate() {
        view.nodes.insert(
            node_id.clone(),
            NodeLayout::new(fallback_position_for_index(index)),
        );
    }
    view
}

fn incremental_layout_commands(
    graph: &Graph,
    current: Option<&GraphView>,
    candidate: &GraphView,
) -> Vec<GraphViewCommand> {
    let mut commands = Vec::new();
    if let Some(current) = current {
        for node in current.nodes.keys() {
            if !graph.nodes.contains_key(node) {
                commands.push(GraphViewCommand::RemoveNodeLayout { node: node.clone() });
            }
        }
    }

    for (index, node_id) in graph.nodes.keys().enumerate() {
        let candidate_layout = candidate
            .nodes
            .get(node_id)
            .cloned()
            .unwrap_or_else(|| NodeLayout::new(fallback_position_for_index(index)));
        let mut layout = candidate_layout;
        if let Some(existing) = current.and_then(|view| view.nodes.get(node_id)) {
            if existing.pinned {
                layout.position = existing.position;
                layout.pinned = true;
            }
            layout.collapsed = existing.collapsed;
            layout.annotations = existing.annotations.clone();
        }
        commands.push(GraphViewCommand::SetNodeLayout {
            node: node_id.clone(),
            layout,
        });
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn deleting_a_selected_state_leaves_the_graph_view_committable() {
        let mut session = EditorSession::empty_animation_graph();
        let entry = session
            .graph()
            .nodes
            .values()
            .find(|node| node.node_type.as_str() == "anim.entry")
            .expect("new graph must have Entry")
            .id
            .clone();
        let kept = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(220.0, 0.0)))
            .expect("kept State should be added");
        let removed = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(440.0, 0.0)))
            .expect("removed State should be added");
        session
            .connect_animation_transition(entry, kept.clone())
            .expect("Entry should connect to a State");
        session
            .connect_animation_transition(kept.clone(), removed.clone())
            .expect("State should connect to State");
        session
            .select_node(Some(removed.clone()))
            .expect("selecting a live State must succeed");

        session
            .delete_node(removed.clone())
            .expect("deleting a State must succeed");

        let view = session.graph_view().expect("graph view must exist");
        assert!(
            !view.nodes.contains_key(&removed),
            "deleted State kept its layout"
        );
        assert!(
            view.selection.nodes.is_empty(),
            "deleted State stayed selected"
        );
        assert!(
            view.selection.edges.is_empty(),
            "cascaded transition stayed selected"
        );
        session
            .select_node(Some(kept))
            .expect("selection must still commit after a deletion");
    }

    /// A transition selected when its endpoint is deleted must not block the
    /// view either; `DeleteNode` cascades incident edges away.

    #[test]
    fn deleting_a_state_clears_a_selected_cascaded_transition() {
        let mut session = EditorSession::empty_animation_graph();
        let entry = session
            .graph()
            .nodes
            .values()
            .find(|node| node.node_type.as_str() == "anim.entry")
            .expect("new graph must have Entry")
            .id
            .clone();
        let kept = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(220.0, 0.0)))
            .expect("kept State should be added");
        let removed = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(440.0, 0.0)))
            .expect("removed State should be added");
        session
            .connect_animation_transition(entry, kept.clone())
            .expect("Entry should connect to a State");
        let transition = session
            .connect_animation_transition(kept.clone(), removed.clone())
            .expect("State should connect to State");
        session
            .select_edge(Some(transition))
            .expect("selecting a live transition must succeed");

        session
            .delete_node(removed)
            .expect("deleting a State must succeed");

        assert!(session
            .graph_view()
            .expect("graph view must exist")
            .selection
            .edges
            .is_empty());
        session
            .select_node(Some(kept))
            .expect("selection must still commit after a cascaded deletion");
    }

    /// A view file written before the repair existed still names deleted
    /// nodes. Opening it must recover instead of leaving the document
    /// permanently unselectable.

    #[test]
    fn selection_uses_graph_view_command_boundary() {
        let mut session =
            EditorSession::behavior_tree_example().expect("example session should be valid");
        let node = session
            .graph()
            .nodes
            .keys()
            .next()
            .expect("example has nodes")
            .clone();

        session
            .select_node(Some(node.clone()))
            .expect("selection should apply");

        assert_eq!(session.selected_node(), Some(&node));
        assert!(session
            .graph_view()
            .expect("selection creates or updates view")
            .selection
            .nodes
            .contains(&node));
    }

    #[test]
    fn semantic_property_edit_uses_graph_command_boundary() {
        let mut session =
            EditorSession::behavior_tree_example().expect("example session should be valid");
        let node = session
            .graph()
            .nodes
            .keys()
            .next()
            .expect("example has nodes")
            .clone();
        let value = Value::Object(BTreeMap::from([(
            "behavior".into(),
            Value::String("edited".into()),
        )]));

        session
            .set_node_property(node.clone(), value.clone())
            .expect("property edit should apply structurally");

        assert_eq!(session.graph().nodes[&node].properties, value);
    }

    #[test]
    fn dragging_updates_layout_but_preserves_pin_state() {
        let mut session =
            EditorSession::behavior_tree_example().expect("example session should be valid");
        let node = session
            .graph()
            .nodes
            .keys()
            .next()
            .expect("example has nodes")
            .clone();
        session
            .set_node_pinned(node.clone(), true)
            .expect("pin should apply");
        session
            .move_node(node.clone(), Vec2::new(42.0, 24.0))
            .expect("move should apply");

        let layout = &session.graph_view().expect("view exists").nodes[&node];
        assert!(layout.pinned);
        assert_eq!(layout.position, Vec2::new(42.0, 24.0));
    }

    #[test]
    fn pin_without_existing_layout_uses_node_fallback_position() {
        let mut session = EditorSession::empty_behavior_tree();
        let first = session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "first", None)
            .expect("first node should be added");
        let second = session
            .add_behavior_node(BehaviorNodeInsertKind::Action, "second", None)
            .expect("second node should be added");

        session
            .set_node_pinned(second.clone(), true)
            .expect("pin should apply");

        let layout = &session.graph_view().expect("view exists").nodes[&second];
        assert!(layout.pinned);
        assert_eq!(
            layout.position,
            fallback_position_for_node(session.graph(), &second)
        );
        assert!(!session
            .graph_view()
            .expect("view exists")
            .nodes
            .contains_key(&first));
    }

    #[test]
    fn incremental_layout_uses_fallback_for_incomplete_graph() {
        let mut session = EditorSession::empty_behavior_tree();
        let node = session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "orphan_action",
                Some(Vec2::new(500.0, 500.0)),
            )
            .expect("orphan action should be structurally valid");

        session
            .apply_incremental_layout()
            .expect("fallback layout should apply through graph view commands");

        assert!(session
            .graph_view()
            .expect("view exists")
            .nodes
            .contains_key(&node));
        assert!(session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.layout_fallback_used"));
    }

    #[test]
    fn incremental_layout_preserves_existing_diagnostics() {
        let mut session = EditorSession::empty_behavior_tree();
        session.push_diagnostic(Diagnostic::warning(
            "editor.existing_warning",
            "existing warning",
        ));

        session
            .apply_incremental_layout()
            .expect("empty graph layout should apply");

        assert!(session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.existing_warning"));
    }

    // ── Undo/Redo tests ────────────────────────────────────────────────────
}
