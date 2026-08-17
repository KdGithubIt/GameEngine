//! Play-mode runtime workspace, game-view input forwarding, and frame capture.

use super::*;
use crate::view_resolution::{
    clamp_render_target_size_in_pixels, render_target_size_in_pixels,
};

impl EditorApp {
    pub(super) fn show_runtime_debugger(&mut self, ui: &mut egui::Ui) {
        let Some(runtime) = self.runtime_state.as_ref() else {
            ui.heading("Runtime Debugger");
            ui.label("Start Play to inspect the runtime world and profiler.");
            return;
        };
        let performance = runtime.performance_snapshot();
        let entities = runtime.entity_debug_snapshot();
        control_row(ui, |ui| {
            ui.strong("Profiler");
            ui.label(format!("Last {:.2} ms", performance.last_tick_ms));
            ui.label(format!("Average {:.2} ms", performance.average_tick_ms));
            ui.label(format!("Max {:.2} ms", performance.maximum_tick_ms));
            ui.label(format!("Entities {}", performance.entity_count));
            ui.label(format!("Fixed steps {}", performance.fixed_steps));
        });
        ui.separator();
        // Pair the row list with a live value pane so paused frames can be
        // inspected without adding logging to game code.
        let selected_values = self
            .selected_runtime_entity
            .and_then(|key| {
                entities
                    .iter()
                    .find(|row| (row.entity.id(), row.entity.generation()) == key)
            })
            .map(|row| {
                (
                    row.name.clone(),
                    row.authoring_id.clone(),
                    runtime.entity_component_values(row.entity),
                )
            });
        let mut link_selection: Option<EntityId> = None;
        ui.columns(2, |columns| {
            egui::ScrollArea::vertical()
                .id_salt("runtime_entity_rows")
                .show(&mut columns[0], |ui| {
                    egui::Grid::new("runtime_entity_debugger")
                        .num_columns(4)
                        .striped(true)
                        .show(ui, |ui| {
                            ui.strong("Runtime ID");
                            ui.strong("Name");
                            ui.strong("Authoring ID");
                            ui.strong("Components");
                            ui.end_row();
                            for entity in &entities {
                                let key = (entity.entity.id(), entity.entity.generation());
                                if ui
                                    .selectable_label(
                                        self.selected_runtime_entity == Some(key),
                                        entity.entity.to_string(),
                                    )
                                    .clicked()
                                {
                                    self.selected_runtime_entity = Some(key);
                                    if let Some(authoring_id) = &entity.authoring_id {
                                        let stable = StableId::new(authoring_id);
                                        if let Ok(id) = EntityId::from_stable_id(stable) {
                                            link_selection = Some(id);
                                        }
                                    }
                                }
                                ui.label(&entity.name);
                                ui.monospace(
                                    entity.authoring_id.clone().unwrap_or_else(|| "—".into()),
                                );
                                ui.label(entity.components.join(", "));
                                ui.end_row();
                            }
                        });
                });
            let ui = &mut columns[1];
            match &selected_values {
                Some((name, authoring_id, values)) => {
                    ui.strong(format!("Values: {name}"));
                    if let Some(authoring_id) = authoring_id {
                        ui.monospace(authoring_id);
                    }
                    if values.is_empty() {
                        ui.label("No readable component values");
                    }
                    egui::Grid::new("runtime_entity_values")
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            for (label, value) in values {
                                ui.label(label);
                                ui.monospace(value);
                                ui.end_row();
                            }
                        });
                    ui.small("Pause / Step in the toolbar to inspect a single frame.");
                }
                None => {
                    ui.label("Select a runtime entity to inspect its values.");
                }
            }
        });
        if let Some(id) = link_selection {
            // Selecting a runtime row also highlights the matching authoring
            // entity so the Hierarchy and Inspector follow along.
            self.select_single_entity(Some(id));
        }
    }

    pub(super) fn show_runtime_workspace(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let Some(render_state) = frame.wgpu_render_state() else {
            ui.label("WGPU render state unavailable");
            return;
        };

        control_row(ui, |ui| {
            ui.strong("View");
            let previous = self.preferences.play_mode_view;
            if ui
                .selectable_label(
                    !self.behavior_debug.visible && previous == PlayModeView::Game,
                    "Game View",
                )
                .clicked()
            {
                self.behavior_debug.visible = false;
                self.preferences.play_mode_view = PlayModeView::Game;
            }
            if ui
                .selectable_label(
                    !self.behavior_debug.visible && previous == PlayModeView::Scene,
                    "Scene View",
                )
                .clicked()
            {
                self.behavior_debug.visible = false;
                self.preferences.play_mode_view = PlayModeView::Scene;
            }
            if ui
                .selectable_label(self.behavior_debug.visible, "Behavior Tree")
                .clicked()
            {
                self.behavior_debug.visible = true;
            }
            if self.preferences.play_mode_view != previous {
                self.preferences.save();
            }
            if !self.behavior_debug.visible
                && self.preferences.play_mode_view == PlayModeView::Scene
            {
                ui.separator();
                if ui.button("Reset Camera").clicked() {
                    self.scene_view.reset_camera();
                }
                ui.small("Right-drag orbit/fly, middle-drag pan, wheel zoom");
            }
        });
        ui.separator();
        if self.behavior_debug.visible {
            if self.game_view_focused {
                self.release_all_forwarded_input();
                self.game_view_focused = false;
            }
            self.show_behavior_tree_debug_workspace(ui);
            return;
        }

        // The image below fills all remaining panel space, so any widget
        // added after it would clip off the bottom edge.
        control_row(ui, |ui| {
            let recording = self
                .runtime_state
                .as_ref()
                .is_some_and(RuntimePlayState::is_replay_recording);
            let replaying = self
                .runtime_state
                .as_ref()
                .is_some_and(RuntimePlayState::is_replaying);
            if ui
                .add_enabled(!recording && !replaying, egui::Button::new("Record"))
                .clicked()
                && let Some(runtime) = &mut self.runtime_state
                    && let Err(error) = runtime.start_replay_recording() {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.replay.record_failed",
                                error.to_string(),
                            ));
                    }
            if ui
                .add_enabled(recording, egui::Button::new("Stop Recording"))
                .clicked()
                && let Some(replay) = self
                    .runtime_state
                    .as_mut()
                    .and_then(RuntimePlayState::stop_replay_recording)
                {
                    self.last_replay = Some(replay);
                }
            if ui
                .add_enabled(
                    self.last_replay.is_some() && !recording,
                    egui::Button::new(if replaying { "Replaying..." } else { "Replay" }),
                )
                .clicked()
                && let (Some(runtime), Some(replay)) =
                    (&mut self.runtime_state, self.last_replay.clone())
                    && let Err(error) = runtime.start_replay(replay) {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.replay.start_failed",
                                error.to_string(),
                            ));
                    }
            if ui.button("Load Replay...").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Engine Replay", &["json"])
                    .pick_file()
                {
                    match fs::read_to_string(&path)
                        .map_err(|error| error.to_string())
                        .and_then(|json| {
                            engine::InputReplay::from_json(&json).map_err(|error| error.to_string())
                        }) {
                        Ok(replay) => self.last_replay = Some(replay),
                        Err(error) => {
                            self.session
                                .push_diagnostic(engine_authoring::Diagnostic::error(
                                    "editor.replay.load_failed",
                                    format!("{}: {error}", path.display()),
                                ))
                        }
                    }
                }
            if ui
                .add_enabled(
                    self.last_replay.is_some(),
                    egui::Button::new("Save Replay..."),
                )
                .clicked()
            {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("Engine Replay", &["json"])
                    .set_file_name("input.replay.json");
                if let Some(project) = &self.project_root {
                    dialog = dialog.set_directory(project.assets_root().join("replays"));
                }
                if let (Some(path), Some(replay)) = (dialog.save_file(), &self.last_replay) {
                    let result = replay
                        .to_json()
                        .map_err(|error| error.to_string())
                        .and_then(|json| {
                            if let Some(parent) = path.parent() {
                                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                            }
                            engine_authoring::replace_file_contents(&path, &json)
                                .map_err(|error| error.to_string())
                        });
                    if let Err(error) = result {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.replay.save_failed",
                                format!("{}: {error}", path.display()),
                            ));
                    }
                }
            }
            ui.separator();
            if ui.button("Capture Frame").clicked() {
                let capture_result = self
                    .runtime_state
                    .as_ref()
                    .map(|runtime| runtime.capture_game_view(render_state));
                match capture_result {
                    Some(Ok(capture)) => self.save_frame_capture(capture),
                    Some(Err(error)) => {
                        self.session
                            .push_diagnostic(engine_authoring::Diagnostic::error(
                                "editor.runtime.capture_save_failed",
                                format!("Game View capture failed: {error}"),
                            ))
                    }
                    None => {}
                }
            }
            if ui
                .checkbox(&mut self.show_debug_lines, "Debug Draw")
                .changed()
                && let Some(runtime) = &mut self.runtime_state {
                    runtime.set_debug_draw_enabled(self.show_debug_lines);
                }
            ui.separator();
            egui::ComboBox::from_id_salt("game_view_aspect")
                .selected_text(self.game_view_aspect.label())
                .show_ui(ui, |ui| {
                    for preset in ViewAspect::ALL {
                        if ui
                            .selectable_label(self.game_view_aspect == preset, preset.label())
                            .clicked()
                        {
                            self.game_view_aspect = preset;
                        }
                    }
                })
                .response
                .on_hover_text(
                    "Constrain the Game View to a target screen; 1920x1080 renders at that resolution and is scaled into the panel, UI included",
                );
            if let Some(runtime) = &self.runtime_state {
                let collision = runtime.collision_stats();
                ui.separator();
                ui.label(format!(
                    "{} entities | {} frames | {} fixed | {:.1}s | collision {}/{}/{}",
                    runtime.entity_count(),
                    runtime.ticks(),
                    runtime.fixed_step_count(),
                    runtime.elapsed_seconds(),
                    collision.proxy_count,
                    collision.candidate_pair_count,
                    collision.contact_count
                ));
            }
        });
        ui.separator();

        if self.preferences.play_mode_view == PlayModeView::Scene {
            if self.game_view_focused {
                self.release_all_forwarded_input();
                self.game_view_focused = false;
            }
            let output = match self.runtime_state.as_mut() {
                Some(runtime) => self.scene_view.show_play(ui, runtime, render_state),
                None => return,
            };
            if let Some(entity) = output.picked_entity {
                self.select_single_entity(Some(entity.clone()));
                if let Some(runtime_entity) = self.runtime_state.as_ref().and_then(|runtime| {
                    runtime.entity_debug_snapshot().into_iter().find_map(|row| {
                        (row.authoring_id.as_deref() == Some(entity.as_str()))
                            .then_some((row.entity.id(), row.entity.generation()))
                    })
                }) {
                    self.selected_runtime_entity = Some(runtime_entity);
                }
            }
            if let Some(error) = output.render_error {
                self.stop_play(Some(render_state));
                self.session.push_diagnostic(
                    RuntimeDiagnosticKind::RenderError.to_diagnostic(format!(
                        "Play Scene View render failed: {error}"
                    )),
                );
            }
            return;
        }

        // Stuck-key fix: release all forwarded keys when the OS window loses
        // focus so game entities don't keep moving after Alt+Tab.
        let window_focused = ui.ctx().input(|i| i.focused);
        if !window_focused && self.game_view_focused {
            self.release_all_forwarded_input();
            self.game_view_focused = false;
        }

        // Forward keyboard input when the Game View has focus.
        if self.game_view_focused {
            self.forward_game_view_keys(ui);
            self.forward_game_view_mouse_buttons(ui);
        }

        let available = ui.available_size();
        let dimensions = game_view_dimensions(
            self.game_view_aspect,
            available,
            ui.ctx().pixels_per_point(),
            render_state.device.limits().max_texture_dimension_2d,
        );
        let mut game_view_rect: Option<egui::Rect> = None;
        let render_error = if let Some(runtime) = &mut self.runtime_state {
            match runtime.render_game_view(render_state, dimensions.render_size) {
                Ok(texture_id) => {
                    let image = egui::Image::from_texture(egui::load::SizedTexture::new(
                        texture_id,
                        dimensions.display_size,
                    ))
                    .fit_to_exact_size(dimensions.display_size)
                    .maintain_aspect_ratio(false)
                    .sense(egui::Sense::click());
                    let response = if self.game_view_aspect == ViewAspect::Free {
                        ui.add(image)
                    } else {
                        // Letterbox: center the constrained image inside the
                        // panel instead of stretching to fill it.
                        ui.vertical_centered(|ui| ui.add(image)).inner
                    };
                    if response.clicked() {
                        self.game_view_focused = true;
                    }
                    let clicked_outside = ui.ctx().input(|input| {
                        input.pointer.any_pressed()
                            && input
                                .pointer
                                .interact_pos()
                                .is_some_and(|position| !response.rect.contains(position))
                    });
                    if clicked_outside && self.game_view_focused {
                        self.release_all_forwarded_input();
                        self.game_view_focused = false;
                    }
                    game_view_rect = Some(response.rect);
                    None
                }
                Err(error) => Some(error.to_string()),
            }
        } else {
            None
        };

        // Runtime UI stays in logical target-screen units even when the 3D
        // texture uses more physical pixels on a high-DPI display. Mixing the
        // two would make HUD size depend on the editor monitor scale (ADR 0090).
        if let (Some(runtime), Some(rect)) = (&mut self.runtime_state, game_view_rect) {
            runtime.run_ui_systems(
                ui.ctx(),
                engine::UiViewport::scaled(rect, dimensions.ui_screen_size),
            );
        }

        if let Some(error) = render_error {
            self.stop_play(Some(render_state));
            self.session.push_diagnostic(
                RuntimeDiagnosticKind::RenderError
                    .to_diagnostic(format!("Game View render failed: {error}")),
            );
        }
    }

    /// Translates every Project Settings keyboard binding supported by egui to
    /// runtime `InputCommand`s and queues only state transitions.
    ///
    /// Escape remains reserved for the editor's Stop Play shortcut. All other
    /// key names accepted by `InputActionMap::from_project_settings` are
    /// forwarded while the Game View owns focus.
    fn forward_game_view_keys(&mut self, ui: &mut egui::Ui) {
        const GAME_KEYS: &[(egui::Key, KeyCode)] = &[
            (egui::Key::A, KeyCode::KeyA),
            (egui::Key::B, KeyCode::KeyB),
            (egui::Key::C, KeyCode::KeyC),
            (egui::Key::D, KeyCode::KeyD),
            (egui::Key::E, KeyCode::KeyE),
            (egui::Key::F, KeyCode::KeyF),
            (egui::Key::G, KeyCode::KeyG),
            (egui::Key::H, KeyCode::KeyH),
            (egui::Key::I, KeyCode::KeyI),
            (egui::Key::J, KeyCode::KeyJ),
            (egui::Key::K, KeyCode::KeyK),
            (egui::Key::L, KeyCode::KeyL),
            (egui::Key::M, KeyCode::KeyM),
            (egui::Key::N, KeyCode::KeyN),
            (egui::Key::O, KeyCode::KeyO),
            (egui::Key::P, KeyCode::KeyP),
            (egui::Key::Q, KeyCode::KeyQ),
            (egui::Key::R, KeyCode::KeyR),
            (egui::Key::S, KeyCode::KeyS),
            (egui::Key::T, KeyCode::KeyT),
            (egui::Key::U, KeyCode::KeyU),
            (egui::Key::V, KeyCode::KeyV),
            (egui::Key::W, KeyCode::KeyW),
            (egui::Key::X, KeyCode::KeyX),
            (egui::Key::Y, KeyCode::KeyY),
            (egui::Key::Z, KeyCode::KeyZ),
            (egui::Key::Num0, KeyCode::Digit0),
            (egui::Key::Num1, KeyCode::Digit1),
            (egui::Key::Num2, KeyCode::Digit2),
            (egui::Key::Num3, KeyCode::Digit3),
            (egui::Key::Num4, KeyCode::Digit4),
            (egui::Key::Num5, KeyCode::Digit5),
            (egui::Key::Num6, KeyCode::Digit6),
            (egui::Key::Num7, KeyCode::Digit7),
            (egui::Key::Num8, KeyCode::Digit8),
            (egui::Key::Num9, KeyCode::Digit9),
            (egui::Key::ArrowUp, KeyCode::ArrowUp),
            (egui::Key::ArrowDown, KeyCode::ArrowDown),
            (egui::Key::ArrowLeft, KeyCode::ArrowLeft),
            (egui::Key::ArrowRight, KeyCode::ArrowRight),
            (egui::Key::Space, KeyCode::Space),
            (egui::Key::Enter, KeyCode::Enter),
            (egui::Key::Tab, KeyCode::Tab),
        ];

        let Some(runtime) = &mut self.runtime_state else {
            return;
        };

        for &(egui_key, engine_key) in GAME_KEYS {
            let held = ui.input(|i| i.key_down(egui_key));
            forward_key_state(runtime, &mut self.forwarded_keys, engine_key, held);
        }

        // egui exposes Shift and Control as a combined modifier state instead of
        // as `Key` variants, so the physical side is not observable here. The
        // left variants stand in for either key; bindings that name the right
        // variants only resolve in a standalone player window.
        let modifiers = ui.input(|i| i.modifiers);
        for (engine_key, held) in [
            (KeyCode::ShiftLeft, modifiers.shift),
            (KeyCode::ControlLeft, modifiers.ctrl),
        ] {
            forward_key_state(runtime, &mut self.forwarded_keys, engine_key, held);
        }
    }

    /// Translates pointer button state while the Game View owns focus.
    fn forward_game_view_mouse_buttons(&mut self, ui: &mut egui::Ui) {
        const GAME_BUTTONS: &[(egui::PointerButton, engine::MouseButton)] = &[
            (egui::PointerButton::Primary, engine::MouseButton::Left),
            (egui::PointerButton::Secondary, engine::MouseButton::Right),
            (egui::PointerButton::Middle, engine::MouseButton::Middle),
        ];

        let Some(runtime) = &mut self.runtime_state else {
            return;
        };
        for &(egui_button, engine_button) in GAME_BUTTONS {
            let held = ui.input(|input| input.pointer.button_down(egui_button));
            let was_forwarded = self.forwarded_mouse_buttons.contains(&engine_button);
            if held != was_forwarded {
                runtime.queue_input(
                    InputSource::Human,
                    InputCommand::MouseButton {
                        button: engine_button,
                        pressed: held,
                    },
                );
                if held {
                    self.forwarded_mouse_buttons.insert(engine_button);
                } else {
                    self.forwarded_mouse_buttons.remove(&engine_button);
                }
            }
        }
    }

    /// Releases all input owned by the embedded runtime surface.
    fn release_all_forwarded_input(&mut self) {
        let Some(runtime) = &mut self.runtime_state else {
            self.forwarded_keys.clear();
            self.forwarded_mouse_buttons.clear();
            return;
        };
        // One runtime-owned command also releases gamepad axes and transient
        // mouse movement, keeping every adapter on the same focus contract.
        runtime.release_all_input();
        self.forwarded_keys.clear();
        self.forwarded_mouse_buttons.clear();
    }

    fn save_frame_capture(&mut self, capture: FrameCapture) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PNG image", &["png"])
            .set_file_name("capture.png")
            .save_file()
        else {
            return;
        };

        match save_frame_capture_png(&capture, &path) {
            Ok(()) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::info(
                        "editor.runtime.frame_captured",
                        format!(
                            "captured Game View frame to {}: {}x{} ({} rgba8 bytes)",
                            path.display(),
                            capture.width,
                            capture.height,
                            capture.rgba8.len()
                        ),
                    ));
            }
            Err(error) => {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::error(
                        "editor.runtime.capture_save_failed",
                        format!(
                            "failed to save Game View capture to {}: {error}",
                            path.display()
                        ),
                    ));
            }
        }
    }

    pub(super) fn start_play(&mut self) {
        let has_game_project = self
            .project_root
            .as_ref()
            .is_some_and(|project| project.game_dir().join("Cargo.toml").is_file());
        if has_game_project && self.game_code_is_stale() {
            if self.game_build.state() != GameBuildState::Idle {
                self.session
                    .push_diagnostic(engine_authoring::Diagnostic::warning(
                        "editor.play_waiting_for_game_build",
                        "wait for the current game-code build to finish before Play",
                    ));
                return;
            }
            self.play_after_game_build = true;
            if !self.start_game_build(GameBuildKind::Build) {
                self.play_after_game_build = false;
            }
            return;
        }
        self.start_play_ready();
    }

    pub(super) fn start_play_ready(&mut self) {
        self.session.set_diagnostics(Vec::new());
        // The Console was just cleared, so let the following refresh re-log
        // every still-present problem into the fresh Play log.
        self.mirrored_problem_keys.clear();
        self.refresh_scene_problems();
        let result = match self.session.scene() {
            Some(scene) => RuntimePlayState::start_from_document_with_game_module(
                scene,
                self.project_root.as_ref(),
                self.session.current_document_path(),
                self.game_module.as_ref().map(Arc::clone),
            ),
            None => Err(PlayError::NoScene),
        };

        match result {
            Ok(start) => {
                self.session.extend_diagnostics(start.diagnostics);
                self.runtime_state = Some(start.state);
                self.behavior_debug.clear();
                self.selected_runtime_entity = None;
                self.editor_mode = EditorMode::Playing;
                self.game_view_focused = true;
            }
            Err(error) => {
                self.session.extend_diagnostics(error.into_diagnostics());
            }
        }
    }

    pub(super) fn reload_scene(&mut self, render_state: Option<&egui_wgpu::RenderState>) {
        let Some(path) = self
            .session
            .current_document_path()
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        match RuntimePlayState::reload_from_path_with_game_module(
            &path,
            self.project_root.as_ref(),
            self.game_module.as_ref().map(Arc::clone),
        ) {
            Ok(start) => {
                if let Some(runtime) = &mut self.runtime_state {
                    match render_state {
                        Some(rs) => runtime.release_game_view(rs),
                        None => self
                            .orphaned_game_view_textures
                            .extend(runtime.take_view_textures()),
                    }
                }
                self.session.extend_diagnostics(start.diagnostics);
                self.runtime_state = Some(start.state);
                self.behavior_debug.clear_observation();
                self.selected_runtime_entity = None;
            }
            Err(error) => {
                self.session.extend_diagnostics(error.into_diagnostics());
            }
        }
    }

    pub(super) fn stop_play(&mut self, render_state: Option<&egui_wgpu::RenderState>) {
        if let Some(runtime) = &mut self.runtime_state {
            match render_state {
                Some(render_state) => runtime.release_game_view(render_state),
                // The egui registration cannot be freed without a render
                // state; park it so the next frame that has one releases it.
                None => self
                    .orphaned_game_view_textures
                    .extend(runtime.take_view_textures()),
            }
        }
        self.runtime_state = None;
        self.behavior_debug.clear();
        self.selected_runtime_entity = None;
        self.editor_mode = EditorMode::Edit;
        self.game_view_focused = false;
        self.forwarded_keys.clear();
        self.forwarded_mouse_buttons.clear();
        self.show_debug_lines = true;
        if self.game_build_requested_after_edit && self.game_build.state() == GameBuildState::Idle {
            self.game_build_quiet_deadline = Some(std::time::Instant::now());
        }
    }

    /// Executes one AI Studio managed runtime action through the normal Editor Play path.
    pub fn handle_ai_studio_runtime_action(
        &mut self,
        action: crate::ai_studio::AiStudioRuntimeAction,
        render_state: Option<&egui_wgpu::RenderState>,
    ) -> crate::ai_studio::AiStudioRuntimeResult {
        use crate::ai_studio::{AiStudioRuntimeAction, AiStudioRuntimeResult};
        match action {
            AiStudioRuntimeAction::StartPlaytest => {
                if !self.is_playing() { self.start_play(); }
                if self.is_playing() {
                    AiStudioRuntimeResult::PlayStarted
                } else if self.play_after_game_build {
                    AiStudioRuntimeResult::PlayStartPending
                } else {
                    AiStudioRuntimeResult::Failed("Editor Play could not start; inspect the current Editor diagnostics.".to_owned())
                }
            }
            AiStudioRuntimeAction::CaptureFrame => {
                let Some(render_state) = render_state else {
                    return AiStudioRuntimeResult::Failed("WGPU render state is unavailable for managed frame capture.".to_owned());
                };
                let Some(runtime) = self.runtime_state.as_ref() else {
                    return AiStudioRuntimeResult::Failed("Managed frame capture requires an active Editor Play session.".to_owned());
                };
                match runtime.capture_game_view(render_state) {
                    Ok(capture) => AiStudioRuntimeResult::FrameCaptured(capture),
                    Err(error) => AiStudioRuntimeResult::Failed(format!("Managed Game View capture failed: {error}")),
                }
            }
            AiStudioRuntimeAction::StopPlaytest => {
                self.stop_play(render_state);
                AiStudioRuntimeResult::PlayStopped
            }
        }
    }

    /// Returns whether the normal Editor Play runtime is currently active for AI Studio.
    pub fn ai_studio_playtest_running(&self) -> bool {
        self.is_playing()
    }

    pub(super) fn is_playing(&self) -> bool {
        self.editor_mode == EditorMode::Playing
    }

    pub(super) fn game_code_is_stale(&self) -> bool {
        let Some(project) = &self.project_root else {
            return false;
        };
        let Some(module) = &self.game_module else {
            return true;
        };
        let Ok(module_time) = fs::metadata(module.path()).and_then(|metadata| metadata.modified())
        else {
            return true;
        };
        newest_file_time(&project.rust_scripts_dir())
            .into_iter()
            .chain(newest_file_time(&project.game_dir().join("src")))
            .chain(
                fs::metadata(project.game_dir().join("Cargo.toml"))
                    .and_then(|metadata| metadata.modified())
                    .ok(),
            )
            .any(|modified| modified > module_time)
    }

    pub(super) fn finish_pending_game_package(&mut self, game_module: Option<&Path>) {
        let Some(pending) = self.pending_game_package.take() else {
            return;
        };
        let output_dir = pending.config.output_dir.clone();
        match package_project_with_game_module(
            &pending.config,
            &self.asset_manifest,
            &pending.player_binary,
            game_module,
        ) {
            Ok(plan) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::info(
                    "editor.package_succeeded",
                    format!(
                        "packaged {} project files and Rust game code to {}",
                        plan.copies.len() + 3,
                        output_dir.display()
                    ),
                )),
            Err(error) => self
                .session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.package_failed",
                    format!("package failed: {error}"),
                )),
        }
    }
}

