//! Non-blocking Editor workflow for the shared production navigation bake service.

use crate::workspace::WorkspaceTabId;
use engine::navigation_bake::{
    bake_scene_navmesh, NavMeshBakeDocument, NavMeshBakeError, NavMeshBakeResult,
    NavigationBakeServiceError,
};
use engine::AssetManifest;
use engine_authoring::{
    replace_file_contents, AuthoringScene, ComponentTypeId, ProjectRoot,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;

fn scene_uses_navigation(scene: &AuthoringScene) -> bool {
    let surface = ComponentTypeId::new(engine::scene_bridge::NAV_MESH_SURFACE_COMPONENT);
    let agent = ComponentTypeId::new(engine::scene_bridge::NAV_MESH_AGENT_COMPONENT);
    scene.entities().any(|(_, entity)| {
        entity.components.contains_key(&surface) || entity.components.contains_key(&agent)
    })
}

pub(super) fn navigation_bake_document_path(
    project: &ProjectRoot,
    scene_path: Option<&Path>,
) -> Option<PathBuf> {
    let scene_stem = scene_path?
        .file_stem()?
        .to_string_lossy()
        .trim_end_matches(".scene")
        .to_owned();
    (!scene_stem.is_empty()).then(|| {
        project
            .assets_root()
            .join("navigation")
            .join(format!("{scene_stem}.navmesh.bake.json"))
    })
}

pub(super) fn require_current_navigation_artifact(
    scene: &AuthoringScene,
    project: &ProjectRoot,
    manifest: &AssetManifest,
    scene_path: Option<&Path>,
) -> Result<(), String> {
    if !scene_uses_navigation(scene) {
        return Ok(());
    }
    let document_path = navigation_bake_document_path(project, scene_path)
        .ok_or_else(|| "save the scene before baking navigation".to_owned())?;
    let document_json = std::fs::read_to_string(&document_path).map_err(|error| {
        format!(
            "navigation bake metadata is missing at {}: {error}; bake NavMesh before Play or packaging",
            document_path.display()
        )
    })?;
    let document = NavMeshBakeDocument::from_json(&document_json).map_err(|error| {
        format!(
            "navigation bake metadata at {} is invalid or from an unsupported format: {error}",
            document_path.display()
        )
    })?;
    let asset_path = project.assets_root().join(&document.output_asset);
    let nav_mesh = engine::navmesh::load_navmesh(&asset_path).map_err(|error| {
        format!(
            "navigation runtime asset is missing or invalid at {}: {error}",
            asset_path.display()
        )
    })?;
    let current = engine::navigation_bake::is_scene_navmesh_current(
        scene, project, manifest, &document, &nav_mesh,
    )
    .map_err(|error| format!("could not validate navigation bake currentness: {error}"))?;
    if current {
        Ok(())
    } else {
        Err("navigation bake is stale; rebuild NavMesh before Play or packaging".to_owned())
    }
}

pub(super) fn navigation_artifact_diagnostics(
    scene: &AuthoringScene,
    project: &ProjectRoot,
    manifest: &AssetManifest,
    scene_path: Option<&Path>,
) -> Vec<engine_authoring::Diagnostic> {
    require_current_navigation_artifact(scene, project, manifest, scene_path)
        .err()
        .map(|message| {
            vec![engine_authoring::Diagnostic::warning(
                "editor.navigation_bake_not_current",
                message,
            )]
        })
        .unwrap_or_default()
}

#[derive(Debug)]
pub(super) struct NavigationWorkspaceUi {
    pub(super) visible: bool,
    pub(super) profile_id: String,
    pub(super) path_start: [f32; 3],
    pub(super) path_end: [f32; 3],
    pub(super) path_report: String,
    pub(super) path_waypoints: Vec<engine::glam::Vec3>,
    settings_document: Option<NavMeshBakeDocument>,
    settings_path: Option<PathBuf>,
    settings_error: Option<String>,
}

impl Default for NavigationWorkspaceUi {
    fn default() -> Self {
        Self {
            visible: false,
            profile_id: engine::navmesh::DEFAULT_NAVIGATION_PROFILE.to_owned(),
            path_start: [0.0, 0.0, 0.0],
            path_end: [4.0, 0.0, 4.0],
            path_report: "Run a path test against the current production artifact.".to_owned(),
            path_waypoints: Vec::new(),
            settings_document: None,
            settings_path: None,
            settings_error: None,
        }
    }
}

fn load_navigation_document_for_ui(
    project: &ProjectRoot,
    scene_path: Option<&Path>,
) -> Result<(NavMeshBakeDocument, PathBuf), String> {
    let path = navigation_bake_document_path(project, scene_path)
        .ok_or_else(|| "save the scene before editing navigation bake settings".to_owned())?;
    if !path.is_file() {
        let mut document = NavMeshBakeDocument::default();
        let scene_stem = scene_path
            .and_then(Path::file_stem)
            .map(|stem| stem.to_string_lossy().trim_end_matches(".scene").to_owned())
            .unwrap_or_else(|| "scene".to_owned());
        document.output_asset = format!("navigation/{scene_stem}.navmesh.json");
        return Ok((document, path));
    }
    let json = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let document = NavMeshBakeDocument::from_json(&json)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
    Ok((document, path))
}

#[cfg(feature = "visual-validation")]
fn visual_navigation_component_value(
    type_id: &str,
    overrides: impl IntoIterator<Item = (&'static str, engine_authoring::Value)>,
) -> Result<engine_authoring::Value, String> {
    let component_type = ComponentTypeId::new(type_id);
    let registry = engine::builtin_registry();
    let definition = registry
        .get(&component_type)
        .ok_or_else(|| format!("missing built-in component schema `{type_id}`"))?;
    let mut value = definition.schema.default_value();
    let engine_authoring::Value::Object(fields) = &mut value else {
        return Err(format!("built-in component `{type_id}` is not object-valued"));
    };
    for (field, replacement) in overrides {
        fields.insert(field.to_owned(), replacement);
    }
    Ok(value)
}

impl super::EditorApp {
    #[cfg(feature = "visual-validation")]
    pub fn prepare_navigation_visual_validation(&mut self) {
        self.open_navigation_window();
        if let Err(error) = self.prepare_navigation_visual_validation_fixture() {
            self.navigation_workspace.path_report =
                format!("Visual validation fixture failed: {error}");
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.navigation_visual_fixture_failed",
                    error,
                ));
        }
    }

    #[cfg(feature = "visual-validation")]
    fn prepare_navigation_visual_validation_fixture(&mut self) -> Result<(), String> {
        use engine_authoring::Value;

        let project = self
            .project_root
            .clone()
            .ok_or_else(|| "open a project before preparing navigation visual validation".to_owned())?;

        let floor = self
            .session
            .create_scene_entity("visual_nav_floor")
            .map_err(|error| error.to_string())?;
        self.session
            .add_scene_component(
                floor.clone(),
                ComponentTypeId::new(engine::scene_bridge::COLLIDER_COMPONENT),
                visual_navigation_component_value(
                    engine::scene_bridge::COLLIDER_COMPONENT,
                    [
                        ("half_extent_x", Value::F64(4.0)),
                        ("half_extent_y", Value::F64(0.25)),
                        ("half_extent_z", Value::F64(4.0)),
                    ],
                )?,
            )
            .map_err(|error| error.to_string())?;
        self.session
            .add_scene_component(
                floor.clone(),
                ComponentTypeId::new(engine::scene_bridge::PHYSICS_BODY_COMPONENT),
                visual_navigation_component_value(
                    engine::scene_bridge::PHYSICS_BODY_COMPONENT,
                    std::iter::empty(),
                )?,
            )
            .map_err(|error| error.to_string())?;

        let link = self
            .session
            .create_scene_entity("visual_nav_link")
            .map_err(|error| error.to_string())?;
        self.session
            .add_scene_component(
                link,
                ComponentTypeId::new(engine::navigation_bake::NAVIGATION_LINK_COMPONENT),
                visual_navigation_component_value(
                    engine::navigation_bake::NAVIGATION_LINK_COMPONENT,
                    [
                        ("start_x", Value::F64(-2.5)),
                        ("start_y", Value::F64(0.25)),
                        ("start_z", Value::F64(-2.0)),
                        ("end_x", Value::F64(2.5)),
                        ("end_y", Value::F64(0.25)),
                        ("end_z", Value::F64(-2.0)),
                    ],
                )?,
            )
            .map_err(|error| error.to_string())?;

        let modifier = self
            .session
            .create_scene_entity("visual_nav_modifier")
            .map_err(|error| error.to_string())?;
        self.session
            .add_scene_component(
                modifier,
                ComponentTypeId::new(engine::navigation_bake::NAVIGATION_MODIFIER_COMPONENT),
                visual_navigation_component_value(
                    engine::navigation_bake::NAVIGATION_MODIFIER_COMPONENT,
                    [
                        ("center_x", Value::F64(0.0)),
                        ("center_y", Value::F64(0.25)),
                        ("center_z", Value::F64(1.5)),
                        ("half_extents_x", Value::F64(0.8)),
                        ("half_extents_y", Value::F64(0.3)),
                        ("half_extents_z", Value::F64(0.8)),
                        ("mode", Value::String("area".to_owned())),
                        ("area", Value::I64(0)),
                        ("cost_multiplier", Value::F64(2.0)),
                    ],
                )?,
            )
            .map_err(|error| error.to_string())?;

        let agent = self
            .session
            .create_scene_entity("visual_nav_agent")
            .map_err(|error| error.to_string())?;
        self.session
            .add_scene_component(
                agent,
                ComponentTypeId::new(engine::scene_bridge::NAV_MESH_AGENT_COMPONENT),
                visual_navigation_component_value(
                    engine::scene_bridge::NAV_MESH_AGENT_COMPONENT,
                    [
                        ("has_target", Value::Bool(true)),
                        ("target_x", Value::F64(3.0)),
                        ("target_y", Value::F64(0.25)),
                        ("target_z", Value::F64(3.0)),
                    ],
                )?,
            )
            .map_err(|error| error.to_string())?;

        let scene = self
            .session
            .scene()
            .cloned()
            .ok_or_else(|| "navigation visual validation requires an open scene".to_owned())?;
        let (mut document, document_path) =
            load_navigation_document_for_ui(&project, self.session.current_document_path())?;
        document.source_fingerprint = None;
        let mut manifest = self.asset_manifest.clone();
        let cancelled = AtomicBool::new(false);
        let result = bake_scene_navmesh(
            &scene,
            &project,
            &mut manifest,
            &mut document,
            &cancelled,
        )
        .map_err(|error| error.to_string())?;
        let document_json = document
            .to_canonical_json()
            .map_err(|error| error.to_string())?;
        if let Some(parent) = document_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        replace_file_contents(&document_path, &document_json)
            .map_err(|error| error.to_string())?;
        self.asset_manifest = manifest;

        let surface = self
            .session
            .create_scene_entity("visual_nav_surface")
            .map_err(|error| error.to_string())?;
        self.session
            .add_scene_component(
                surface,
                ComponentTypeId::new(engine::scene_bridge::NAV_MESH_SURFACE_COMPONENT),
                Value::Object(std::collections::BTreeMap::from([(
                    "source".to_owned(),
                    Value::AssetRef(result.asset_id),
                )])),
            )
            .map_err(|error| error.to_string())?;

        self.navigation_workspace.path_start = [-3.0, 0.25, -3.0];
        self.navigation_workspace.path_end = [3.0, 0.25, 3.0];
        self.reload_navigation_settings_for_ui();
        self.run_navigation_path_test();
        if let Some(scene) = self.session.scene() {
            let _ = self.scene_view.focus_entity(scene, &floor);
        }
        Ok(())
    }

    pub(super) fn open_navigation_window(&mut self) {
        self.navigation_workspace.visible = true;
        if self.navigation_workspace.settings_document.is_none() {
            self.reload_navigation_settings_for_ui();
        }
    }

    fn reload_navigation_settings_for_ui(&mut self) {
        let result = self.project_root.as_ref().ok_or_else(|| "open a project first".to_owned()).and_then(|project| {
            load_navigation_document_for_ui(project, self.session.current_document_path())
        });
        match result {
            Ok((document, path)) => {
                self.navigation_workspace.settings_document = Some(document);
                self.navigation_workspace.settings_path = Some(path);
                self.navigation_workspace.settings_error = None;
            }
            Err(error) => {
                self.navigation_workspace.settings_document = None;
                self.navigation_workspace.settings_path = None;
                self.navigation_workspace.settings_error = Some(error);
            }
        }
    }

    pub(super) fn show_navigation_window(&mut self, context: &eframe::egui::Context) {
        if !self.navigation_workspace.visible {
            return;
        }
        let mut open = true;
        let mut bake = false;
        let mut cancel = false;
        let mut run_path_test = false;
        let mut reload_settings = false;
        let mut save_settings = false;
        eframe::egui::Window::new("Navigation")
            .open(&mut open)
            .default_width(420.0)
            .show(context, |ui| {
                ui.heading("Production NavMesh");
                let uses_navigation = self.session.scene().is_some_and(scene_uses_navigation);
                let stale = self.filesystem_scene_problems.iter().any(|problem| {
                    problem.code == "editor.navigation_bake_not_current"
                });
                let status = if self.navigation_bake.is_cancelling() {
                    "Cancelling..."
                } else if self.navigation_bake.is_running() {
                    "Baking..."
                } else if stale {
                    "STALE / MISSING"
                } else if uses_navigation {
                    "Current"
                } else {
                    "No navigation components"
                };
                ui.horizontal(|ui| {
                    ui.strong("Bake status:");
                    ui.label(status);
                });
                ui.label("Scene View overlay: polygons, edges, off-mesh links, agents, and tested path.");
                ui.horizontal(|ui| {
                    bake = ui
                        .add_enabled(
                            self.session.scene().is_some()
                                && !self.is_playing()
                                && !self.navigation_bake.is_running(),
                            eframe::egui::Button::new(if stale { "Rebuild NavMesh" } else { "Bake NavMesh" }),
                        )
                        .clicked();
                    cancel = ui
                        .add_enabled(
                            self.navigation_bake.is_running() && !self.navigation_bake.is_cancelling(),
                            eframe::egui::Button::new("Cancel Bake"),
                        )
                        .clicked();
                    reload_settings = ui.button("Reload Settings").clicked();
                });
                ui.separator();
                ui.strong("Bake Settings / Agent Profiles");
                if let Some(error) = &self.navigation_workspace.settings_error {
                    ui.label(error);
                }
                if let Some(document) = &mut self.navigation_workspace.settings_document {
                    ui.horizontal(|ui| {
                        ui.label("Tile size");
                        ui.add(eframe::egui::DragValue::new(&mut document.settings.tile_size).range(0.1..=1024.0));
                    });
                    for profile in &mut document.settings.profiles {
                        let title = format!("{} ({})", profile.name, profile.id.as_str());
                        ui.collapsing(title, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("ID");
                                ui.text_edit_singleline(&mut profile.id.0);
                            });
                            ui.horizontal(|ui| {
                                ui.label("Name");
                                ui.text_edit_singleline(&mut profile.name);
                            });
                            ui.horizontal(|ui| {
                                ui.label("Radius");
                                ui.add(eframe::egui::DragValue::new(&mut profile.radius).speed(0.05));
                                ui.label("Height");
                                ui.add(eframe::egui::DragValue::new(&mut profile.height).speed(0.05));
                            });
                            ui.horizontal(|ui| {
                                ui.label("Max slope");
                                ui.add(eframe::egui::DragValue::new(&mut profile.max_slope_degrees).speed(1.0));
                                ui.label("Max step");
                                ui.add(eframe::egui::DragValue::new(&mut profile.max_climb).speed(0.05));
                            });
                        });
                    }
                    if ui.button("Add Agent Profile").clicked() {
                        let mut profile = engine::navmesh::NavigationAgentProfile::default();
                        let suffix = document.settings.profiles.len() + 1;
                        profile.id = engine::navmesh::NavigationProfileId::new(format!("profile_{suffix}"));
                        profile.name = format!("Profile {suffix}");
                        document.settings.profiles.push(profile);
                    }
                    save_settings = ui.button("Save Settings (marks bake stale)").clicked();
                }
                ui.separator();
                ui.strong("Path Test");
                ui.horizontal(|ui| {
                    ui.label("Profile");
                    ui.text_edit_singleline(&mut self.navigation_workspace.profile_id);
                });
                for (label, point) in [
                    ("Start", &mut self.navigation_workspace.path_start),
                    ("Destination", &mut self.navigation_workspace.path_end),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        for value in point {
                            ui.add(eframe::egui::DragValue::new(value).speed(0.1));
                        }
                    });
                }
                run_path_test = ui.button("Test Path").clicked();
                ui.label(&self.navigation_workspace.path_report);
            });
        self.navigation_workspace.visible = open;
        if reload_settings {
            self.reload_navigation_settings_for_ui();
        }
        if save_settings {
            self.save_navigation_settings_for_ui();
        }
        if bake {
            self.bake_current_navmesh();
        }
        if cancel {
            self.cancel_current_navmesh_bake();
        }
        if run_path_test {
            self.run_navigation_path_test();
        }
    }

    fn save_navigation_settings_for_ui(&mut self) {
        let (Some(document), Some(path)) = (
            self.navigation_workspace.settings_document.as_mut(),
            self.navigation_workspace.settings_path.as_ref(),
        ) else {
            return;
        };
        document.source_fingerprint = None;
        match document
            .to_canonical_json()
            .map_err(|error| error.to_string())
            .and_then(|json| {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                replace_file_contents(path, &json).map_err(|error| error.to_string())
            })
        {
            Ok(()) => {
                self.navigation_workspace.settings_error = None;
                self.refresh_scene_problems();
            }
            Err(error) => self.navigation_workspace.settings_error = Some(error),
        }
    }

    fn run_navigation_path_test(&mut self) {
        self.navigation_workspace.path_waypoints.clear();
        let result = (|| -> Result<(String, Vec<engine::glam::Vec3>), String> {
            let project = self.project_root.as_ref().ok_or_else(|| "open a project first".to_owned())?;
            let (document, _) = load_navigation_document_for_ui(
                project,
                self.session.current_document_path(),
            )?;
            let nav_mesh_path = project.assets_root().join(&document.output_asset);
            let nav_mesh = engine::navmesh::load_navmesh(&nav_mesh_path)
                .map_err(|error| format!("could not load {}: {error}", nav_mesh_path.display()))?;
            let query = engine::navmesh::NavMeshQuery::new(nav_mesh);
            let start = engine::glam::Vec3::from_array(self.navigation_workspace.path_start);
            let end = engine::glam::Vec3::from_array(self.navigation_workspace.path_end);
            match query.query_path(&self.navigation_workspace.profile_id, start, end) {
                engine::navmesh::NavigationPathResult::Complete(path) => Ok((
                    format!(
                        "Complete: {} waypoints, {} corridor polygons, {} links, cost {:.3}",
                        path.waypoints.len(),
                        path.corridor.len(),
                        path.links.len(),
                        path.total_cost
                    ),
                    path.waypoints,
                )),
                engine::navmesh::NavigationPathResult::Partial(path) => Ok((
                    format!(
                        "Partial: {} waypoints, {} corridor polygons, {} links, cost {:.3}",
                        path.waypoints.len(),
                        path.corridor.len(),
                        path.links.len(),
                        path.total_cost
                    ),
                    path.waypoints,
                )),
                engine::navmesh::NavigationPathResult::Failure(error) => {
                    Err(format!("Path failed: {error:?}"))
                }
            }
        })();
        match result {
            Ok((report, waypoints)) => {
                self.navigation_workspace.path_report = report;
                self.navigation_workspace.path_waypoints = waypoints;
            }
            Err(error) => self.navigation_workspace.path_report = error,
        }
    }
}

