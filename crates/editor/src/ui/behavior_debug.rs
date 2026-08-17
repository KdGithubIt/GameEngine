//! Behavior Tree runtime snapshot presentation for Editor Play mode.

use super::*;
use crate::canvas::{
    show_graph_debug_canvas, GraphCanvasState, GraphDebugBadge, GraphDebugNodePresentation,
    GraphDebugOverlay,
};
#[cfg(feature = "visual-validation")]
use crate::canvas::show_graph_node_palette_visual_fixture;
use engine::behavior_tree::{
    BehaviorExecutionSnapshot, BehaviorExecutionTransitionKind, BehaviorResetReason, BehaviorStatus,
};
use engine_authoring::{Graph, GraphId, NodeId};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BehaviorDebugSourceKey {
    runtime_entity: (u32, u32),
    graph: GraphId,
    tree_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GraphFileFingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

impl GraphFileFingerprint {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

/// Transient Play-mode Behavior Tree graph-debug presentation state.
#[derive(Default)]
pub(super) struct BehaviorTreeDebugState {
    pub(super) visible: bool,
    source_key: Option<BehaviorDebugSourceKey>,
    source_path: Option<PathBuf>,
    source_fingerprint: Option<GraphFileFingerprint>,
    graph_session: Option<EditorSession>,
    invalidated: bool,
    message: Option<String>,
    canvas: GraphCanvasState,
    #[cfg(feature = "visual-validation")]
    fixture_snapshot: Option<BehaviorExecutionSnapshot>,
}

impl BehaviorTreeDebugState {
    /// Clears all Play-mode Behavior Tree debug presentation.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Drops the observed runner and graph while preserving view visibility.
    pub(super) fn clear_observation(&mut self) {
        let visible = self.visible;
        *self = Self::default();
        self.visible = visible;
    }

    fn sync(
        &mut self,
        runtime_entity: (u32, u32),
        project: Option<&ProjectRoot>,
        snapshot: &BehaviorExecutionSnapshot,
    ) {
        let next_key = BehaviorDebugSourceKey {
            runtime_entity,
            graph: snapshot.tree_source.clone(),
            tree_generation: snapshot.tree_generation,
        };
        if self.source_key.as_ref() != Some(&next_key) {
            self.clear_observation();
            self.source_key = Some(next_key);
            self.canvas.request_frame_all();
        }

        if self.invalidated {
            return;
        }
        if let (Some(path), Some(expected)) = (&self.source_path, &self.source_fingerprint)
            && GraphFileFingerprint::read(path).as_ref() != Some(expected)
        {
            self.graph_session = None;
            self.invalidated = true;
            self.message = Some(
                "The source graph changed after this runner generation was loaded. Replace/restart the runner before continuing live graph debugging."
                    .to_owned(),
            );
            return;
        }
        if self.graph_session.is_some() {
            return;
        }

        let Some(project) = project else {
            self.message = Some("The Play session has no project root for source graph lookup.".into());
            return;
        };
        let Some(path) = find_graph_source(&project.assets_root(), &snapshot.tree_source) else {
            self.message = Some(format!(
                "Could not locate source Behavior Tree graph {} under the project assets.",
                snapshot.tree_source.as_str()
            ));
            return;
        };
        let mut session = EditorSession::empty_behavior_tree();
        match session.open_graph_discarding_changes(path.clone()) {
            Ok(()) if session.graph().id == snapshot.tree_source && !session.is_animation_graph() => {
                self.source_fingerprint = GraphFileFingerprint::read(&path);
                self.source_path = Some(path);
                self.graph_session = Some(session);
                self.message = None;
            }
            Ok(()) => {
                self.invalidated = true;
                self.message = Some(
                    "The resolved graph no longer matches the runtime Behavior Tree source identity."
                        .into(),
                );
            }
            Err(error) => {
                self.message = Some(format!("Could not load the source Behavior Tree graph: {error}"));
            }
        }
    }

    fn show_graph(&mut self, ui: &mut egui::Ui, presentation: &BehaviorTreeDebugPresentation) {
        if let Some(session) = self.graph_session.as_ref() {
            show_graph_debug_canvas(ui, session, &mut self.canvas, &presentation.overlay);
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("Behavior Tree source unavailable");
                if let Some(message) = &self.message {
                    ui.label(message);
                }
            });
        }
    }

    #[cfg(feature = "visual-validation")]
    fn install_visual_fixture(&mut self) -> bool {
        let Ok(session) = EditorSession::behavior_tree_example() else {
            return false;
        };
        let nodes = session.graph().nodes.keys().cloned().collect::<Vec<_>>();
        if nodes.len() < 3 {
            return false;
        }
        let active_path = nodes
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, node)| engine::behavior_tree::BehaviorActiveNodeSnapshot {
                node: node.clone(),
                elapsed_seconds: 0.35 + index as f64 * 0.42,
            })
            .collect();
        let mut recent_transitions = Vec::new();
        recent_transitions.push(engine::behavior_tree::BehaviorExecutionTransition {
            generation: 4,
            node: nodes.get(1).cloned(),
            behavior_id: Some("enemy.has_target".into()),
            kind: BehaviorExecutionTransitionKind::Success,
            reason: None,
        });
        recent_transitions.push(engine::behavior_tree::BehaviorExecutionTransition {
            generation: 4,
            node: nodes.get(2).cloned(),
            behavior_id: Some("enemy.chase_target".into()),
            kind: BehaviorExecutionTransitionKind::Enter,
            reason: None,
        });
        if let Some(node) = nodes.get(3) {
            recent_transitions.push(engine::behavior_tree::BehaviorExecutionTransition {
                generation: 4,
                node: Some(node.clone()),
                behavior_id: Some("enemy.previous_action".into()),
                kind: BehaviorExecutionTransitionKind::Abort,
                reason: Some(BehaviorResetReason::Interrupted),
            });
        }
        if let Some(node) = nodes.get(4) {
            recent_transitions.push(engine::behavior_tree::BehaviorExecutionTransition {
                generation: 4,
                node: Some(node.clone()),
                behavior_id: Some("enemy.fallback_failed".into()),
                kind: BehaviorExecutionTransitionKind::Failure,
                reason: None,
            });
        }
        let snapshot = BehaviorExecutionSnapshot {
            tree_source: session.graph().id.clone(),
            tree_generation: 2,
            execution_generation: 4,
            status: Some(BehaviorStatus::Running),
            active_path,
            running_node: nodes.get(2).cloned(),
            last_terminal_node: nodes.get(1).cloned(),
            last_terminal_status: Some(BehaviorStatus::Success),
            last_reset_reason: Some(BehaviorResetReason::Interrupted),
            recent_transitions,
            blackboard: BTreeMap::new(),
            error: None,
        };
        self.clear();
        self.visible = true;
        self.graph_session = Some(session);
        self.fixture_snapshot = Some(snapshot);
        self.canvas.request_frame_all();
        true
    }
}

