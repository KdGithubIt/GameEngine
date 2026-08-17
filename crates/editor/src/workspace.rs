//! In-memory document tabs for the editor workspace.
//!
//! Each tab owns a complete [`EditorSession`], so switching tabs preserves
//! unsaved authoring data and per-document undo history without changing any
//! persisted project format.

use crate::document::{CurrentDocument, OpenDocumentError};
use crate::session::{EditorPersistError, EditorSession};
use engine_authoring::{AuthoringScene, EntityId};
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

/// Stable process-local identifier for one open editor tab.
pub(crate) type WorkspaceTabId = u64;

/// Document type displayed by a workspace tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkspaceDocumentKind {
    Scene,
    Graph,
    Ui,
}

impl WorkspaceDocumentKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Scene => "Scene",
            Self::Graph => "Graph",
            Self::Ui => "UI Builder",
        }
    }
}

/// Immutable data used to draw one tab without borrowing its session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceTabSummary {
    pub(crate) id: WorkspaceTabId,
    pub(crate) kind: WorkspaceDocumentKind,
    pub(crate) label: String,
    pub(crate) is_active: bool,
    pub(crate) is_dirty: bool,
}

struct WorkspaceTab {
    id: WorkspaceTabId,
    session: EditorSession,
}

/// Collection of open document sessions with one active tab.
pub(crate) struct DocumentWorkspace {
    tabs: Vec<WorkspaceTab>,
    active_index: usize,
    next_id: WorkspaceTabId,
}

impl DocumentWorkspace {
    pub(crate) fn new(session: EditorSession) -> Self {
        Self {
            tabs: vec![WorkspaceTab { id: 1, session }],
            active_index: 0,
            next_id: 2,
        }
    }

    /// Replaces every tab with one tab holding `session`.
    ///
    /// The replacement tab receives an unused identifier so presentation state
    /// recorded for a discarded tab can never be restored into an unrelated
    /// document that happened to reuse its number.
    pub(crate) fn reset(&mut self, session: EditorSession) {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.tabs = vec![WorkspaceTab { id, session }];
        self.active_index = 0;
    }

    /// Returns the identifier of the tab currently drawn by the editor.
    pub(crate) fn active_tab_id(&self) -> WorkspaceTabId {
        self.tabs[self.active_index].id
    }

    /// Reports whether `id` still identifies an open tab.
    pub(crate) fn has_tab(&self, id: WorkspaceTabId) -> bool {
        self.tabs.iter().any(|tab| tab.id == id)
    }

    pub(crate) fn tab_session_mut(&mut self, id: WorkspaceTabId) -> Option<&mut EditorSession> {
        self.tabs.iter_mut().find(|tab| tab.id == id).map(|tab| &mut tab.session)
    }

    /// Iterates every open session without transferring working-copy ownership.
    pub(crate) fn sessions(&self) -> impl Iterator<Item = &EditorSession> {
        self.tabs.iter().map(|tab| &tab.session)
    }

    /// Returns the one existing session that owns `path`, if the document is open.
    pub(crate) fn session_for_path(&self, path: &Path) -> Option<&EditorSession> {
        self.tabs.iter().find_map(|tab| {
            tab.session
                .current_document_path()
                .is_some_and(|candidate| candidate == path)
                .then_some(&tab.session)
        })
    }