pub(super) fn show_input_debugger(ui: &mut egui::Ui, snapshot: Option<&RuntimeInputDebugSnapshot>) {
    let Some(snapshot) = snapshot else {
        ui.heading("Input Debugger");
        ui.label("Start Play to inspect physical inputs and resolved project actions.");
        return;
    };

    ui.heading("Input Debugger");
    let pads = if snapshot.connected_gamepads.is_empty() {
        "none".to_owned()
    } else {
        snapshot
            .connected_gamepads
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    ui.label(format!(
        "Connected gamepads: {pads}  |  connection generation {}",
        snapshot.connection_generation
    ));
    if let Some((gamepad, connected)) = snapshot.last_connection_change {
        ui.label(format!(
            "Last device change: pad {gamepad} {}",
            if connected {
                "connected"
            } else {
                "disconnected"
            }
        ));
    }

    ui.separator();
    ui.label("Physical state");
    egui::Grid::new("input_debugger_physical")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            input_debug_row(ui, "Keyboard", &snapshot.keyboard);
            input_debug_row(ui, "Mouse buttons", &snapshot.mouse_buttons);
            input_debug_row(ui, "Gamepad buttons", &snapshot.gamepad_buttons);
            input_debug_row(ui, "Gamepad axes", &snapshot.gamepad_axes);
        });

    ui.separator();
    ui.label("Resolved actions");
    if snapshot.actions.is_empty() {
        ui.label("No input actions are declared in Project Settings.");
    } else {
        egui::Grid::new("input_debugger_actions")
            .num_columns(5)
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Action");
                ui.strong("State");
                ui.strong("Scalar");
                ui.strong("Vector X");
                ui.strong("Vector Y");
                ui.end_row();
                for (name, state) in &snapshot.actions {
                    ui.label(name);
                    let transition = if state.just_pressed {
                        "just pressed"
                    } else if state.just_released {
                        "just released"
                    } else if state.pressed {
                        "pressed"
                    } else {
                        "idle"
                    };
                    ui.label(transition);
                    ui.monospace(format!("{:.3}", state.scalar));
                    ui.monospace(format!("{:.3}", state.vector[0]));
                    ui.monospace(format!("{:.3}", state.vector[1]));
                    ui.end_row();
                }
            });
    }
}