#[derive(Debug)]
struct BehaviorTreeDebugPresentation {
    runtime_entity: Option<(u32, u32)>,
    graph: GraphId,
    tree_generation: u64,
    execution_generation: u64,
    status: Option<BehaviorStatus>,
    last_reset_reason: Option<BehaviorResetReason>,
    recent_transitions: Vec<String>,
    blackboard: Vec<(String, String)>,
    error: Option<String>,
    overlay: GraphDebugOverlay,
}

impl BehaviorTreeDebugPresentation {
    fn from_snapshot(
        runtime_entity: Option<(u32, u32)>,
        snapshot: &BehaviorExecutionSnapshot,
    ) -> Self {
        let mut nodes: BTreeMap<NodeId, GraphDebugNodePresentation> = BTreeMap::new();
        for active in &snapshot.active_path {
            let entry = nodes.entry(active.node.clone()).or_default();
            entry.active = true;
            entry.elapsed_seconds = Some(active.elapsed_seconds);
        }

        if let (Some(node), Some(status)) = (
            snapshot.last_terminal_node.as_ref(),
            snapshot.last_terminal_status,
        ) {
            let badge = match status {
                BehaviorStatus::Success => Some(GraphDebugBadge::Success),
                BehaviorStatus::Failure => Some(GraphDebugBadge::Failure),
                BehaviorStatus::Running => None,
            };
            if let Some(badge) = badge {
                nodes.entry(node.clone()).or_default().badge = Some(badge);
            }
        }

        for transition in snapshot.recent_transitions.iter().rev() {
            if transition.generation != snapshot.execution_generation {
                continue;
            }
            let Some(node) = transition.node.as_ref() else {
                continue;
            };
            let entry = nodes.entry(node.clone()).or_default();
            if entry.badge.is_none() {
                entry.badge = Some(match transition.kind {
                    BehaviorExecutionTransitionKind::Enter => GraphDebugBadge::Entered,
                    BehaviorExecutionTransitionKind::Success => GraphDebugBadge::Success,
                    BehaviorExecutionTransitionKind::Failure => GraphDebugBadge::Failure,
                    BehaviorExecutionTransitionKind::Abort => GraphDebugBadge::Aborted,
                    BehaviorExecutionTransitionKind::Reset => GraphDebugBadge::Reset,
                });
            }
            if entry.detail.is_none() {
                entry.detail = Some(format_transition(transition));
            }
        }

        if let Some(node) = snapshot.running_node.as_ref() {
            let entry = nodes.entry(node.clone()).or_default();
            entry.active = true;
            entry.badge = Some(GraphDebugBadge::Running);
        }

        let recent_transitions = snapshot
            .recent_transitions
            .iter()
            .rev()
            .take(8)
            .map(format_transition)
            .collect();
        let blackboard = snapshot
            .blackboard
            .iter()
            .map(|(key, value)| {
                let value = serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"));
                (key.clone(), value)
            })
            .collect();

        Self {
            runtime_entity,
            graph: snapshot.tree_source.clone(),
            tree_generation: snapshot.tree_generation,
            execution_generation: snapshot.execution_generation,
            status: snapshot.status,
            last_reset_reason: snapshot.last_reset_reason,
            recent_transitions,
            blackboard,
            error: snapshot.error.clone(),
            overlay: GraphDebugOverlay { nodes },
        }
    }
}

