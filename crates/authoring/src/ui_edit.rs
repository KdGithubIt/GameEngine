//! Command-backed editing operations for declarative UI documents.
//!
//! The visual editor, CLI adapters, and automation tools use the commands in
//! this module instead of mutating [`UiDocument`]
//! trees directly. A transaction owns an isolated working copy, applies one
//! or more structural commands, and validates the complete document before it
//! can be committed.

use crate::diagnostic::{Diagnostic, Severity};
use crate::ui::{UiDocument, UiElementConstraints, UiNode, UiScalePolicy};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A reversible semantic operation over one UI document tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiDocumentCommand {
    /// Replaces document-wide responsive layout settings.
    SetResponsiveSettings {
        /// Positive logical reference resolution.
        reference_resolution: [f32; 2],
        /// Viewport or physical scaling policy.
        scale_policy: UiScalePolicy,
        /// Left, top, right, and bottom logical padding.
        safe_area_padding: [f32; 4],
    },
    /// Adds, replaces, or removes responsive constraints for one node.
    SetNodeConstraints {
        /// Existing stable node ID.
        node: String,
        /// New constraints, or `None` to remove the entry.
        constraints: Option<UiElementConstraints>,
    },
    /// Inserts a new subtree below a container node.
    InsertNode {
        /// Identifier of the container that receives the new child.
        parent: String,
        /// Requested child index. Values beyond the end append the node.
        index: usize,
        /// Complete subtree to insert.
        node: UiNode,
    },
    /// Removes a node and its complete subtree.
    RemoveNode {
        /// Identifier of the node to remove.
        node: String,
    },
    /// Moves an existing subtree below another container.
    MoveNode {
        /// Identifier of the subtree root to move.
        node: String,
        /// Identifier of the new parent container.
        parent: String,
        /// Requested index below the new parent.
        index: usize,
    },
    /// Replaces a node's editable properties while preserving its identity.
    ReplaceNode {
        /// Identifier of the node being replaced.
        node: String,
        /// Replacement node. Its `id` must match `node`.
        replacement: UiNode,
    },
    /// Changes a node identifier without changing its subtree or position.
    RenameNode {
        /// Current document-unique identifier.
        node: String,
        /// Replacement document-unique identifier.
        new_id: String,
    },
}

/// One semantic change produced by a [`UiDocumentCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiDocumentChange {
    /// Document-wide responsive settings changed.
    ResponsiveSettingsChanged,
    /// Responsive constraints for a node changed.
    NodeConstraintsChanged {
        /// Stable node ID whose constraints changed.
        node: String,
    },
    /// A subtree was inserted below `parent`.
    Inserted {
        /// Identifier of the inserted subtree root.
        node: String,
        /// Identifier of the receiving parent.
        parent: String,
        /// Actual insertion index after clamping.
        index: usize,
    },
    /// A subtree was removed.
    Removed {
        /// Identifier of the removed subtree root.
        node: String,
    },
    /// A subtree was moved below another parent.
    Moved {
        /// Identifier of the moved subtree root.
        node: String,
        /// Identifier of the receiving parent.
        parent: String,
        /// Actual insertion index after clamping.
        index: usize,
    },
    /// A node's editable properties were replaced.
    Replaced {
        /// Identifier of the replaced node.
        node: String,
    },
    /// A node identifier changed.
    Renamed {
        /// Previous node identifier.
        old_id: String,
        /// New node identifier.
        new_id: String,
    },
}

/// Result of applying one command to a UI document transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct UiDocumentCommandResult {
    /// Semantic changes produced by the command.
    pub changes: Vec<UiDocumentChange>,
    /// Current whole-document validation diagnostics.
    pub diagnostics: Vec<Diagnostic>,
}

/// An isolated, command-backed edit of one [`UiDocument`].
pub struct UiDocumentTransaction {
    working: UiDocument,
}

impl UiDocumentTransaction {
    /// Starts a transaction from the current document state.
    pub fn begin(document: &UiDocument) -> Self {
        Self {
            working: document.clone(),
        }
    }

    /// Returns the transaction's current working document.
    pub fn document(&self) -> &UiDocument {
        &self.working
    }