pub(super) fn show_runtime_animation_debug(
    ui: &mut egui::Ui,
    snapshot: &RuntimeAnimationDebugSnapshot,
) {
    egui::CollapsingHeader::new("Runtime Animation")
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("runtime_animation_debug")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Playback");
                    ui.label(format!(
                        "{} at {:.3}s, {:.2}x{}",
                        snapshot.playback_state,
                        snapshot.clip_time,
                        snapshot.playback_speed,
                        if snapshot.looping { " (loop)" } else { "" }
                    ));
                    ui.end_row();
                    ui.label("Runtime clip");
                    ui.monospace(snapshot.clip_runtime_id.to_string());
                    ui.end_row();
                    ui.label("Crossfade");
                    ui.label(snapshot.crossfade_progress.map_or_else(
                        || "inactive".to_owned(),
                        |progress| format!("{:.0}%", progress * 100.0),
                    ));
                    ui.end_row();
                    ui.label("Root motion");
                    ui.label(format!(
                        "{}  [{:.3}, {:.3}, {:.3}]",
                        snapshot.root_motion_mode,
                        snapshot.root_motion_delta[0],
                        snapshot.root_motion_delta[1],
                        snapshot.root_motion_delta[2]
                    ));
                    ui.end_row();
                    if let Some(state) = &snapshot.graph_state {
                        ui.label("Graph state");
                        ui.label(state);
                        ui.end_row();
                        ui.label("Transitions");
                        ui.label(snapshot.graph_transition_sequence.to_string());
                        ui.end_row();
                    }
                    if let Some(transition) = &snapshot.graph_last_transition {
                        ui.label("Last transition");
                        ui.label(transition);
                        ui.end_row();
                    }
                });
            if !snapshot.graph_parameters.is_empty() {
                ui.label("Graph parameters");
                for (name, value) in &snapshot.graph_parameters {
                    ui.horizontal(|ui| {
                        ui.monospace(name);
                        ui.label(if *value { "true" } else { "false" });
                    });
                }
            }
        });
}