pub(super) enum NavigationBakeCompletion {
    Succeeded {
        tab_id: WorkspaceTabId,
        project: ProjectRoot,
        manifest: AssetManifest,
        result: Box<NavMeshBakeResult>,
    },
    Cancelled {
        tab_id: WorkspaceTabId,
    },
    Failed {
        tab_id: WorkspaceTabId,
        error: String,
    },
}

#[derive(Default)]
pub(super) struct NavigationBakeManager {
    receiver: Option<Receiver<NavigationBakeCompletion>>,
    cancelled: Option<Arc<AtomicBool>>,
    cancel_requested: bool,
}

impl NavigationBakeManager {
    pub(super) fn is_running(&self) -> bool {
        self.receiver.is_some()
    }

    pub(super) fn is_cancelling(&self) -> bool {
        self.is_running() && self.cancel_requested
    }

    pub(super) fn start(
        &mut self,
        tab_id: WorkspaceTabId,
        scene: AuthoringScene,
        project: ProjectRoot,
        manifest: AssetManifest,
        document: NavMeshBakeDocument,
        document_path: PathBuf,
    ) -> Result<(), &'static str> {
        if self.is_running() {
            return Err("a navigation bake is already running");
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        self.cancelled = Some(cancelled);
        self.cancel_requested = false;
        thread::spawn(move || {
            let completion = run_navigation_bake(
                tab_id,
                scene,
                project,
                manifest,
                document,
                document_path,
                &worker_cancelled,
            );
            let _ = sender.send(completion);
        });
        Ok(())
    }