    /// Applies one structural command and returns its semantic result.
    ///
    /// # Errors
    ///
    /// Returns [`UiDocumentEditError`] when an identifier is missing, a
    /// destination cannot contain children, identity would be duplicated, or
    /// the requested operation would create a tree cycle.
    pub fn apply(
        &mut self,
        command: UiDocumentCommand,
    ) -> Result<UiDocumentCommandResult, UiDocumentEditError> {
        let change = match command {
            UiDocumentCommand::SetResponsiveSettings {
                reference_resolution,
                scale_policy,
                safe_area_padding,
            } => {
                self.working.reference_resolution = reference_resolution;
                self.working.scale_policy = scale_policy;
                self.working.safe_area_padding = safe_area_padding;
                UiDocumentChange::ResponsiveSettingsChanged
            }
            UiDocumentCommand::SetNodeConstraints { node, constraints } => {
                if find_node(&self.working.root, &node).is_none() {
                    return Err(UiDocumentEditError::NodeNotFound(node));
                }
                if let Some(constraints) = constraints {
                    self.working.constraints.insert(node.clone(), constraints);
                } else {
                    self.working.constraints.remove(&node);
                }
                UiDocumentChange::NodeConstraintsChanged { node }
            }
            UiDocumentCommand::InsertNode {
                parent,
                index,
                node,
            } => {
                ensure_unique_subtree_ids(&self.working, &node)?;
                let inserted_id = node.id.clone();
                let parent_node = find_node_mut(&mut self.working.root, &parent)
                    .ok_or_else(|| UiDocumentEditError::NodeNotFound(parent.clone()))?;
                ensure_container(parent_node)?;
                let actual_index = index.min(parent_node.children.len());
                parent_node.children.insert(actual_index, node);
                UiDocumentChange::Inserted {
                    node: inserted_id,
                    parent,
                    index: actual_index,
                }
            }
            UiDocumentCommand::RemoveNode { node } => {
                if self.working.root.id == node {
                    return Err(UiDocumentEditError::RootMutation);
                }
                let removed = take_node(&mut self.working.root, &node)
                    .ok_or_else(|| UiDocumentEditError::NodeNotFound(node.clone()))?;
                let mut removed_ids = Vec::new();
                collect_node_ids(&removed, &mut removed_ids);
                for removed_id in removed_ids {
                    self.working.constraints.remove(&removed_id);
                }
                UiDocumentChange::Removed { node }
            }
            UiDocumentCommand::MoveNode {
                node,
                parent,
                index,
            } => {
                if self.working.root.id == node {
                    return Err(UiDocumentEditError::RootMutation);
                }
                let subtree = find_node(&self.working.root, &node)
                    .ok_or_else(|| UiDocumentEditError::NodeNotFound(node.clone()))?;
                if contains_node(subtree, &parent) {
                    return Err(UiDocumentEditError::TreeCycle { node, parent });
                }
                let destination = find_node(&self.working.root, &parent)
                    .ok_or_else(|| UiDocumentEditError::NodeNotFound(parent.clone()))?;
                ensure_container(destination)?;

                let moved = take_node(&mut self.working.root, &node)
                    .ok_or_else(|| UiDocumentEditError::NodeNotFound(node.clone()))?;
                let destination = find_node_mut(&mut self.working.root, &parent)
                    .ok_or_else(|| UiDocumentEditError::NodeNotFound(parent.clone()))?;
                let actual_index = index.min(destination.children.len());
                destination.children.insert(actual_index, moved);
                UiDocumentChange::Moved {
                    node,
                    parent,
                    index: actual_index,
                }
            }
            UiDocumentCommand::ReplaceNode { node, replacement } => {
                if replacement.id != node {
                    return Err(UiDocumentEditError::ReplacementIdMismatch {
                        expected: node,
                        found: replacement.id,
                    });
                }
                let target = find_node_mut(&mut self.working.root, &node)
                    .ok_or_else(|| UiDocumentEditError::NodeNotFound(node.clone()))?;
                *target = replacement;
                UiDocumentChange::Replaced { node }
            }
            UiDocumentCommand::RenameNode { node, new_id } => {
                if new_id.trim().is_empty() {
                    return Err(UiDocumentEditError::EmptyNodeId);
                }
                if node != new_id && find_node(&self.working.root, &new_id).is_some() {
                    return Err(UiDocumentEditError::DuplicateNodeId(new_id));
                }
                let target = find_node_mut(&mut self.working.root, &node)
                    .ok_or_else(|| UiDocumentEditError::NodeNotFound(node.clone()))?;
                target.id = new_id.clone();
                if let Some(constraints) = self.working.constraints.remove(&node) {
                    self.working.constraints.insert(new_id.clone(), constraints);
                }
                UiDocumentChange::Renamed {
                    old_id: node,
                    new_id,
                }
            }
        };

        Ok(UiDocumentCommandResult {
            changes: vec![change],
            diagnostics: self.working.validate(),
        })
    }

