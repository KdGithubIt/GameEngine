//! Domain-neutral Play-mode graph debug shell (ADR 0138).

use super::*;
use crate::canvas::{
    show_graph_debug_canvas, GraphCanvasState, GraphDebugBadge, GraphDebugNodePresentation,
    GraphDebugOverlay,
};
use engine_authoring::GraphId;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GraphDebugDomain {
    Animation,
    BehaviorTree,
}

impl GraphDebugDomain {
    fn label(self) -> &'static str {
        match self {
            Self::Animation => "Animation Graph",
            Self::BehaviorTree => "Behavior Tree",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphDebugTargetKey {
    runtime_entity: (u32, u32),
    domain: GraphDebugDomain,
    graph: GraphId,
}

#[derive(Debug, Clone)]
enum GraphDebugObservation {
    Animation(Box<RuntimeAnimationDebugSnapshot>),
    Behavior,
}

#[derive(Debug, Clone)]
struct GraphDebugTarget {
    key: GraphDebugTargetKey,
    entity_name: String,
    authoring_entity: Option<EntityId>,
    observation: GraphDebugObservation,
}

impl GraphDebugTarget {
    fn label(&self) -> String {
        format!("{}  ·  {}", self.entity_name, self.key.domain.label())
    }
}

#[derive(Default)]
pub(super) struct GraphDebugState {
    pub(super) visible: bool,
    selected: Option<GraphDebugTargetKey>,
    play_source_revisions: BTreeMap<GraphId, u64>,
    canvas: GraphCanvasState,
    selected_node: Option<NodeId>,
}

impl GraphDebugState {
    pub(super) fn begin_play(&mut self, workspace: &DocumentWorkspace) {
        self.visible = false;
        self.selected = None;
        self.play_source_revisions = workspace.graph_working_copy_revisions();
        self.canvas = GraphCanvasState::default();
        self.selected_node = None;
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    fn source_is_stale(&self, workspace: &DocumentWorkspace, graph_id: &GraphId) -> bool {
        let Some((_, _, revision, dirty)) = workspace.graph_working_copy(graph_id) else {
            return false;
        };
        dirty
            || self
                .play_source_revisions
                .get(graph_id)
                .is_some_and(|play_revision| *play_revision != revision)
    }
}

impl EditorApp {
    pub(super) fn has_graph_debug_targets(&mut self) -> bool {
        self.runtime_state
            .as_mut()
            .is_some_and(|runtime| !collect_graph_debug_targets(runtime).is_empty())
    }

    pub(super) fn show_graph_debug_workspace(&mut self, ui: &mut egui::Ui) {
        let Some(runtime) = self.runtime_state.as_mut() else {
            self.graph_debug.visible = false;
            return;
        };
        let targets = collect_graph_debug_targets(runtime);
        if targets.is_empty() {
            self.graph_debug.visible = false;
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("Graph Debug");
                ui.label("No live runtime graph execution target exists.");
            });
            return;
        }

        if self
            .graph_debug
            .selected
            .as_ref()
            .is_none_or(|selected| !targets.iter().any(|target| &target.key == selected))
        {
            self.graph_debug.selected = Some(targets[0].key.clone());
        }

        let selected_key = self.graph_debug.selected.clone().expect("target list is non-empty");
        let selected_label = targets
            .iter()
            .find(|target| target.key == selected_key)
            .map(GraphDebugTarget::label)
            .unwrap_or_else(|| "Graph target".to_owned());

        control_row(ui, |ui| {
            ui.strong("Graph Debug");
            egui::ComboBox::from_id_salt("graph_debug_target")
                .selected_text(selected_label)
                .width(300.0)
                .show_ui(ui, |ui| {
                    for target in &targets {
                        if ui
                            .selectable_label(
                                self.graph_debug.selected.as_ref() == Some(&target.key),
                                target.label(),
                            )
                            .clicked()
                        {
                            self.graph_debug.selected = Some(target.key.clone());
                        }
                    }
                });
            if ui.button("Frame All").clicked() {
                self.graph_debug.canvas.request_frame_all();
            }
            ui.small("Read-only runtime observation");
        });
        ui.separator();

        let selected_key = self.graph_debug.selected.clone().expect("selection retained");
        let Some(target) = targets.into_iter().find(|target| target.key == selected_key) else {
            return;
        };
        self.selected_runtime_entity = Some(target.key.runtime_entity);
        if let Some(authoring) = &target.authoring_entity {
            self.select_single_entity(Some(authoring.clone()));
        }

        let stale = self
            .graph_debug
            .source_is_stale(&self.session, &target.key.graph);
        if stale {
            ui.colored_label(
                egui::Color32::YELLOW,
                "Source changed after Play started. Runtime evidence is older than the current working copy; live overlays are suppressed.",
            );
            ui.separator();
        }

        match target.observation {
            GraphDebugObservation::Behavior if !stale => {
                self.show_behavior_tree_debug_workspace(ui);
            }
            GraphDebugObservation::Behavior => {
                self.show_graph_debug_source_only(ui, &target.key.graph);
            }
            GraphDebugObservation::Animation(snapshot) => {
                self.show_animation_graph_debug(ui, &snapshot, stale);
            }
        }
    }

    fn show_graph_debug_source_only(&mut self, ui: &mut egui::Ui, graph_id: &GraphId) {
        let Some(session) = self.graph_debug_source_session(graph_id) else {
            ui.label("Current graph source is unavailable.");
            return;
        };
        show_graph_debug_canvas(
            ui,
            &session,
            &mut self.graph_debug.canvas,
            &GraphDebugOverlay::default(),
            &mut self.graph_debug.selected_node,
        );
    }

    fn show_animation_graph_debug(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &RuntimeAnimationDebugSnapshot,
        stale: bool,
    ) {
        let Some(graph_id) = snapshot.graph_id.as_ref() else {
            ui.heading("Animation Graph");
            ui.colored_label(
                egui::Color32::from_rgb(235, 104, 104),
                snapshot
                    .runtime_error
                    .as_deref()
                    .unwrap_or("Runtime source graph identity is unavailable."),
            );
            return;
        };
        let Some(session) = self.graph_debug_source_session(graph_id) else {
            ui.heading("Animation Graph source unavailable");
            ui.monospace(graph_id.as_str());
            return;
        };

        let mut overlay = GraphDebugOverlay::default();
        if !stale {
            if let Some(edge) = &snapshot.active_transition
                && session.graph().edges.contains_key(edge)
            {
                overlay.active_edges.insert(edge.clone());
            }
            if let Some(node) = &snapshot.current_state
                && session.graph().nodes.contains_key(node)
            {
                overlay.nodes.insert(
                    node.clone(),
                    GraphDebugNodePresentation {
                        active: true,
                        badge: Some(GraphDebugBadge::Running),
                        elapsed_seconds: Some(f64::from(snapshot.clip_time)),
                        detail: Some(format!(
                            "Active state · clip {:.3}s{}",
                            snapshot.clip_time,
                            snapshot
                                .crossfade_progress
                                .map(|progress| format!(" · transition {:.0}%", progress * 100.0))
                                .unwrap_or_default()
                        )),
                    },
                );
            }
            if let Some(node) = &snapshot.previous_state
                && session.graph().nodes.contains_key(node)
            {
                overlay
                    .nodes
                    .entry(node.clone())
                    .or_default()
                    .detail = Some("Transition source state".to_owned());
            }
            if let Some(node) = &snapshot.next_state
                && session.graph().nodes.contains_key(node)
            {
                overlay
                    .nodes
                    .entry(node.clone())
                    .or_default()
                    .detail = Some("Transition destination state".to_owned());
            }
        }

        let selected_node = self.graph_debug.selected_node.clone();
        egui::Panel::right("animation_graph_debug_details")
            .resizable(true)
            .default_size(320.0)
            .min_size(250.0)
            .max_size(460.0)
            .show_inside(ui, |ui| show_animation_graph_debug_details(ui, snapshot, selected_node.as_ref()));

        show_graph_debug_canvas(
            ui,
            &session,
            &mut self.graph_debug.canvas,
            &overlay,
            &mut self.graph_debug.selected_node,
        );
    }

    fn graph_debug_source_session(&self, graph_id: &GraphId) -> Option<EditorSession> {
        if let Some((graph, view, _, _)) = self.session.graph_working_copy(graph_id) {
            return Some(EditorSession::new(graph, view));
        }
        let project = self.project_root.as_ref()?;
        let path = find_graph_source(&project.assets_root(), graph_id)?;
        let mut session = EditorSession::empty_behavior_tree();
        session.open_graph_discarding_changes(path).ok()?;
        (session.graph().id == *graph_id).then_some(session)
    }
}

fn collect_graph_debug_targets(runtime: &mut RuntimePlayState) -> Vec<GraphDebugTarget> {
    let entities = runtime.entity_debug_snapshot();
    let mut targets = Vec::new();
    for row in entities {
        let key = (row.entity.id(), row.entity.generation());
        let authoring_entity = row
            .authoring_id
            .as_deref()
            .and_then(|id| EntityId::from_stable_id(StableId::new(id)).ok());

        if let Some(snapshot) = runtime.animation_graph_debug_snapshot(key)
            && let Some(graph) = snapshot.graph_id.clone()
        {
            targets.push(GraphDebugTarget {
                key: GraphDebugTargetKey {
                    runtime_entity: key,
                    domain: GraphDebugDomain::Animation,
                    graph,
                },
                entity_name: row.name.clone(),
                authoring_entity: Some(snapshot.authoring_entity.clone()),
                observation: GraphDebugObservation::Animation(Box::new(snapshot)),
            });
        }
        if let Some(snapshot) = runtime.behavior_tree_debug_snapshot(key) {
            targets.push(GraphDebugTarget {
                key: GraphDebugTargetKey {
                    runtime_entity: key,
                    domain: GraphDebugDomain::BehaviorTree,
                    graph: snapshot.tree_source.clone(),
                },
                entity_name: row.name,
                authoring_entity,
                observation: GraphDebugObservation::Behavior,
            });
        }
    }
    targets.sort_by(|left, right| {
        left.entity_name
            .cmp(&right.entity_name)
            .then_with(|| left.key.domain.label().cmp(right.key.domain.label()))
            .then_with(|| left.key.graph.as_str().cmp(right.key.graph.as_str()))
    });
    targets
}

fn show_animation_graph_debug_details(
    ui: &mut egui::Ui,
    snapshot: &RuntimeAnimationDebugSnapshot,
    selected_node: Option<&NodeId>,
) {
    ui.heading("Animation Graph");
    ui.monospace(
        snapshot
            .graph_id
            .as_ref()
            .map(GraphId::as_str)
            .unwrap_or("(source unavailable)"),
    );
    ui.label(format!(
        "Runtime entity {}:{}",
        snapshot.runtime_entity.0, snapshot.runtime_entity.1
    ));
    if let Some(asset) = &snapshot.graph_asset {
        ui.small(format!("Graph asset {}", asset.as_str()));
    }
    if let Some(asset) = &snapshot.animation_set_asset {
        ui.small(format!("Animation Set {}", asset.as_str()));
    }

    ui.separator();
    ui.strong("Execution");
    if let Some(state) = &snapshot.current_state {
        ui.label("Current State");
        ui.monospace(state.as_str());
    }
    if let (Some(from), Some(to), Some(progress)) = (
        snapshot.previous_state.as_ref(),
        snapshot.next_state.as_ref(),
        snapshot.crossfade_progress,
    ) {
        ui.label(format!("Transition {:.0}%", progress * 100.0));
        ui.monospace(format!("{} -> {}", from.as_str(), to.as_str()));
        if let Some(edge) = &snapshot.active_transition {
            ui.small(format!("Edge {}", edge.as_str()));
        }
        if let Some(reason) = &snapshot.transition_condition {
            ui.small(format!("Runtime condition: {reason}"));
        }
    }
    ui.label(format!("Clip time {:.3}s", snapshot.clip_time));
    ui.label(format!("Playback x{:.2}", snapshot.playback_speed));

    ui.separator();
    ui.strong("Motion resolution");
    match (
        snapshot.motion_slot.as_ref(),
        snapshot.motion_slot_name.as_deref(),
        snapshot.motion_source.as_ref(),
    ) {
        (Some(slot), name, Some(source)) => {
            ui.monospace(slot.as_str());
            if let Some(name) = name {
                ui.label(name);
            }
            ui.label(format!(
                "{:?} -> {}",
                snapshot.resolved_motion_variant.unwrap_or(source.variant),
                source.asset.as_str()
            ));
            ui.small(format!("Concrete runtime clip {}", snapshot.clip_runtime_id));
        }
        _ => {
            ui.small("Active state has no resolved Motion Slot evidence.");
        }
    }

    ui.separator();
    ui.strong("Parameters");
    if snapshot.graph_parameter_values.is_empty() {
        ui.small("No runtime parameters");
    } else {
        egui::Grid::new("animation_graph_debug_parameters")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for (name, value) in &snapshot.graph_parameter_values {
                    ui.label(name);
                    ui.monospace(value);
                    ui.end_row();
                }
            });
    }

    ui.separator();
    ui.strong("Recent animation events");
    if snapshot.recent_events.is_empty() {
        ui.small("No events fired in the latest fixed step.");
    } else {
        for event in &snapshot.recent_events {
            ui.small(event);
        }
    }
    if let Some(error) = &snapshot.runtime_error {
        ui.colored_label(egui::Color32::from_rgb(235, 104, 104), error);
    }

    ui.separator();
    match selected_node {
        Some(node) => {
            ui.small("Selected source node");
            ui.monospace(node.as_str());
        }
        None => {
            ui.small("Select a source node for stable-ID inspection.");
        }
    }
    ui.small("Graph Debug is read-only and never mutates runtime parameters or source documents.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_source_detects_dirty_or_revision_changed_working_copy() {
        let mut state = GraphDebugState::default();
        let workspace = DocumentWorkspace::new(EditorSession::empty_animation_graph());
        state.begin_play(&workspace);
        let graph = workspace.graph().id.clone();
        assert!(!state.source_is_stale(&workspace, &graph));
    }
}
