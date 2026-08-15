//! Per-tab transient presentation state.
//!
//! Each workspace tab owns its authoring document, but selection, graph canvas
//! placement, and Inspector edit buffers live on [`EditorApp`] because only the
//! active document is drawn. Snapshotting that state per tab keeps a Scene's
//! selection and a Graph's canvas position alive while the author works in
//! another tab, instead of discarding them on every tab change.
//!
//! Nothing here is persisted: the snapshots exist only for the lifetime of the
//! process and are dropped when their tab closes.

use super::*;

/// Cross-fade duration used until a transition declares an explicit override.
pub(super) const DEFAULT_TRANSITION_FADE_DURATION: f64 = 0.2;

/// Transient presentation state belonging to one document tab.
pub(super) struct DocumentPresentation {
    canvas: GraphCanvasState,
    pending_connect_source: Option<NodeId>,
    property_node: Option<NodeId>,
    property_text: String,
    state_name_text: String,
    transition_edge: Option<EdgeId>,
    transition_condition_text: String,
    transition_fade_duration: f64,
    transition_uses_default_fade: bool,
    selected_entity: Option<EntityId>,
    selected_entities: std::collections::BTreeSet<EntityId>,
    hierarchy_selection_anchor: Option<EntityId>,
    ui_selected_node: Option<String>,
    ui_selected_nodes: std::collections::BTreeSet<String>,
    ui_selection_anchor: Option<String>,
}

impl Default for DocumentPresentation {
    fn default() -> Self {
        Self {
            canvas: GraphCanvasState::default(),
            pending_connect_source: None,
            property_node: None,
            property_text: String::new(),
            state_name_text: String::new(),
            transition_edge: None,
            transition_condition_text: String::new(),
            transition_fade_duration: DEFAULT_TRANSITION_FADE_DURATION,
            transition_uses_default_fade: true,
            selected_entity: None,
            selected_entities: std::collections::BTreeSet::new(),
            hierarchy_selection_anchor: None,
            ui_selected_node: None,
            ui_selected_nodes: std::collections::BTreeSet::new(),
            ui_selection_anchor: None,
        }
    }
}

impl EditorApp {
    /// Moves the live presentation state out of the shell, leaving defaults.
    fn take_document_presentation(&mut self) -> DocumentPresentation {
        DocumentPresentation {
            canvas: std::mem::take(&mut self.canvas),
            pending_connect_source: self.pending_connect_source.take(),
            property_node: self.property_node.take(),
            property_text: std::mem::take(&mut self.property_text),
            state_name_text: std::mem::take(&mut self.state_name_text),
            transition_edge: self.transition_edge.take(),
            transition_condition_text: std::mem::take(&mut self.transition_condition_text),
            transition_fade_duration: self.transition_fade_duration,
            transition_uses_default_fade: self.transition_uses_default_fade,
            selected_entity: self.selected_entity.take(),
            selected_entities: std::mem::take(&mut self.selected_entities),
            hierarchy_selection_anchor: self.hierarchy_selection_anchor.take(),
            ui_selected_node: self.ui_builder.selected_node.take(),
            ui_selected_nodes: std::mem::take(&mut self.ui_builder.selected_nodes),
            ui_selection_anchor: self.ui_builder.selection_anchor.take(),
        }
    }

    /// Installs `presentation` as the state drawn for the active tab.
    fn install_document_presentation(&mut self, presentation: DocumentPresentation) {
        self.canvas = presentation.canvas;
        self.pending_connect_source = presentation.pending_connect_source;
        self.property_node = presentation.property_node;
        self.property_text = presentation.property_text;
        self.state_name_text = presentation.state_name_text;
        self.transition_edge = presentation.transition_edge;
        self.transition_condition_text = presentation.transition_condition_text;
        self.transition_fade_duration = presentation.transition_fade_duration;
        self.transition_uses_default_fade = presentation.transition_uses_default_fade;
        self.selected_entity = presentation.selected_entity;
        self.selected_entities = presentation.selected_entities;
        self.hierarchy_selection_anchor = presentation.hierarchy_selection_anchor;
        self.ui_builder.selected_node = presentation.ui_selected_node;
        self.ui_builder.selected_nodes = presentation.ui_selected_nodes;
        self.ui_builder.selection_anchor = presentation.ui_selection_anchor;
    }

    /// Hands the live presentation state to `outgoing` and installs the state
    /// recorded for the tab that is now active.
    ///
    /// Pass `None` for `outgoing` when the live state must be discarded rather
    /// than remembered, which is the case when its tab was closed or when its
    /// document was replaced in place. A tab with no recorded state, such as a
    /// newly opened document, starts from [`DocumentPresentation::default`].
    pub(super) fn adopt_active_document_presentation(
        &mut self,
        outgoing: Option<WorkspaceTabId>,
    ) {
        // A numeric drag belongs to the pointer rather than to a document, so
        // it can never be resumed after the drawn document changes.
        self.pending_component_drag = None;

        let active = self.session.active_tab_id();
        let leaving = self.take_document_presentation();
        if let Some(outgoing) = outgoing.filter(|id| *id != active && self.session.has_tab(*id)) {
            self.document_presentations.insert(outgoing, leaving);
        }
        let restored = self
            .document_presentations
            .remove(&active)
            .unwrap_or_default();
        self.install_document_presentation(restored);
        self.forget_closed_document_presentations();

        // A restored selection may name entities that undo, a reload, or an
        // external edit removed while the tab was in the background.
        self.prune_dead_selection();
        self.sync_property_buffer();
        self.refresh_scene_problems();
    }

    /// Updates presentation state after the tab `closed` disappeared.
    ///
    /// The closed tab's state is dropped. When it was the active tab, the tab
    /// that took its place gets its own recorded state back.
    pub(super) fn adopt_presentation_after_close(
        &mut self,
        closed: WorkspaceTabId,
        was_active: bool,
    ) {
        self.document_presentations.remove(&closed);
        if was_active {
            self.adopt_active_document_presentation(None);
        } else {
            self.forget_closed_document_presentations();
        }
    }

    /// Drops recorded state for tabs that are no longer open.
    fn forget_closed_document_presentations(&mut self) {
        self.document_presentations
            .retain(|id, _| self.session.has_tab(*id));
    }
}