    /// Validates and returns the committed document.
    ///
    /// # Errors
    ///
    /// Returns [`UiDocumentCommitError`] when whole-document validation
    /// produces at least one error-level diagnostic.
    pub fn commit(self) -> Result<UiDocument, UiDocumentCommitError> {
        let diagnostics = self.working.validate();
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            return Err(UiDocumentCommitError { diagnostics });
        }
        Ok(self.working)
    }
}

/// Reports why a UI document command could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiDocumentEditError {
    /// No node exists with the requested identifier.
    NodeNotFound(String),
    /// A document root cannot be removed or moved.
    RootMutation,
    /// A destination node does not accept children.
    ParentIsNotContainer(String),
    /// An inserted or renamed node would duplicate an existing identifier.
    DuplicateNodeId(String),
    /// An inserted subtree contains the same identifier more than once.
    DuplicateSubtreeNodeId(String),
    /// Empty node identifiers are not accepted by editing commands.
    EmptyNodeId,
    /// Moving a node below its own descendant would create a cycle.
    TreeCycle {
        /// Node being moved.
        node: String,
        /// Requested descendant parent.
        parent: String,
    },
    /// A replacement attempted to change identity implicitly.
    ReplacementIdMismatch {
        /// Identifier targeted by the command.
        expected: String,
        /// Identifier carried by the replacement.
        found: String,
    },
}

impl fmt::Display for UiDocumentEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeNotFound(node) => write!(formatter, "UI node `{node}` was not found"),
            Self::RootMutation => {
                write!(formatter, "the UI document root cannot be moved or removed")
            }
            Self::ParentIsNotContainer(node) => {
                write!(formatter, "UI node `{node}` cannot contain child nodes")
            }
            Self::DuplicateNodeId(node) => {
                write!(formatter, "UI node id `{node}` is already in use")
            }
            Self::DuplicateSubtreeNodeId(node) => {
                write!(formatter, "inserted UI subtree repeats node id `{node}`")
            }
            Self::EmptyNodeId => write!(formatter, "UI node id must not be empty"),
            Self::TreeCycle { node, parent } => write!(
                formatter,
                "moving UI node `{node}` below `{parent}` would create a cycle"
            ),
            Self::ReplacementIdMismatch { expected, found } => write!(
                formatter,
                "replacement id `{found}` does not match targeted UI node `{expected}`"
            ),
        }
    }
}

impl std::error::Error for UiDocumentEditError {}

/// Validation failure returned when committing a UI document transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct UiDocumentCommitError {
    diagnostics: Vec<Diagnostic>,
}

impl UiDocumentCommitError {
    /// Returns the diagnostics that blocked the commit.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

impl fmt::Display for UiDocumentCommitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "UI document validation failed with {} diagnostic(s)",
            self.diagnostics.len()
        )
    }
}

impl std::error::Error for UiDocumentCommitError {}

/// Returns the node identified by `id`.
pub fn find_ui_node<'a>(document: &'a UiDocument, id: &str) -> Option<&'a UiNode> {
    find_node(&document.root, id)
}

fn find_node<'a>(node: &'a UiNode, id: &str) -> Option<&'a UiNode> {
    if node.id == id {
        return Some(node);
    }
    node.children.iter().find_map(|child| find_node(child, id))
}

fn find_node_mut<'a>(node: &'a mut UiNode, id: &str) -> Option<&'a mut UiNode> {
    if node.id == id {
        return Some(node);
    }
    node.children
        .iter_mut()
        .find_map(|child| find_node_mut(child, id))
}

fn contains_node(node: &UiNode, id: &str) -> bool {
    find_node(node, id).is_some()
}

fn collect_node_ids(node: &UiNode, ids: &mut Vec<String>) {
    ids.push(node.id.clone());
    for child in &node.children {
        collect_node_ids(child, ids);
    }
}

fn take_node(parent: &mut UiNode, id: &str) -> Option<UiNode> {
    if let Some(index) = parent.children.iter().position(|child| child.id == id) {
        return Some(parent.children.remove(index));
    }
    parent
        .children
        .iter_mut()
        .find_map(|child| take_node(child, id))
}

