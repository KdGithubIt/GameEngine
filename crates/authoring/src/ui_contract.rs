//! Typed UI binding, event, and gamepad-focus authoring contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ui::{UiDocument, UiNode};

/// Runtime value kind expected by one UI binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiBindingKind {
    /// String-valued binding used by text and button labels.
    Text,
    /// Numeric binding used by progress and formatted values.
    Number,
    /// Boolean binding used by visibility or state indicators.
    Flag,
}

/// One typed binding offered by the UI Builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiBindingDeclaration {
    /// Stable binding name published by project gameplay.
    pub name: String,
    /// Runtime value kind accepted by the binding.
    pub kind: UiBindingKind,
    /// Human-readable purpose shown by authoring clients.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One project event offered by the UI Builder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiEventDeclaration {
    /// Stable event name relayed to project gameplay.
    pub name: String,
    /// Human-readable purpose shown by authoring clients.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// Direction used by gamepad and keyboard focus navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiFocusDirection {
    /// Move focus upward.
    Up,
    /// Move focus downward.
    Down,
    /// Move focus left.
    Left,
    /// Move focus right.
    Right,
}

/// Explicit directional focus target for one UI node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiFocusLink {
    /// Focusable source node ID.
    pub from: String,
    /// Direction that activates the link.
    pub direction: UiFocusDirection,
    /// Focusable destination node ID.
    pub to: String,
}

/// Project-owned UI authoring catalog consumed by editor candidate pickers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiAuthoringContract {
    /// Typed binding candidate list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<UiBindingDeclaration>,
    /// UI event candidate list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<UiEventDeclaration>,
    /// Explicit directional focus graph.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub focus_links: Vec<UiFocusLink>,
    /// Node that receives focus when the document becomes visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_focus: Option<String>,
}

impl UiAuthoringContract {
    /// Returns bindings of `kind` in deterministic name order.
    pub fn binding_candidates(
        &self,
        kind: UiBindingKind,
    ) -> impl Iterator<Item = &UiBindingDeclaration> {
        let mut candidates = self
            .bindings
            .iter()
            .filter(move |binding| binding.kind == kind)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.name.cmp(&right.name));
        candidates.into_iter()
    }

    /// Returns events in deterministic name order.
    pub fn event_candidates(&self) -> impl Iterator<Item = &UiEventDeclaration> {
        let mut candidates = self.events.iter().collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.name.cmp(&right.name));
        candidates.into_iter()
    }

    /// Resolves one authored directional focus move.
    pub fn focus_target(&self, from: &str, direction: UiFocusDirection) -> Option<&str> {
        self.focus_links
            .iter()
            .find(|link| link.from == from && link.direction == direction)
            .map(|link| link.to.as_str())
    }

    /// Validates names, duplicates, node references, and focus determinism.
    ///
    /// # Errors
    ///
    /// Returns every independent contract error so the Builder can present one
    /// actionable Problems list instead of stopping at the first field.
    pub fn validate(&self, document: &UiDocument) -> Result<(), Vec<UiContractError>> {
        let mut errors = Vec::new();
        validate_named_declarations(
            self.bindings.iter().map(|binding| binding.name.as_str()),
            "binding",
            &mut errors,
        );
        validate_named_declarations(
            self.events.iter().map(|event| event.name.as_str()),
            "event",
            &mut errors,
        );

        let mut node_ids = BTreeSet::new();
        collect_node_ids(&document.root, &mut node_ids);
        if let Some(initial) = self.initial_focus.as_deref() {
            if initial.trim().is_empty() {
                errors.push(UiContractError::BlankInitialFocus);
            } else if !node_ids.contains(initial) {
                errors.push(UiContractError::MissingFocusNode {
                    node: initial.to_owned(),
                });
            }
        }

        let mut focus_keys = BTreeMap::new();
        for link in &self.focus_links {
            if !node_ids.contains(link.from.as_str()) {
                errors.push(UiContractError::MissingFocusNode {
                    node: link.from.clone(),
                });
            }
            if !node_ids.contains(link.to.as_str()) {
                errors.push(UiContractError::MissingFocusNode {
                    node: link.to.clone(),
                });
            }
            let key = (link.from.clone(), link.direction);
            if let Some(previous) = focus_keys.insert(key, link.to.clone()) {
                errors.push(UiContractError::DuplicateFocusDirection {
                    node: link.from.clone(),
                    direction: link.direction,
                    first_target: previous,
                    second_target: link.to.clone(),
                });
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

fn validate_named_declarations<'a>(
    names: impl Iterator<Item = &'a str>,
    category: &'static str,
    errors: &mut Vec<UiContractError>,
) {
    let mut seen = BTreeSet::new();
    for name in names {
        if name.trim().is_empty() {
            errors.push(UiContractError::BlankDeclaration { category });
        } else if !seen.insert(name) {
            errors.push(UiContractError::DuplicateDeclaration {
                category,
                name: name.to_owned(),
            });
        }
    }
}

fn collect_node_ids<'a>(node: &'a UiNode, output: &mut BTreeSet<&'a str>) {
    output.insert(node.id.as_str());
    for child in &node.children {
        collect_node_ids(child, output);
    }
}

/// Validation failure for [`UiAuthoringContract`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiContractError {
    /// A binding or event declaration has a blank name.
    BlankDeclaration {
        /// Declaration category.
        category: &'static str,
    },
    /// A binding or event name is declared more than once.
    DuplicateDeclaration {
        /// Declaration category.
        category: &'static str,
        /// Duplicated name.
        name: String,
    },
    /// `initial_focus` was present but blank.
    BlankInitialFocus,
    /// A focus link or initial target names a missing UI node.
    MissingFocusNode {
        /// Missing node ID.
        node: String,
    },
    /// One source and direction have more than one destination.
    DuplicateFocusDirection {
        /// Source node ID.
        node: String,
        /// Conflicting direction.
        direction: UiFocusDirection,
        /// First destination.
        first_target: String,
        /// Second destination.
        second_target: String,
    },
}