impl EditorApp {
    /// Draws the transient live Behavior Tree debugger for the current Play session.
    pub(super) fn show_behavior_tree_debug_workspace(&mut self, ui: &mut egui::Ui) {
        #[cfg(feature = "visual-validation")]
        let fixture = self.behavior_debug.fixture_snapshot.clone();
        #[cfg(not(feature = "visual-validation"))]
        let fixture: Option<BehaviorExecutionSnapshot> = None;

        let runtime_observation = self.runtime_state.as_ref().and_then(|runtime| {
            if let Some(key) = self.selected_runtime_entity
                && let Some(snapshot) = runtime.behavior_tree_debug_snapshot(key)
            {
                return Some((key, snapshot));
            }
            let key = runtime.first_behavior_tree_entity_key()?;
            runtime
                .behavior_tree_debug_snapshot(key)
                .map(|snapshot| (key, snapshot))
        });
        let is_fixture = fixture.is_some();
        let (runtime_entity, snapshot) = match fixture {
            Some(snapshot) => (Some((42, 7)), Some(snapshot)),
            None => match runtime_observation {
                Some((key, snapshot)) => {
                    self.selected_runtime_entity = Some(key);
                    (Some(key), Some(snapshot))
                }
                None => (None, None),
            },
        };

        let Some(snapshot) = snapshot else {
            self.behavior_debug.clear_observation();
            ui.vertical_centered(|ui| {
                ui.add_space(48.0);
                ui.heading("Behavior Tree Debug");
                ui.label("Select a runtime entity with a Behavior Tree runner.");
                ui.small("The overlay is read-only and exists only for the current Play session.");
            });
            return;
        };
        if !is_fixture && let Some(key) = runtime_entity {
            self.behavior_debug
                .sync(key, self.project_root.as_ref(), &snapshot);
        }
        let presentation = BehaviorTreeDebugPresentation::from_snapshot(runtime_entity, &snapshot);

        #[cfg(feature = "visual-validation")]
        if is_fixture {
            let context = ui.ctx().clone();
            egui::Window::new("Behavior Tree Visual Validation")
                .id(egui::Id::new("behavior_tree_visual_validation_workspace"))
                .collapsible(false)
                .resizable(false)
                .fixed_pos(egui::pos2(8.0, 60.0))
                .fixed_size(egui::vec2(1080.0, 660.0))
                .show(&context, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(350.0);
                            ui.set_min_height(600.0);
                            if let Some(session) = self.behavior_debug.graph_session.as_ref() {
                                show_graph_node_palette_visual_fixture(ui, session);
                            }
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(470.0);
                            ui.set_min_height(600.0);
                            self.behavior_debug.show_graph(ui, &presentation);
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(210.0);
                            ui.set_min_height(600.0);
                            show_behavior_debug_details(ui, &presentation);
                        });
                    });
                });
            return;
        }

        egui::Panel::right("behavior_tree_debug_details")
            .resizable(true)
            .default_size(280.0)
            .min_size(220.0)
            .max_size(420.0)
            .show_inside(ui, |ui| show_behavior_debug_details(ui, &presentation));
        self.behavior_debug.show_graph(ui, &presentation);
    }

    #[cfg(feature = "visual-validation")]
    /// Installs deterministic live-debug presentation for the trusted visual-validation build.
    pub(super) fn prepare_behavior_tree_visual_validation(&mut self) {
        if !visual_validation_touches_behavior_debug() {
            return;
        }
        if self.behavior_debug.install_visual_fixture() {
            self.editor_mode = EditorMode::Playing;
            self.selected_runtime_entity = None;
            self.game_view_focused = false;
        }
    }
}