/// Queues a key command only when the held state differs from what the Game
/// View last forwarded, so the runtime sees edges instead of per-frame repeats.
fn forward_key_state(
    runtime: &mut RuntimePlayState,
    forwarded: &mut std::collections::HashSet<KeyCode>,
    key: KeyCode,
    held: bool,
) {
    if held == forwarded.contains(&key) {
        return;
    }
    runtime.queue_input(InputSource::Human, InputCommand::Key { key, pressed: held });
    if held {
        forwarded.insert(key);
    } else {
        forwarded.remove(&key);
    }
}

fn input_debug_row(ui: &mut egui::Ui, label: &str, values: &[String]) {
    ui.label(label);
    if values.is_empty() {
        ui.weak("none");
    } else {
        ui.monospace(values.join(", "));
    }
    ui.end_row();
}

pub(super) struct PendingGamePackage {
    pub(super) config: BuildConfig,
    pub(super) player_binary: PathBuf,
}

#[derive(Debug)]
enum FrameCaptureSaveError {
    Encode(png::EncodingError),
    Io(std::io::Error),
}

impl std::fmt::Display for FrameCaptureSaveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Encode(error) => write!(formatter, "PNG encode failed: {error}"),
            Self::Io(error) => write!(formatter, "file write failed: {error}"),
        }
    }
}