impl fmt::Display for UiContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankDeclaration { category } => {
                write!(formatter, "UI {category} declaration name must not be blank")
            }
            Self::DuplicateDeclaration { category, name } => {
                write!(formatter, "UI {category} `{name}` is declared more than once")
            }
            Self::BlankInitialFocus => write!(formatter, "UI initial focus must not be blank"),
            Self::MissingFocusNode { node } => {
                write!(formatter, "UI focus references missing node `{node}`")
            }
            Self::DuplicateFocusDirection {
                node,
                direction,
                first_target,
                second_target,
            } => write!(
                formatter,
                "UI node `{node}` direction {direction:?} targets both `{first_target}` and `{second_target}`"
            ),
        }
    }
}

impl std::error::Error for UiContractError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{UiAnchor, UiLayout, UiNodeKind, UiString};

    fn document_with_buttons() -> UiDocument {
        let mut document = UiDocument::default();
        document.root.children = vec![
            UiNode {
                id: "play".to_owned(),
                kind: UiNodeKind::Button {
                    label: UiString::Literal("Play".to_owned()),
                    event: "menu.play".to_owned(),
                },
                children: Vec::new(),
            },
            UiNode {
                id: "quit".to_owned(),
                kind: UiNodeKind::Button {
                    label: UiString::Literal("Quit".to_owned()),
                    event: "menu.quit".to_owned(),
                },
                children: Vec::new(),
            },
        ];
        document
    }

    #[test]
    fn typed_candidates_are_filtered_and_sorted() {
        let contract = UiAuthoringContract {
            bindings: vec![
                UiBindingDeclaration {
                    name: "player.name".to_owned(),
                    kind: UiBindingKind::Text,
                    description: String::new(),
                },
                UiBindingDeclaration {
                    name: "player.health".to_owned(),
                    kind: UiBindingKind::Number,
                    description: String::new(),
                },
                UiBindingDeclaration {
                    name: "mission.name".to_owned(),
                    kind: UiBindingKind::Text,
                    description: String::new(),
                },
            ],
            ..UiAuthoringContract::default()
        };

        let names = contract
            .binding_candidates(UiBindingKind::Text)
            .map(|binding| binding.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["mission.name", "player.name"]);
    }

    #[test]
    fn focus_links_validate_against_document_nodes() {
        let contract = UiAuthoringContract {
            initial_focus: Some("play".to_owned()),
            focus_links: vec![UiFocusLink {
                from: "play".to_owned(),
                direction: UiFocusDirection::Down,
                to: "quit".to_owned(),
            }],
            ..UiAuthoringContract::default()
        };

        assert_eq!(
            contract.focus_target("play", UiFocusDirection::Down),
            Some("quit")
        );
        assert_eq!(contract.validate(&document_with_buttons()), Ok(()));
    }

    #[test]
    fn duplicate_direction_is_rejected() {
        let contract = UiAuthoringContract {
            focus_links: vec![
                UiFocusLink {
                    from: "play".to_owned(),
                    direction: UiFocusDirection::Down,
                    to: "quit".to_owned(),
                },
                UiFocusLink {
                    from: "play".to_owned(),
                    direction: UiFocusDirection::Down,
                    to: "root".to_owned(),
                },
            ],
            ..UiAuthoringContract::default()
        };

        let errors = contract
            .validate(&document_with_buttons())
            .expect_err("duplicate direction must fail validation");
        assert!(errors
            .iter()
            .any(|error| matches!(error, UiContractError::DuplicateFocusDirection { .. })));
    }

    #[test]
    fn default_document_helpers_remain_constructible() {
        let _ = UiNodeKind::Panel {
            anchor: UiAnchor::TopLeft,
            offset_x: 0.0,
            offset_y: 0.0,
            layout: UiLayout::Vertical,
            spacing: 0.0,
            padding: 0.0,
            background: None,
        };
    }
}