    pub(crate) fn summaries(&self) -> Vec<WorkspaceTabSummary> {
        self.tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                let kind = document_kind(tab.session.current_document())?;
                let label = tab
                    .session
                    .current_document_path()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| kind.label().to_owned());
                Some(WorkspaceTabSummary {
                    id: tab.id,
                    kind,
                    label,
                    is_active: index == self.active_index,
                    is_dirty: tab.session.is_dirty(),
                })
            })
            .collect()
    }

    pub(crate) fn activate(&mut self, id: WorkspaceTabId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        let changed = self.active_index != index;
        self.active_index = index;
        changed
    }

    pub(crate) fn open_document(
        &mut self,
        kind: WorkspaceDocumentKind,
        path: PathBuf,
    ) -> Result<(), OpenDocumentError> {
        if let Some(index) = self.tabs.iter().position(|tab| {
            tab.session
                .current_document_path()
                .is_some_and(|open_path| open_path == path)
        }) {
            self.active_index = index;
            return Ok(());
        }

        let mut session = EditorSession::empty_behavior_tree();
        open_session_document(&mut session, kind, path)?;

        if self.tabs.len() == 1
            && self.tabs[0].session.current_document_path().is_none()
            && !self.tabs[0].session.is_dirty()
        {
            self.tabs[0].session = session;
            return Ok(());
        }

        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.tabs.push(WorkspaceTab { id, session });
        self.active_index = self.tabs.len() - 1;
        Ok(())
    }

    pub(crate) fn close_if_clean(&mut self, id: WorkspaceTabId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        if self.tabs[index].session.is_dirty() {
            return false;
        }

        self.remove_tab_at(index);
        true
    }

    /// Closes one tab after the user explicitly chose to discard its changes.
    pub(crate) fn close_discarding_changes(&mut self, id: WorkspaceTabId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        self.tabs[index].session.close_document_discarding_changes();
        self.remove_tab_at(index);
        true
    }

    /// Saves only the requested tab and activates it if persistence fails.
    pub(crate) fn save_tab(&mut self, id: WorkspaceTabId) -> Result<bool, EditorPersistError> {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return Ok(false);
        };
        self.active_index = index;
        self.tabs[index].session.save()?;
        Ok(true)
    }

    /// Removes a known tab index and keeps the active index valid.
    fn remove_tab_at(&mut self, index: usize) {
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.reset(EditorSession::empty_behavior_tree());
        } else if index < self.active_index {
            self.active_index -= 1;
        } else if self.active_index >= self.tabs.len() {
            self.active_index = self.tabs.len() - 1;
        }
    }

    pub(crate) fn any_dirty(&self) -> bool {
        self.tabs.iter().any(|tab| tab.session.is_dirty())
    }

    pub(crate) fn save_all(&mut self) -> Result<(), EditorPersistError> {
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            if tab.session.is_dirty()
                && let Err(error) = tab.session.save() {
                    self.active_index = index;
                    return Err(error);
                }
        }
        Ok(())
    }

    pub(crate) fn autosave_recovery_all(&self) -> Vec<String> {
        self.tabs
            .iter()
            .filter_map(|tab| tab.session.autosave_recovery().err())
            .map(|error| error.to_string())
            .collect()
    }

    /// Returns the document path of every tab in tab order.
    ///
    /// A tab that has never been saved has no path and is skipped, because
    /// there is nothing a later editor session could reopen for it.
    pub(crate) fn open_document_paths(&self) -> Vec<PathBuf> {
        self.tabs
            .iter()
            .filter_map(|tab| tab.session.current_document_path().map(Path::to_path_buf))
            .collect()
    }

    pub(crate) fn tab_for_path(&self, path: &Path) -> Option<WorkspaceTabId> {
        self.tabs.iter().find_map(|tab| {
            tab.session
                .current_document_path()
                .is_some_and(|open_path| open_path == path)
                .then_some(tab.id)
        })
    }

    pub(crate) fn tab_is_dirty(&self, id: WorkspaceTabId) -> bool {
        self.tabs
            .iter()
            .find(|tab| tab.id == id)
            .is_some_and(|tab| tab.session.is_dirty())
    }

    /// Returns an open scene suitable for editor-only preview tools.
    ///
    /// A scene containing `target` wins, followed by the active scene and the
    /// first remaining scene tab. This lets an Animation Graph tab keep using
    /// the character selected in a neighboring scene tab.
    pub(crate) fn scene_context(
        &self,
        target: Option<&EntityId>,
    ) -> Option<(&AuthoringScene, u64)> {
        if let Some(target) = target
            && let Some(session) = self
                .tabs
                .iter()
                .map(|tab| &tab.session)
                .find(|session| session.scene_entity(target).is_some())
            {
                return session
                    .scene()
                    .map(|scene| (scene, session.document_revision()));
            }
        let active = &self.tabs[self.active_index].session;
        if let Some(scene) = active.scene() {
            return Some((scene, active.document_revision()));
        }
        self.tabs.iter().find_map(|tab| {
            tab.session
                .scene()
                .map(|scene| (scene, tab.session.document_revision()))
        })
    }

    pub(crate) fn reload_tab(&mut self, id: WorkspaceTabId) -> Result<(), OpenDocumentError> {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else {
            return Ok(());
        };
        let Some(path) = tab.session.current_document_path().map(Path::to_path_buf) else {
            return Ok(());
        };
        let Some(kind) = document_kind(tab.session.current_document()) else {
            return Ok(());
        };
        open_session_document(&mut tab.session, kind, path)
    }
}

impl Deref for DocumentWorkspace {
    type Target = EditorSession;