impl std::error::Error for FrameCaptureSaveError {}

fn save_frame_capture_png(
    capture: &FrameCapture,
    path: &Path,
) -> Result<(), FrameCaptureSaveError> {
    let png = encode_frame_png(capture).map_err(FrameCaptureSaveError::Encode)?;
    std::fs::write(path, png).map_err(FrameCaptureSaveError::Io)
}

pub(super) fn encode_frame_png(capture: &FrameCapture) -> Result<Vec<u8>, png::EncodingError> {
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, capture.width, capture.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);

        let mut writer = encoder.write_header()?;
        writer.write_image_data(&capture.rgba8)?;
    }
    Ok(png_bytes)
}

/// Logical presentation and physical render sizes for one Game View frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GameViewDimensions {
    /// GPU render-target extent in physical pixels.
    render_size: [u32; 2],
    /// Egui image extent in logical points.
    display_size: eframe::egui::Vec2,
    /// Logical target-screen extent used by runtime UI layout.
    ui_screen_size: eframe::egui::Vec2,
}

/// Calculates Game View sizes without conflating egui points and GPU pixels.
///
/// Dynamic presets rasterize their logical display rectangle at the current
/// physical pixel density. Fixed 1920x1080 remains an explicit GPU resolution
/// regardless of editor DPI scaling. Every physical target is uniformly
/// clamped to the device texture limit while UI keeps its logical target size.
fn game_view_dimensions(
    aspect: ViewAspect,
    available: eframe::egui::Vec2,
    pixels_per_point: f32,
    max_texture_dimension_2d: u32,
) -> GameViewDimensions {
    let fit = |ratio: f32| {
        let width = available.x.min(available.y * ratio).max(1.0);
        let height = (width / ratio).max(1.0);
        eframe::egui::vec2(width, height)
    };
    match aspect {
        // The only preset with a render-target size of its own: the image is
        // rendered at 1920x1080 and then scaled down to fit the panel.
        ViewAspect::Fixed1080 => GameViewDimensions {
            render_size: clamp_render_target_size_in_pixels(
                [1920, 1080],
                max_texture_dimension_2d,
            ),
            display_size: fit(16.0 / 9.0),
            ui_screen_size: eframe::egui::vec2(1920.0, 1080.0),
        },
        other => {
            let display = match other.ratio() {
                Some(ratio) => fit(ratio),
                None => eframe::egui::vec2(available.x.max(1.0), available.y.max(1.0)),
            };
            GameViewDimensions {
                render_size: render_target_size_in_pixels(
                    display,
                    pixels_per_point,
                    max_texture_dimension_2d,
                ),
                display_size: display,
                ui_screen_size: other.target_resolution().unwrap_or(display),
            }
        }
    }
}

