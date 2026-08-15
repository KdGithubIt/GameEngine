//! Persistence and deterministic runtime navigation for typed UI contracts.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::persist::{replace_file_contents, PersistError};
use crate::ui::UiDocument;
use crate::ui_contract::{UiAuthoringContract, UiContractError, UiFocusDirection};

/// Conventional suffix used beside a `.ui.json` document.
pub const UI_CONTRACT_FILE_SUFFIX: &str = ".ui-contract.json";

impl UiAuthoringContract {
    /// Parses a typed UI contract from JSON.
    ///
    /// Node-reference validation requires the associated [`UiDocument`] and is
    /// performed separately by [`UiAuthoringContract::validate`].
    ///
    /// # Errors
    ///
    /// Returns an error when the JSON does not match the persisted contract.
    pub fn from_json_str(json: &str) -> Result<Self, UiContractDocumentError> {
        serde_json::from_str(json).map_err(UiContractDocumentError::Json)
    }

    /// Serializes this contract as readable deterministic JSON.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization fails.
    pub fn to_json_string(&self) -> Result<String, UiContractDocumentError> {
        serde_json::to_string_pretty(self).map_err(UiContractDocumentError::Json)
    }

    /// Loads a typed UI contract from disk.
    ///
    /// Call [`UiAuthoringContract::validate`] with its UI document before using
    /// focus links at runtime.
    ///
    /// # Errors
    ///
    /// Returns an I/O or JSON error.
    pub fn load(path: &Path) -> Result<Self, UiContractDocumentError> {
        let json = fs::read_to_string(path).map_err(|source| UiContractDocumentError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_json_str(&json)
    }

    /// Saves the contract without replacing an unchanged file.
    ///
    /// # Errors
    ///
    /// Returns a serialization or persistence error.
    pub fn save(&self, path: &Path) -> Result<(), UiContractDocumentError> {
        let json = self.to_json_string()?;
        replace_file_contents(path, &json).map_err(UiContractDocumentError::Persist)
    }
}

/// Current focus state driven by one validated [`UiAuthoringContract`].
///
/// This object is independent of a GUI backend. A runtime host maps keyboard or
/// gamepad directions to [`move_focus`](Self::move_focus), then requests focus
/// for the returned stable UI node ID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiFocusNavigator {
    focused: Option<String>,
}

impl UiFocusNavigator {
    /// Validates the contract against `document` and activates its initial focus.
    ///
    /// # Errors
    ///
    /// Returns every contract validation error without creating partial state.
    pub fn activate(
        contract: &UiAuthoringContract,
        document: &UiDocument,
    ) -> Result<Self, Vec<UiContractError>> {
        contract.validate(document)?;
        Ok(Self {
            focused: contract.initial_focus.clone(),
        })
    }

    /// Returns the stable ID of the currently focused node.
    pub fn focused(&self) -> Option<&str> {
        self.focused.as_deref()
    }

    /// Restores the contract's authored initial focus.
    pub fn reset<'a>(&'a mut self, contract: &UiAuthoringContract) -> Option<&'a str> {
        self.focused.clone_from(&contract.initial_focus);
        self.focused()
    }

    /// Moves through one explicit authored directional link.
    ///
    /// Returns the new stable node ID. When the current node has no link in the
    /// requested direction, focus remains unchanged and `None` is returned.
    pub fn move_focus<'a>(
        &'a mut self,
        contract: &UiAuthoringContract,
        direction: UiFocusDirection,
    ) -> Option<&'a str> {
        let target = {
            let current = self.focused.as_deref()?;
            contract.focus_target(current, direction)?.to_owned()
        };
        self.focused = Some(target);
        self.focused()
    }
}

/// Failure to read, serialize, or persist a typed UI contract.
#[derive(Debug)]
pub enum UiContractDocumentError {
    /// Reading the source document failed.
    Io {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying operating-system error.
        source: std::io::Error,
    },
    /// JSON parsing or serialization failed.
    Json(serde_json::Error),
    /// Atomic persistence failed.
    Persist(PersistError),
}

impl fmt::Display for UiContractDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Json(source) => source.fmt(formatter),
            Self::Persist(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for UiContractDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json(source) => Some(source),
            Self::Persist(source) => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{UiAnchor, UiLayout, UiNode, UiNodeKind, UiString};
    use crate::ui_contract::UiFocusLink;

    fn menu_document() -> UiDocument {
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
    fn contract_json_roundtrip_preserves_focus_graph() {
        let contract = UiAuthoringContract {
            initial_focus: Some("play".to_owned()),
            focus_links: vec![UiFocusLink {
                from: "play".to_owned(),
                direction: UiFocusDirection::Down,
                to: "quit".to_owned(),
            }],
            ..UiAuthoringContract::default()
        };
        let json = contract.to_json_string().expect("contract should serialize");
        let decoded = UiAuthoringContract::from_json_str(&json).expect("contract should decode");
        assert_eq!(decoded, contract);
    }

    #[test]
    fn navigator_uses_initial_focus_and_directional_links() {
        let contract = UiAuthoringContract {
            initial_focus: Some("play".to_owned()),
            focus_links: vec![UiFocusLink {
                from: "play".to_owned(),
                direction: UiFocusDirection::Down,
                to: "quit".to_owned(),
            }],
            ..UiAuthoringContract::default()
        };
        let document = menu_document();
        let mut navigator = UiFocusNavigator::activate(&contract, &document)
            .expect("valid focus graph should activate");
        assert_eq!(navigator.focused(), Some("play"));
        assert_eq!(
            navigator.move_focus(&contract, UiFocusDirection::Down),
            Some("quit")
        );
        assert_eq!(
            navigator.move_focus(&contract, UiFocusDirection::Down),
            None
        );
        assert_eq!(navigator.focused(), Some("quit"));
        assert_eq!(navigator.reset(&contract), Some("play"));
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
