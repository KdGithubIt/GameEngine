//! Deterministic first-release visual fixtures for ADR 0136/0137/0138/0139.
//!
//! The module is compiled only by the `visual-validation` feature. It configures
//! ordinary Editor presentation state and draws deterministic evidence surfaces
//! on top of the production widgets without changing persisted authoring data.

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
            "adr0136-preview-pending"
            | "adr0136-preview-ready"
            | "adr0136-preview-failed" => {
                self.animation_preview.prepare_adr_visual_validation();
            }
            "adr0137-diagnostics" => {
                self.prepare_diagnostic_visual_fixture();
            }
            "adr0138-transition-progress" | "adr0138-stale-source" => {
                self.prepare_animation_graph_visual_fixture();
            }
            "adr0139-working-copy-conflict" => {
                self.prepare_animation_graph_visual_fixture();
            }
            _ => {}
        }
    }

    fn prepare_diagnostic_visual_fixture(&mut self) {
        let entity = self
            .session
            .scene()
            .and_then(|scene| scene.entities().next().map(|(id, _)| id.clone()))
            .unwrap_or_else(EntityId::generate);
        let related = self
            .session
            .scene()
            .and_then(|scene| {
                scene
                    .entities()
                    .map(|(id, _)| id.clone())
                    .find(|id| id != &entity)
            })
            .unwrap_or_else(|| entity.clone());

        self.selected_entity = Some(entity.clone());
        self.selected_entities.clear();
        self.selected_entities.insert(entity.clone());
        self.hierarchy_filter.clear();
        self.problems_panel.set_problems(vec![
            Diagnostic::error(
                "editor.scene_view.components_skipped",
                "Animation Controller cannot resolve every Motion Slot from the assigned Animation Set.",
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
            .with_target(DiagnosticTarget::Entity { id: entity.clone() }),
            Diagnostic::info(
                "editor.animation_preview.ready",
                "Animation Preview is using the authoritative working-copy graph and Animation Set.",
            )
            .with_target(DiagnosticTarget::Entity { id: entity }),
        ]);
        self.bottom_panel_open = true;
        self.bottom_panel_tab = BottomPanelTab::Problems;
    }

    fn prepare_animation_graph_visual_fixture(&mut self) {
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
            let _ = session.set_node_name(idle.clone(), "Locomotion Idle");
        }
        if let Some(run) = run.as_ref() {
            let _ = session.set_node_name(run.clone(), "Sprint Forward");
        }

        let motion_slot = session.add_motion_slot("Locomotion / Base Motion").ok();
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

        match scenario.as_str() {
            "adr0136-preview-pending"
            | "adr0136-preview-ready"
            | "adr0136-preview-failed" => {
                self.show_preview_residency_visual_fixture(context, &scenario);
            }
            "adr0137-diagnostics" => {
                self.show_diagnostic_navigation_visual_fixture(context);
            }
            "adr0138-transition-progress" | "adr0138-stale-source" => {
                self.show_graph_debug_visual_fixture(context, &scenario);
            }
            "adr0139-working-copy-conflict" => {
                self.show_working_copy_conflict_visual_fixture(context);
            }
            _ => {}
        }
    }

    fn show_preview_residency_visual_fixture(
        &mut self,
        context: &egui::Context,
        scenario: &str,
    ) {
        let (state, color, headline, cpu, gpu, placeholder, detail) = match scenario {
            "adr0136-preview-pending" => (
                "Loading",
                egui::Color32::YELLOW,
                "Latest request is still streaming",
                "pending",
                "not resident",
                "Visible — last good frame is retained while the request resolves",
                "Request #42 / revision 118 · asynchronous decode has not completed.",
            ),
            "adr0136-preview-ready" => (
                "Ready",
                egui::Color32::LIGHT_GREEN,
                "Latest request is presentation-ready",
                "resident",
                "resident",
                "Hidden — the resolved preview is authoritative for revision 118",
                "Request #42 / revision 118 · CPU decode and GPU upload are complete.",
            ),
            _ => (
                "Failed",
                egui::Color32::LIGHT_RED,
                "Latest request failed without blocking the Editor",
                "not retained",
                "not resident",
                "Visible — failure placeholder replaces only the failed request",
                "Request #42 / revision 118 · malformed animation payload · retry is available.",
            ),
        };

        egui::Window::new("Animation Preview · Asset Residency")
            .id(egui::Id::new("adr0136_preview_residency_fixture"))
            .collapsible(false)
            .resizable(false)
            .fixed_pos(egui::pos2(900.0, 86.0))
            .fixed_size(egui::vec2(680.0, 430.0))
            .show(context, |ui| {
                ui.heading("Async preview lifecycle");
                ui.horizontal_wrapped(|ui| {
                    ui.strong("State");
                    ui.colored_label(color, state);
                    ui.separator();
                    ui.label(headline);
                });
                ui.separator();
                egui::Grid::new("adr0136_residency_grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.label("Asset");
                        ui.monospace("animations/locomotion/walk_cycle.animclip");
                        ui.end_row();
                        ui.label("Request identity");
                        ui.monospace("asset + preview-kind + revision 118");
                        ui.end_row();
                        ui.label("CPU residency");
                        ui.monospace(cpu);
                        ui.end_row();
                        ui.label("GPU residency");
                        ui.monospace(gpu);
                        ui.end_row();
                        ui.label("Placeholder");
                        ui.label(placeholder);
                        ui.end_row();
                    });
                ui.separator();
                ui.label(detail);
                ui.small(
                    "Older completions are ignored by latest-wins request identity; preview state is observable and never fabricated.",
                );
                if scenario == "adr0136-preview-failed" {
                    ui.separator();
                    ui.colored_label(
                        egui::Color32::LIGHT_RED,
                        "Preview failed: malformed animation payload",
                    );
                    ui.small("The Editor remains interactive and a new request can retry.");
                }
            });
    }

    fn show_diagnostic_navigation_visual_fixture(&self, context: &egui::Context) {
        egui::Window::new("Problems · Navigation Evidence")
            .id(egui::Id::new("adr0137_navigation_fixture"))
            .collapsible(false)
            .resizable(false)
            .fixed_pos(egui::pos2(1030.0, 82.0))
            .fixed_size(egui::vec2(500.0, 270.0))
            .show(context, |ui| {
                ui.heading("Progressive disclosure + navigation");
                ui.label("The selected Hierarchy row and Inspector target are the same entity referenced by the visible Problems rows.");
                ui.separator();
                if let Some(entity) = self.selected_entity.as_ref() {
                    ui.label("Navigation target");
                    ui.monospace(entity.as_str());
                }
                ui.label("Severity: Error / Warning / Info");
                ui.label("Grouping: repeated warning code + target collapses into one summary row");
                ui.label("Repair target: related entity remains an explicit action");
                ui.small("Scene View / Hierarchy / Inspector selection is preserved while Problems stays open.");
            });
    }

    fn show_graph_debug_visual_fixture(&mut self, context: &egui::Context, scenario: &str) {
        let nodes = self.session.graph().nodes.keys().cloned().collect::<Vec<_>>();
        let edges = self.session.graph().edges.keys().cloned().collect::<Vec<_>>();
        let stale = scenario == "adr0138-stale-source";
        let mut overlay = GraphDebugOverlay::default();

        if !stale {
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

        egui::Window::new("Graph Debug · Play Mode")
            .id(egui::Id::new("adr0138_graph_debug_visual_fixture"))
            .collapsible(false)
            .resizable(false)
            .fixed_pos(egui::pos2(18.0, 76.0))
            .fixed_size(egui::vec2(1520.0, 850.0))
            .show(context, |ui| {
                control_row(ui, |ui| {
                    ui.strong("Graph Debug");
                    egui::ComboBox::from_id_salt("adr0138_target_selector")
                        .selected_text("Player Character · Animation Graph")
                        .width(340.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_label(true, "Player Character · Animation Graph");
                            ui.selectable_label(false, "Enemy Captain · Behavior Tree");
                        });
                    if ui.button("Frame All").clicked() {
                        self.canvas.request_frame_all();
                    }
                    ui.small("Read-only runtime observation");
                });
                ui.separator();

                if stale {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "STALE: source changed after Play started. Runtime evidence is older than the current working copy; live overlays are suppressed.",
                    );
                    ui.small("Play source revision 12 · working-copy revision 13 (dirty) · stable graph identity retained.");
                    ui.separator();
                } else {
                    ui.colored_label(
                        egui::Color32::LIGHT_GREEN,
                        "LIVE: exact Play source revision 12 matches working-copy revision 12.",
                    );
                    ui.small("Active transition provenance is tied to the selected runtime entity and stable graph identity.");
                    ui.separator();
                }

                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(1090.0);
                        ui.set_min_height(700.0);
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
                        ui.set_width(360.0);
                        ui.heading("Animation Graph");
                        ui.small("Stable source identity");
                        ui.monospace(self.session.graph().id.as_str());
                        ui.separator();
                        if stale {
                            ui.strong("Runtime overlay suppressed");
                            ui.label("Active colors, badges, and transition progress are hidden because the source is stale.");
                            ui.label("The source graph remains inspectable.");
                        } else {
                            ui.strong("Execution");
                            ui.label("Current state: Sprint Forward");
                            ui.label("Transition: Locomotion Idle → Sprint Forward");
                            ui.label("Transition progress: 63%");
                            ui.label("Clip time: 0.821s");
                            ui.separator();
                            ui.strong("Motion resolution");
                            ui.label("Locomotion / Base Motion");
                            ui.label("Auto → Humanoid");
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
        egui::Window::new("Document Coherency")
            .id(egui::Id::new("adr0139_document_status_fixture"))
            .collapsible(false)
            .resizable(false)
            .fixed_pos(egui::pos2(26.0, 82.0))
            .fixed_size(egui::vec2(520.0, 260.0))
            .show(context, |ui| {
                ui.heading("Authoritative working copy");
                ui.label("Working copy revision 18 · DIRTY");
                ui.label("Saved copy revision 17");
                ui.label("Disk revision 19 · changed externally");
                ui.separator();
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Conflict: automatic reload is blocked while the working copy is dirty.",
                );
                ui.small("No silent overwrite · explicit user choice is required.");
            });

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