fn show_behavior_debug_details(ui: &mut egui::Ui, presentation: &BehaviorTreeDebugPresentation) {
    ui.heading("Behavior Tree Debug");
    if let Some((id, generation)) = presentation.runtime_entity {
        ui.label(format!("Runner entity {id}"));
        ui.label(format!("Entity generation {generation}"));
    }
    ui.monospace(presentation.graph.as_str());
    ui.label(format!(
        "Tree gen {}  |  Execution gen {}",
        presentation.tree_generation, presentation.execution_generation
    ));
    ui.separator();
    ui.strong(format!(
        "Status: {}",
        presentation
            .status
            .map(status_label)
            .unwrap_or("Not ticked")
    ));
    if let Some(reason) = presentation.last_reset_reason {
        ui.label(format!("Last abort/reset: {}", reset_reason_label(reason)));
    }
    if let Some(error) = &presentation.error {
        ui.colored_label(egui::Color32::from_rgb(235, 104, 104), error);
    }

    ui.separator();
    ui.strong("Recent lifecycle");
    if presentation.recent_transitions.is_empty() {
        ui.small("No recent transitions");
    } else {
        egui::ScrollArea::vertical()
            .id_salt("behavior_debug_transitions")
            .max_height(180.0)
            .show(ui, |ui| {
                for transition in &presentation.recent_transitions {
                    ui.small(transition);
                }
            });
    }

    ui.separator();
    ui.strong("Blackboard");
    if presentation.blackboard.is_empty() {
        ui.small("Empty");
    } else {
        egui::Grid::new("behavior_debug_blackboard")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for (key, value) in &presentation.blackboard {
                    ui.label(key);
                    ui.monospace(value);
                    ui.end_row();
                }
            });
    }
    ui.separator();
    ui.small("Live overlay is transient; it never edits the Graph document or GraphView.");
}

fn format_transition(transition: &engine::behavior_tree::BehaviorExecutionTransition) -> String {
    let mut label = match transition.kind {
        BehaviorExecutionTransitionKind::Enter => "ENTER".to_owned(),
        BehaviorExecutionTransitionKind::Success => "SUCCESS".to_owned(),
        BehaviorExecutionTransitionKind::Failure => "FAILURE".to_owned(),
        BehaviorExecutionTransitionKind::Abort => "ABORT".to_owned(),
        BehaviorExecutionTransitionKind::Reset => "RESET".to_owned(),
    };
    if let Some(node) = &transition.node {
        label.push(' ');
        label.push_str(node.as_str());
    }
    if let Some(behavior) = &transition.behavior_id {
        label.push_str(" · ");
        label.push_str(behavior);
    }
    if let Some(reason) = transition.reason {
        label.push_str(" · ");
        label.push_str(reset_reason_label(reason));
    }
    label
}

fn status_label(status: BehaviorStatus) -> &'static str {
    match status {
        BehaviorStatus::Success => "Success",
        BehaviorStatus::Failure => "Failure",
        BehaviorStatus::Running => "Running",
    }
}

fn reset_reason_label(reason: BehaviorResetReason) -> &'static str {
    match reason {
        BehaviorResetReason::RunnerDisabled => "runner disabled",
        BehaviorResetReason::ExplicitReset => "explicit reset",
        BehaviorResetReason::TreeReplaced => "tree replaced",
        BehaviorResetReason::Timeout => "timeout",
        BehaviorResetReason::Interrupted => "interrupted",
        BehaviorResetReason::RunnerRemoved => "runner removed",
        BehaviorResetReason::EntityDespawned => "entity despawned",
    }
}

fn find_graph_source(assets_root: &Path, graph_id: &GraphId) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    collect_graph_files(assets_root, &mut candidates);
    candidates.sort();
    candidates.into_iter().find(|path| {
        fs::read_to_string(path)
            .ok()
            .and_then(|json| serde_json::from_str::<Graph>(&json).ok())
            .is_some_and(|graph| graph.id == *graph_id)
    })
}

fn collect_graph_files(directory: &Path, output: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_graph_files(&path, output);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".graph.json"))
        {
            output.push(path);
        }
    }
}