    pub(super) fn poll(&mut self) -> Option<NavigationBakeCompletion> {
        let completion = self.receiver.as_ref()?.try_recv().ok()?;
        self.receiver = None;
        self.cancelled = None;
        self.cancel_requested = false;
        Some(completion)
    }

    pub(super) fn cancel(&mut self) -> bool {
        let Some(cancelled) = &self.cancelled else {
            return false;
        };
        cancelled.store(true, Ordering::Relaxed);
        self.cancel_requested = true;
        true
    }

    pub(super) fn clear(&mut self) {
        let _ = self.cancel();
        self.receiver = None;
        self.cancelled = None;
        self.cancel_requested = false;
    }
}

fn run_navigation_bake(
    tab_id: WorkspaceTabId,
    scene: AuthoringScene,
    project: ProjectRoot,
    mut manifest: AssetManifest,
    mut document: NavMeshBakeDocument,
    document_path: PathBuf,
    cancelled: &AtomicBool,
) -> NavigationBakeCompletion {
    let result = match bake_scene_navmesh(
        &scene,
        &project,
        &mut manifest,
        &mut document,
        cancelled,
    ) {
        Ok(result) => result,
        Err(NavMeshBakeError::Shared(NavigationBakeServiceError::Cancelled)) => {
            return NavigationBakeCompletion::Cancelled { tab_id };
        }
        Err(error) => {
            return NavigationBakeCompletion::Failed {
                tab_id,
                error: error.to_string(),
            };
        }
    };
    let persist_result = document
        .to_canonical_json()
        .map_err(|error| error.to_string())
        .and_then(|json| {
            if let Some(parent) = document_path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            replace_file_contents(&document_path, &json).map_err(|error| error.to_string())
        });
    if let Err(error) = persist_result {
        return NavigationBakeCompletion::Failed { tab_id, error };
    }
    NavigationBakeCompletion::Succeeded {
        tab_id,
        project,
        manifest,
        result: Box::new(result),
    }
}