#[cfg(test)]
mod game_view_dimension_tests {
    use super::*;

    #[test]
    fn free_game_view_separates_logical_ui_from_physical_render_size() {
        let available = egui::vec2(800.0, 480.0);
        let expected = [
            (1.0, [800, 480]),
            (1.25, [1000, 600]),
            (1.5, [1200, 720]),
            (2.0, [1600, 960]),
        ];

        for (pixels_per_point, render_size) in expected {
            let dimensions = game_view_dimensions(
                ViewAspect::Free,
                available,
                pixels_per_point,
                8192,
            );
            assert_eq!(dimensions.render_size, render_size);
            assert_eq!(dimensions.display_size, available);
            assert_eq!(dimensions.ui_screen_size, available);
        }
    }

    #[test]
    fn constrained_dynamic_game_view_keeps_its_logical_target_screen() {
        let dimensions =
            game_view_dimensions(ViewAspect::Wide16x9, egui::vec2(800.0, 600.0), 1.5, 8192);

        assert_eq!(dimensions.render_size, [1200, 675]);
        assert_eq!(dimensions.display_size, egui::vec2(800.0, 450.0));
        assert_eq!(dimensions.ui_screen_size, egui::vec2(1920.0, 1080.0));
    }

    #[test]
    fn fixed_1080_is_not_multiplied_by_editor_dpi() {
        let dimensions =
            game_view_dimensions(ViewAspect::Fixed1080, egui::vec2(800.0, 600.0), 2.0, 8192);

        assert_eq!(dimensions.render_size, [1920, 1080]);
        assert_eq!(dimensions.display_size, egui::vec2(800.0, 450.0));
        assert_eq!(dimensions.ui_screen_size, egui::vec2(1920.0, 1080.0));
    }

    #[test]
    fn fixed_1080_only_shrinks_when_the_gpu_limit_requires_it() {
        let dimensions =
            game_view_dimensions(ViewAspect::Fixed1080, egui::vec2(800.0, 600.0), 2.0, 1024);

        assert_eq!(dimensions.render_size, [1024, 576]);
        assert_eq!(dimensions.ui_screen_size, egui::vec2(1920.0, 1080.0));
    }
}
