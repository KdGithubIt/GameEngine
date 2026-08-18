//! Deterministic Editor states used only by the ADR visual-validation workflow.
//!
//! These fixtures configure normal Editor presentation state and then render through the
//! existing production widgets. They never participate in persisted authoring formats or
//! normal Editor startup because the module is compiled only by the `visual-validation`
//! feature and additionally requires `GAMEENGINE_VISUAL_SCENARIO`.

use super::*;
use crate::canvas::{
    show_graph_debug_canvas, GraphDebugBadge, GraphDebugNodePresentation, GraphDebugOverlay,
};
use crate::session::AnimationNodeInsertKind;
use engine_authoring::{Diagnostic, DiagnosticTarget, Vec2 as GraphVec2};

pub(super) fn requested_adr_visual_scenario() -> Option<String> {
    std::env::var("GAMEENGINE_VISUAL_SCENARIO")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

impl EditorApp {
    pub(super) fn prepare_adr_visual_scenario(&mut self) {
        let Some(scenario) = requested_adr_visual_scenario() else {
            return;
        };

        match scenario.as_str() {
            "adr0136-animation-preview" => {
                self.animation_preview.prepare_adr_visual_validation();
            }
            "adr0137-diagnostics" => {
                self.prepare_diagnostic_visual_fixture();
            }
            "adr0138-zero-targets" => {
                self.session.reset(EditorSession::empty_behavior_tree());
            }
            "adr0138-behavior-only" => {
                if let Ok(session) = EditorSession::behavior_tree_example() {
                    self.session.reset(session);
                    self.canvas.request_frame_all();
                    self.property_node = self.session.graph().nodes.keys().next().cloned();
                }
            }
            "adr0138-animation-only"
            | "adr0138-mixed-targets"
            | "adr0138-transition-progress"
            | "adr0138-stale-source"
            | "adr0138-entry-state-palette" => {
                self.prepare_animation_graph_visual_fixture(false);
            }
            "adr0138-long-names" => {
                self.prepare_animation_graph_visual_fixture(true);
            }
            "adr0139-working-copy-conflict" => {
                self.prepare_animation_graph_visual_fixture(true);
            }
            _ => {}
        }
    }

    fn prepare_diagnostic_visual_fixture(&mut self) {
        let entity = EntityId::generate();
        let related = EntityId::generate();
        self.problems_panel.set_problems(vec![
            Diagnostic::error(
                "editor.scene_view.components_skipped",
                "Animation Controller cannot resolve every graph Motion Slot from the assigned Animation Set.",
            )
            .with_target(DiagnosticTarget::Entity { id: entity.clone() })
            .with_related_targets([DiagnosticTarget::Entity { id: related }]),
            Diagnostic::warning(
                "anim.humanoid_profile_incomplete",
                "Humanoid adaptation is missing required ancestry for this imported skeleton.",
            )
            .with_target(DiagnosticTarget::Entity { id: entity.clone() }),
            Diagnostic::warning(
                "anim.humanoid_profile_incomplete",
                "A second imported motion reports the same skeleton compatibility problem.",
            )
            .with_target(DiagnosticTarget::Entity { id: entity }),
            Diagnostic::info(
                "editor.animation_preview.ready",
                "Animation Preview is using the authoritative working-copy graph and Animation Set.",
            ),
        ]);
        self.bottom_panel_open = true;
        self.bottom_panel_tab = BottomPanelTab::Problems;
    }

    fn prepare_animation_graph_visual_fixture(&mut self, long_names: bool) {
        let mut session = EditorSession::empty_animation_graph();
        let entry = session.graph().nodes.keys().next().cloned();
        let idle = session
            .add_animation_node(
                AnimationNodeInsertKind::State,
                Some(GraphVec2::new(250.0, 70.0)),
            )
            .ok();
        let run = session
            .add_animation_node(
                AnimationNodeInsertKind::State,
                Some(GraphVec2::new(520.0, 70.0)),
            )
            .ok();

        if let Some(entry) = entry.as_ref() {
            let _ = session.set_node_name(entry.clone(), "Entry");
        }
        if let Some(idle) = idle.as_ref() {
            let name = if long_names {
                "Locomotion Idle With An Intentionally Long Author-Facing State Name"
            } else {
                "Idle"
            };
            let _ = session.set_node_name(idle.clone(), name);
        }
        if let Some(run) = run.as_ref() {
            let name = if long_names {
                "Sprint Forward While Carrying A Long Runtime Debug Display Name"
            } else {
                "Run"
            };
            let _ = session.set_node_name(run.clone(), name);
        }

        let motion_slot = session
            .add_motion_slot("Locomotion / Base Motion")
            .ok();
        if let (Some(idle), Some(slot)) = (idle.as_ref(), motion_slot.as_ref()) {
            let _ = session.set_animation_state_motion_slot(idle.clone(), Some(slot.clone()));
        }
        if let (Some(run), Some(slot)) = (run.as_ref(), motion_slot.as_ref()) {
            let _ = session.set_animation_state_motion_slot(run.clone(), Some(slot.clone()));
        }
        if let (Some(entry), Some(idle)) = (entry.as_ref(), idle.as_ref()) {
            let _ = session.connect_animation_transition(entry.clone(), idle.clone());
        }
        if let (Some(idle), Some(run)) = (idle.as_ref(), run.as_ref()) {
            let _ = session.connect_animation_transition(idle.clone(), run.clone());
        }

        self.property_node = run.or(idle).or(entry);
        self.session.reset(session);
        self.canvas.request_frame_all();
    }

    pub(super) fn show_adr_visual_scenario(&mut self, context: &egui::Context) {
        let Some(scenario) = requested_adr_visual_scenario() else {
            return;
        };

        if scenario.starts_with("adr0138-") {
            self.show_graph_debug_visual_fixture(context, &scenario);
        } else if scenario == "adr0139-working-copy-conflict" {
            self.show_working_copy_conflict_visual_fixture(context);
        }
    }

    fn show_graph_debug_visual_fixture(&mut self, context: &egui::Context, scenario: &str) {
        let nodes = self.session.graph().nodes.keys().cloned().collect::<Vec<_>>();
        let edges = self.session.graph().edges.keys().cloned().collect::<Vec<_>>();
        let mut overlay = GraphDebugOverlay::default();

        if scenario != "adr0138-stale-source" {
            if let Some(node) = nodes.get(1).or_else(|| nodes.first()) {
                overlay.nodes.insert(
                    node.clone(),
                    GraphDebugNodePresentation {
                        active: true,
                        badge: Some(GraphDebugBadge::Running),
                        elapsed_seconds: Some(1.734),
                        detail: Some("Active runtime graph node".to_owned()),
                    },
                );
            }
            if scenario == "adr0138-behavior-only" {
                if let Some(node) = nodes.get(2) {
                    overlay.nodes.insert(
                        node.clone(),
                        GraphDebugNodePresentation {
                            active: false,
                            badge: Some(GraphDebugBadge::Success),
                            elapsed_seconds: Some(0.416),
                            detail: Some("Most recently completed successfully".to_owned()),
                        },
                    );
                }
            }
            if scenario == "adr0138-transition-progress" {
                if let Some(edge) = edges.last() {
                    overlay.active_edges.insert(edge.clone());
                }
                if let Some(node) = nodes.last() {
                    overlay.nodes.insert(
                        node.clone(),
                        GraphDebugNodePresentation {
                            active: true,
                            badge: Some(GraphDebugBadge::Running),
                            elapsed_seconds: Some(0.821),
                            detail: Some("Transition destination · blend 63%".to_owned()),
                        },
                    );
                }
            }
        }

        let selected_text = match scenario {
            "adr0138-behavior-only" => "Enemy Captain · Behavior Tree",
            "adr0138-mixed-targets" => "Player Character · Animation Graph",
            _ => "Player Character · Animation Graph",
        };
        let stale = scenario == "adr0138-stale-source";
        let palette = scenario == "adr0138-entry-state-palette";

        egui::Window::new("Graph Debug Visual Validation")
            .id(egui::Id::new("adr0138_graph_debug_visual_fixture"))
            .collapsible(false)
            .resizable(false)
            .fixed_pos(egui::pos2(18.0, 76.0))
            .fixed_size(egui::vec2(1180.0, 760.0))
            .show(context, |ui| {
                control_row(ui, |ui| {
                    ui.strong("Graph Debug");
                    egui::ComboBox::from_id_salt("adr0138_target_selector")
                        .selected_text(if scenario == "adr0138-zero-targets" {
                            "No live graph target"
                        } else {
                            selected_text
                        })
                        .width(340.0)
                        .show_ui(ui, |ui| {
                            if scenario == "adr0138-zero-targets" {
                                ui.label("No live runtime graph execution target exists.");
                            } else {
                                ui.selectable_label(true, selected_text);
                                if scenario == "adr0138-mixed-targets" {
                                    ui.selectable_label(false, "Enemy Captain · Behavior Tree");
                                    ui.selectable_label(false, "Companion NPC · Animation Graph");
                                }
                            }
                        });
                    if ui.button("Frame All").clicked() {
                        self.canvas.request_frame_all();
                    }
                    ui.small("Read-only runtime observation");
                });
                ui.separator();

                if scenario == "adr0138-zero-targets" {
                    ui.vertical_centered(|ui| {
                        ui.add_space(180.0);
                        ui.heading("Graph Debug");
                        ui.label("No live runtime graph execution target exists.");
                        ui.small("Start Play with an Animation Graph or Behavior Tree runner to inspect execution.");
                    });
                    return;
                }

                if stale {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "Source changed after Play started. Runtime evidence is older than the current working copy; live overlays are suppressed.",
                    );
                    ui.separator();
                }

                if palette {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong("Add Node palette");
                        for kind in self.session.available_graph_node_kinds() {
                            ui.code(format!("{} / {}", kind.category(), kind.label()));
                        }
                        ui.small("Entry is already present and therefore remains unique; State stays available.");
                    });
                    ui.separator();
                }

                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(820.0);
                        ui.set_min_height(610.0);
                        show_graph_debug_canvas(
                            ui,
                            &self.session,
                            &mut self.canvas,
                            &overlay,
                            &mut self.property_node,
                        );
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(315.0);
                        ui.heading(if scenario == "adr0138-behavior-only" {
                            "Behavior Tree"
                        } else {
                            "Animation Graph"
                        });
                        ui.small("Stable source identity");
                        ui.monospace(self.session.graph().id.as_str());
                        ui.separator();
                        if scenario == "adr0138-transition-progress" {
                            ui.strong("Execution");
                            ui.label("Transition 63%");
                            ui.label("Idle → Run");
                            if let Some(edge) = edges.last() {
                                ui.small(format!("Edge {}", edge.as_str()));
                            }
                            ui.label("Clip time 0.821s");
                            ui.separator();
                            ui.strong("Motion resolution");
                            ui.label("Locomotion / Base Motion");
                            ui.label("Auto → Humanoid");
                        } else if scenario == "adr0138-behavior-only" {
                            ui.strong("Status: Running");
                            ui.label("Execution generation 4");
                            ui.label("Selected node runtime");
                            ui.label("State: Running");
                            ui.label("Elapsed: 1.734s");
                        } else if stale {
                            ui.strong("Runtime overlay suppressed");
                            ui.label("The source graph remains inspectable while stale runtime colors and badges are hidden.");
                        } else {
                            ui.strong("Execution");
                            ui.label("Current state: Run");
                            ui.label("Playback x1.00");
                            ui.separator();
                            ui.strong("Parameters");
                            ui.label("is_running    true");
                            ui.label("is_grounded   true");
                        }
                        ui.separator();
                        if let Some(node) = self.property_node.as_ref() {
                            ui.small("Selected source node");
                            ui.monospace(node.as_str());
                        }
                        ui.small("Graph Debug never mutates runtime parameters or source documents.");
                    });
                });
            });
    }

    fn show_working_copy_conflict_visual_fixture(&mut self, context: &egui::Context) {
        egui::Modal::new(egui::Id::new("adr0139_working_copy_conflict")).show(context, |ui| {
            ui.heading("File changed outside the Editor");
            ui.label(
                "The open document has unsaved working-copy changes, and its saved copy changed on disk.",
            );
            ui.monospace("assets/characters/player_locomotion.graph.json");
            ui.separator();
            ui.strong("Authoritative working copy is preserved");
            ui.label(
                "Reload is never automatic while this tab is dirty. Choose which copy should win before continuing.",
            );
            ui.separator();
            control_row(ui, |ui| {
                let _ = ui.button("Keep Working Copy");
                let _ = ui.button("Reload Disk Copy");
                let _ = ui.button("Cancel");
            });
        });
    }
}