    fn deref(&self) -> &Self::Target {
        &self.tabs[self.active_index].session
    }
}

impl DerefMut for DocumentWorkspace {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tabs[self.active_index].session
    }
}

fn document_kind(document: &CurrentDocument) -> Option<WorkspaceDocumentKind> {
    match document {
        CurrentDocument::None => None,
        CurrentDocument::Scene { .. } => Some(WorkspaceDocumentKind::Scene),
        CurrentDocument::Graph { .. } => Some(WorkspaceDocumentKind::Graph),
        CurrentDocument::Ui { .. } => Some(WorkspaceDocumentKind::Ui),
    }
}

fn open_session_document(
    session: &mut EditorSession,
    kind: WorkspaceDocumentKind,
    path: PathBuf,
) -> Result<(), OpenDocumentError> {
    match kind {
        WorkspaceDocumentKind::Scene => session.open_scene_discarding_changes(path),
        WorkspaceDocumentKind::Graph => session.open_graph_discarding_changes(path),
        WorkspaceDocumentKind::Ui => session.open_ui_discarding_changes(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::UiDocument;

    #[test]
    fn opening_documents_creates_tabs_and_reuses_an_existing_path() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first = directory.path().join("first.ui.json");
        let second = directory.path().join("second.ui.json");
        let json = UiDocument::default()
            .to_json_string()
            .expect("default UI document serializes");
        std::fs::write(&first, &json).expect("first UI fixture writes");
        std::fs::write(&second, &json).expect("second UI fixture writes");

        let mut workspace = DocumentWorkspace::new(EditorSession::empty_behavior_tree());
        workspace
            .open_document(WorkspaceDocumentKind::Ui, first.clone())
            .expect("first UI opens");
        workspace
            .open_document(WorkspaceDocumentKind::Ui, second)
            .expect("second UI opens");
        assert_eq!(workspace.summaries().len(), 2);

        workspace
            .open_document(WorkspaceDocumentKind::Ui, first)
            .expect("existing UI focuses");
        assert_eq!(workspace.summaries().len(), 2);
        assert!(workspace.summaries()[0].is_active);
    }

    #[test]
    fn dirty_tab_can_be_saved_and_closed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("document.ui.json");
        let json = UiDocument::default()
            .to_json_string()
            .expect("default UI document serializes");
        std::fs::write(&path, &json).expect("UI fixture writes");

        let mut workspace = DocumentWorkspace::new(EditorSession::empty_behavior_tree());
        workspace
            .open_document(WorkspaceDocumentKind::Ui, path)
            .expect("UI document opens");
        workspace
            .apply_ui_command(engine_authoring::UiDocumentCommand::SetResponsiveSettings {
                reference_resolution: [1280.0, 720.0],
                scale_policy: engine_authoring::UiScalePolicy::ConstantPixels,
                safe_area_padding: [0.0; 4],
            })
            .expect("UI document edit succeeds");
        let tab_id = workspace.summaries()[0].id;

        assert!(workspace.tab_is_dirty(tab_id));
        assert!(workspace.save_tab(tab_id).expect("tab saves"));
        assert!(!workspace.tab_is_dirty(tab_id));
        assert!(workspace.close_if_clean(tab_id));
        assert!(workspace.summaries().is_empty());
    }

    #[test]
    fn discarding_dirty_tab_removes_its_recovery_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("document.ui.json");
        let json = UiDocument::default()
            .to_json_string()
            .expect("default UI document serializes");
        std::fs::write(&path, &json).expect("UI fixture writes");

        let mut workspace = DocumentWorkspace::new(EditorSession::empty_behavior_tree());
        workspace
            .open_document(WorkspaceDocumentKind::Ui, path)
            .expect("UI document opens");
        workspace
            .apply_ui_command(engine_authoring::UiDocumentCommand::SetResponsiveSettings {
                reference_resolution: [1280.0, 720.0],
                scale_policy: engine_authoring::UiScalePolicy::ConstantPixels,
                safe_area_padding: [0.0; 4],
            })
            .expect("UI document edit succeeds");
        let recovery = workspace
            .autosave_recovery()
            .expect("recovery snapshot writes")
            .expect("dirty tab creates recovery snapshot");
        let tab_id = workspace.summaries()[0].id;

        assert!(recovery.is_file());
        assert!(workspace.close_discarding_changes(tab_id));
        assert!(!recovery.exists());
        assert!(workspace.summaries().is_empty());
    }
}