fn ensure_container(node: &UiNode) -> Result<(), UiDocumentEditError> {
    if node.kind.is_container() {
        Ok(())
    } else {
        Err(UiDocumentEditError::ParentIsNotContainer(node.id.clone()))
    }
}

fn ensure_unique_subtree_ids(
    document: &UiDocument,
    subtree: &UiNode,
) -> Result<(), UiDocumentEditError> {
    fn visit(
        document: &UiDocument,
        node: &UiNode,
        local: &mut std::collections::BTreeSet<String>,
    ) -> Result<(), UiDocumentEditError> {
        if node.id.trim().is_empty() {
            return Err(UiDocumentEditError::EmptyNodeId);
        }
        if !local.insert(node.id.clone()) {
            return Err(UiDocumentEditError::DuplicateSubtreeNodeId(node.id.clone()));
        }
        if find_node(&document.root, &node.id).is_some() {
            return Err(UiDocumentEditError::DuplicateNodeId(node.id.clone()));
        }
        for child in &node.children {
            visit(document, child, local)?;
        }
        Ok(())
    }

    visit(document, subtree, &mut std::collections::BTreeSet::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{UiNodeKind, UiString, UI_SCHEMA_VERSION};

    fn text_node(id: &str, text: &str) -> UiNode {
        UiNode {
            id: id.to_owned(),
            kind: UiNodeKind::Text {
                content: UiString::Literal(text.to_owned()),
                size: 16.0,
                color: [1.0; 4],
            },
            children: Vec::new(),
        }
    }

    #[test]
    fn transaction_inserts_and_commits_a_valid_node() {
        let mut transaction = UiDocumentTransaction::begin(&UiDocument::default());
        let result = transaction
            .apply(UiDocumentCommand::InsertNode {
                parent: "root".to_owned(),
                index: 0,
                node: text_node("title", "Title"),
            })
            .expect("valid insertion must apply");
        assert!(matches!(
            result.changes.as_slice(),
            [UiDocumentChange::Inserted { node, .. }] if node == "title"
        ));
        let document = transaction.commit().expect("valid document must commit");
        assert!(find_ui_node(&document, "title").is_some());
    }

    #[test]
    fn transaction_rejects_duplicate_inserted_identity() {
        let document = UiDocument::default();
        let mut transaction = UiDocumentTransaction::begin(&document);
        let error = transaction
            .apply(UiDocumentCommand::InsertNode {
                parent: "root".to_owned(),
                index: 0,
                node: text_node("root", "Duplicate"),
            })
            .expect_err("duplicate identity must fail");
        assert_eq!(
            error,
            UiDocumentEditError::DuplicateNodeId("root".to_owned())
        );
    }

    #[test]
    fn moving_a_parent_below_its_child_is_rejected() {
        let mut document = UiDocument::default();
        document.root.children.push(UiNode {
            id: "container".to_owned(),
            kind: UiNodeKind::Panel {
                anchor: Default::default(),
                offset_x: 0.0,
                offset_y: 0.0,
                layout: Default::default(),
                spacing: 4.0,
                padding: 8.0,
                background: None,
            },
            children: vec![UiNode {
                id: "child".to_owned(),
                kind: UiNodeKind::Panel {
                    anchor: Default::default(),
                    offset_x: 0.0,
                    offset_y: 0.0,
                    layout: Default::default(),
                    spacing: 4.0,
                    padding: 8.0,
                    background: None,
                },
                children: Vec::new(),
            }],
        });
        let mut transaction = UiDocumentTransaction::begin(&document);
        let error = transaction
            .apply(UiDocumentCommand::MoveNode {
                node: "container".to_owned(),
                parent: "child".to_owned(),
                index: 0,
            })
            .expect_err("tree cycle must fail");
        assert!(matches!(error, UiDocumentEditError::TreeCycle { .. }));
    }

    #[test]
    fn rename_preserves_document_schema_and_subtree() {
        let mut document = UiDocument::default();
        document.root.children.push(text_node("title", "Title"));
        let mut transaction = UiDocumentTransaction::begin(&document);
        transaction
            .apply(UiDocumentCommand::RenameNode {
                node: "title".to_owned(),
                new_id: "heading".to_owned(),
            })
            .expect("rename must apply");
        let document = transaction.commit().expect("rename must commit");
        assert_eq!(document.schema_version, UI_SCHEMA_VERSION);
        assert!(find_ui_node(&document, "heading").is_some());
        assert!(find_ui_node(&document, "title").is_none());
    }
}
