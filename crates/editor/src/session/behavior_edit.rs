//! Behavior Tree node creation and ordered child edges.
//!
//! Child edge order is semantic in a Behavior Tree, so new children are
//! appended with an explicit order derived from the parent's current edges
//! rather than relying on map iteration order.

use super::errors::EditorSessionError;
use super::{EditorGraphDomain, EditorSession};
use engine_authoring::{
    Diagnostic, EdgeId, GraphCommand, Node, NodeId, NodeLayout, NodeTypeId, Vec2,
};

/// Behavior Tree node variants available in the Phase 8-A prototype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorNodeInsertKind {
    /// Root node.
    Root,
    /// Sequence composite node.
    Sequence,
    /// Selector composite node.
    Selector,
    /// Condition leaf node.
    Condition,
    /// Action leaf node.
    Action,
    /// Decorator node.
    Decorator,
}

impl EditorSession {
    /// Adds a Behavior Tree node through `GraphCommand`.
    ///
    /// Semantic commit is performed first. Presentation placement and selection
    /// are applied afterwards; their failures produce warnings rather than
    /// errors so that the semantic node is never silently discarded.
    ///
    /// Pushes one undo checkpoint before any mutation. The checkpoint covers
    /// the entire compound operation (semantic + presentation).
    pub fn add_behavior_node(
        &mut self,
        kind: BehaviorNodeInsertKind,
        behavior: impl Into<String>,
        position: Option<Vec2>,
    ) -> Result<NodeId, EditorSessionError> {
        let node_id = NodeId::generate();
        let node = self.make_behavior_node(node_id.clone(), kind, behavior.into())?;
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddNode { node })?;