#[cfg(feature = "visual-validation")]
fn visual_validation_touches_behavior_debug() -> bool {
    let base_ref = std::env::var("GITHUB_BASE_REF").unwrap_or_else(|_| "main".into());
    let base = format!("origin/{base_ref}...HEAD");
    std::process::Command::new("git")
        .args([
            "diff",
            "--name-only",
            &base,
            "--",
            "crates/editor/src/ui/behavior_debug.rs",
        ])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !output.stdout.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presentation_maps_source_node_ids_without_mutating_runtime_snapshot() {
        let session = EditorSession::behavior_tree_example().expect("reference tree");
        let nodes = session.graph().nodes.keys().cloned().collect::<Vec<_>>();
        let snapshot = BehaviorExecutionSnapshot {
            tree_source: session.graph().id.clone(),
            tree_generation: 3,
            execution_generation: 7,
            status: Some(BehaviorStatus::Running),
            active_path: vec![engine::behavior_tree::BehaviorActiveNodeSnapshot {
                node: nodes[0].clone(),
                elapsed_seconds: 1.25,
            }],
            running_node: Some(nodes[0].clone()),
            last_terminal_node: Some(nodes[1].clone()),
            last_terminal_status: Some(BehaviorStatus::Failure),
            last_reset_reason: None,
            recent_transitions: Vec::new(),
            blackboard: BTreeMap::new(),
            error: None,
        };
        let before = snapshot.clone();

        let presentation =
            BehaviorTreeDebugPresentation::from_snapshot(Some((5, 2)), &snapshot);

        assert_eq!(snapshot, before);
        assert_eq!(presentation.runtime_entity, Some((5, 2)));
        assert_eq!(presentation.graph, session.graph().id);
        assert_eq!(
            presentation.overlay.nodes[&nodes[0]].badge,
            Some(GraphDebugBadge::Running)
        );
        assert_eq!(
            presentation.overlay.nodes[&nodes[1]].badge,
            Some(GraphDebugBadge::Failure)
        );
    }

    #[test]
    fn old_execution_generation_lifecycle_is_not_presented_as_current() {
        let session = EditorSession::behavior_tree_example().expect("reference tree");
        let node = session.graph().nodes.keys().next().unwrap().clone();
        let snapshot = BehaviorExecutionSnapshot {
            tree_source: session.graph().id.clone(),
            tree_generation: 1,
            execution_generation: 9,
            status: None,
            active_path: Vec::new(),
            running_node: None,
            last_terminal_node: None,
            last_terminal_status: None,
            last_reset_reason: Some(BehaviorResetReason::ExplicitReset),
            recent_transitions: vec![engine::behavior_tree::BehaviorExecutionTransition {
                generation: 8,
                node: Some(node.clone()),
                behavior_id: None,
                kind: BehaviorExecutionTransitionKind::Abort,
                reason: Some(BehaviorResetReason::Interrupted),
            }],
            blackboard: BTreeMap::new(),
            error: None,
        };

        let presentation = BehaviorTreeDebugPresentation::from_snapshot(None, &snapshot);

        assert!(!presentation.overlay.nodes.contains_key(&node));
    }

    #[test]
    fn clear_observation_drops_runner_state_but_preserves_debug_view_visibility() {
        let mut state = BehaviorTreeDebugState {
            visible: true,
            source_key: Some(BehaviorDebugSourceKey {
                runtime_entity: (1, 3),
                graph: GraphId::generate(),
                tree_generation: 4,
            }),
            graph_session: Some(EditorSession::empty_behavior_tree()),
            invalidated: true,
            message: Some("stale".into()),
            ..BehaviorTreeDebugState::default()
        };

        state.clear_observation();

        assert!(state.visible);
        assert!(state.source_key.is_none());
        assert!(state.graph_session.is_none());
        assert!(!state.invalidated);
        assert!(state.message.is_none());
    }

    #[test]
    fn runner_tree_generation_change_rejects_stale_debug_observation() {
        let session = EditorSession::behavior_tree_example().expect("reference tree");
        let mut snapshot = BehaviorExecutionSnapshot {
            tree_source: session.graph().id.clone(),
            tree_generation: 1,
            execution_generation: 1,
            status: None,
            active_path: Vec::new(),
            running_node: None,
            last_terminal_node: None,
            last_terminal_status: None,
            last_reset_reason: None,
            recent_transitions: Vec::new(),
            blackboard: BTreeMap::new(),
            error: None,
        };
        let mut state = BehaviorTreeDebugState::default();
        state.sync((2, 5), None, &snapshot);
        state.invalidated = true;
        snapshot.tree_generation = 2;

        state.sync((2, 5), None, &snapshot);

        assert_eq!(state.source_key.as_ref().unwrap().tree_generation, 2);
        assert!(!state.invalidated);
        assert!(state.graph_session.is_none());
    }
}