        if let Some(position) = position {
            // Use set_node_layout directly so move_node does not push a
            // second checkpoint for this compound operation.
            let layout = NodeLayout::new(position);
            if let Err(error) = self.set_node_layout(node_id.clone(), layout) {
                self.diagnostics.push(Diagnostic::warning(
                    "editor.presentation_after_semantic_failed",
                    format!("semantic node was added but presentation placement failed: {error}"),
                ));
            }
        }
        if let Err(error) = self.select_node(Some(node_id.clone())) {
            self.diagnostics.push(Diagnostic::warning(
                "editor.presentation_after_semantic_failed",
                format!("semantic node was added but selection failed: {error}"),
            ));
        }
        Ok(node_id)
    }

    /// Adds a Behavior Tree node selected from the shared schema catalog.
    ///
    /// Node construction and defaults stay in [`engine_authoring::BehaviorTreeAuthoringService`];
    /// the Editor only commits the resulting shared graph command and presentation state.
    pub fn add_behavior_schema_node(
        &mut self,
        node_type: NodeTypeId,
        behavior: impl Into<String>,
        position: Option<Vec2>,
    ) -> Result<NodeId, EditorSessionError> {
        let node_id = NodeId::generate();
        let EditorGraphDomain::BehaviorTree(service) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "add Behavior Tree schema node",
            });
        };
        let node = service.create_node_with_defaults(&node_type, node_id.clone(), behavior);
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddNode { node })?;

        if let Some(position) = position {
            let layout = NodeLayout::new(position);
            if let Err(error) = self.set_node_layout(node_id.clone(), layout) {
                self.diagnostics.push(Diagnostic::warning(
                    "editor.presentation_after_semantic_failed",
                    format!("semantic node was added but presentation placement failed: {error}"),
                ));
            }
        }
        if let Err(error) = self.select_node(Some(node_id.clone())) {
            self.diagnostics.push(Diagnostic::warning(
                "editor.presentation_after_semantic_failed",
                format!("semantic node was added but selection failed: {error}"),
            ));
        }
        Ok(node_id)
    }

    /// Connects two Behavior Tree nodes through `GraphCommand`.
    ///
    /// Pushes one undo checkpoint before mutating state.
    pub fn connect_child(
        &mut self,
        parent: NodeId,
        child: NodeId,
    ) -> Result<EdgeId, EditorSessionError> {
        let EditorGraphDomain::BehaviorTree(service) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "connect Behavior Tree child",
            });
        };
        let order = self.next_child_order(&parent);
        let edge_id = EdgeId::generate();
        let edge = service.domain().child_edge(edge_id.clone(), parent, child, order);
        self.push_undo_checkpoint();
        self.apply_graph_command(GraphCommand::AddEdge { edge })?;
        Ok(edge_id)
    }

    fn make_behavior_node(
        &self,
        id: NodeId,
        kind: BehaviorNodeInsertKind,
        behavior: String,
    ) -> Result<Node, EditorSessionError> {
        let EditorGraphDomain::BehaviorTree(service) = &self.domain else {
            return Err(EditorSessionError::WrongGraphDomain {
                operation: "add Behavior Tree node",
            });
        };
        let domain = service.domain();
        Ok(match kind {
            BehaviorNodeInsertKind::Root => domain.root_node(id),
            BehaviorNodeInsertKind::Sequence => domain.sequence_node(id),
            BehaviorNodeInsertKind::Selector => domain.selector_node(id),
            BehaviorNodeInsertKind::Condition => domain.condition_node(id, behavior),
            BehaviorNodeInsertKind::Action => domain.action_node(id, behavior),
            BehaviorNodeInsertKind::Decorator => domain.decorator_node(id, behavior),
        })
    }

    fn next_child_order(&self, parent: &NodeId) -> u32 {
        self.graph
            .edges
            .values()
            .filter(|edge| &edge.from.node == parent)
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_node_commits_semantic_before_presentation() {
        let mut session = EditorSession::empty_behavior_tree();
        let node = session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "new_action",
                Some(Vec2::new(10.0, 20.0)),
            )
            .expect("add node should apply structurally");

        assert!(session.graph().nodes.contains_key(&node));
        assert!(session
            .graph_view()
            .expect("view exists")
            .nodes
            .contains_key(&node));
    }

    #[test]
    fn connect_child_uses_graph_command_boundary() {
        let mut session = EditorSession::empty_behavior_tree();
        let parent = session
            .add_behavior_node(BehaviorNodeInsertKind::Root, "", Some(Vec2::new(0.0, 0.0)))
            .expect("root should be added");
        let child = session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "child",
                Some(Vec2::new(200.0, 0.0)),
            )
            .expect("child should be added");

        let edge = session
            .connect_child(parent, child)
            .expect("edge should be added");

        assert!(session.graph().edges.contains_key(&edge));
    }

    #[test]
    fn add_behavior_node_with_position_is_single_undo_entry() {
        let mut session = EditorSession::empty_behavior_tree();
        session
            .add_behavior_node(
                BehaviorNodeInsertKind::Action,
                "a",
                Some(Vec2::new(100.0, 50.0)),
            )
            .expect("add should succeed");

        session.undo();

        assert_eq!(
            session.graph().nodes.len(),
            0,
            "one undo must revert both semantic and presentation state"
        );
        assert!(
            !session.can_undo(),
            "undo stack must be empty after reverting the only operation"
        );
    }

    #[test]
    fn schema_node_uses_shared_stateful_defaults() {
        let service = engine_authoring::BehaviorTreeAuthoringService::new();
        let wait_type = service.domain().wait_type().clone();
        let mut session = EditorSession::empty_behavior_tree();

        let node = session
            .add_behavior_schema_node(wait_type.clone(), "ignored", Some(Vec2::new(10.0, 20.0)))
            .expect("shared Wait schema should be insertable");

        let inserted = &session.graph().nodes[&node];
        assert_eq!(inserted.node_type, wait_type);
        assert_eq!(
            inserted.properties,
            engine_authoring::Value::Object(std::collections::BTreeMap::from([(
                "duration_seconds".into(),
                engine_authoring::Value::F64(1.0),
            )]))
        );
    }
}
