use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui;
use engine::advanced_geometry::{
    LayeredNavMesh, NavMeshLayer, NavMeshLayerLink, StaticTriangleMesh,
};
use engine::glam::Vec3;
use engine::navmesh::{bake_from_obstacles, NavMeshSettings};
use serde::{Deserialize, Serialize};

const GEOMETRY_PROJECT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObstacleAsset {
    center: [f32; 3],
    half_extents: [f32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NavLayerAsset {
    id: String,
    minimum_height: f32,
    maximum_height: f32,
    cell_size: f32,
    agent_radius: f32,
    world_min: [f32; 3],
    world_max: [f32; 3],
    walkable_height: f32,
    agent_height: f32,
    obstacles: Vec<ObstacleAsset>,
}

impl Default for NavLayerAsset {
    fn default() -> Self {
        Self {
            id: "layer".to_owned(),
            minimum_height: -0.5,
            maximum_height: 0.5,
            cell_size: 0.5,
            agent_radius: 0.4,
            world_min: [-10.0, 0.0, -10.0],
            world_max: [10.0, 0.0, 10.0],
            walkable_height: 0.0,
            agent_height: 1.8,
            obstacles: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NavLinkAsset {
    from_layer: String,
    to_layer: String,
    from: [f32; 3],
    to: [f32; 3],
    cost: f32,
    bidirectional: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StaticMeshAsset {
    id: String,
    vertices: Vec<[f32; 3]>,
    triangles: Vec<[u32; 3]>,
}

impl Default for StaticMeshAsset {
    fn default() -> Self {
        Self {
            id: "mesh".to_owned(),
            vertices: vec![[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [0.0, 0.0, 1.0]],
            triangles: vec![[0, 1, 2]],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdvancedGeometryProject {
    schema_version: u32,
    layers: Vec<NavLayerAsset>,
    links: Vec<NavLinkAsset>,
    meshes: Vec<StaticMeshAsset>,
}

impl Default for AdvancedGeometryProject {
    fn default() -> Self {
        let ground = NavLayerAsset {
            id: "ground".to_owned(),
            ..NavLayerAsset::default()
        };
        let upper = NavLayerAsset {
            id: "upper".to_owned(),
            minimum_height: 2.5,
            maximum_height: 3.5,
            world_min: [-10.0, 3.0, -10.0],
            world_max: [10.0, 3.0, 10.0],
            walkable_height: 3.0,
            ..NavLayerAsset::default()
        };
        Self {
            schema_version: GEOMETRY_PROJECT_SCHEMA_VERSION,
            layers: vec![ground, upper],
            links: vec![NavLinkAsset {
                from_layer: "ground".to_owned(),
                to_layer: "upper".to_owned(),
                from: [0.0, 0.0, 0.0],
                to: [0.0, 3.0, 0.0],
                cost: 1.0,
                bidirectional: true,
            }],
            meshes: vec![StaticMeshAsset {
                id: "ground_probe".to_owned(),
                ..StaticMeshAsset::default()
            }],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GeometryTab {
    Layers,
    Links,
    StaticMeshes,
}

struct AdvancedGeometryDesigner {
    project: AdvancedGeometryProject,
    tab: GeometryTab,
    selected_layer: Option<usize>,
    selected_link: Option<usize>,
    selected_mesh: Option<usize>,
    path: Option<PathBuf>,
    dirty: bool,
    status: String,
    path_start: [f32; 3],
    path_end: [f32; 3],
    path_result: Vec<[f32; 3]>,
    ray_origin: [f32; 3],
    ray_direction: [f32; 3],
    ray_distance: f32,
    ray_result: String,
}

impl Default for AdvancedGeometryDesigner {
    fn default() -> Self {
        Self {
            project: AdvancedGeometryProject::default(),
            tab: GeometryTab::Layers,
            selected_layer: Some(0),
            selected_link: Some(0),
            selected_mesh: Some(0),
            path: None,
            dirty: false,
            status: "Ready".to_owned(),
            path_start: [-4.0, 0.0, 0.0],
            path_end: [4.0, 3.0, 0.0],
            path_result: Vec::new(),
            ray_origin: [0.0, 2.0, 0.0],
            ray_direction: [0.0, -1.0, 0.0],
            ray_distance: 10.0,
            ray_result: "Not tested".to_owned(),
        }
    }
}

impl AdvancedGeometryDesigner {
    fn new_document(&mut self) {
        *self = Self::default();
        self.status = "Created a new advanced geometry project".to_owned();
    }

    fn open_dialog(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Advanced Geometry Project", &["json"])
            .pick_file()
        else {
            return;
        };
        match fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|json| {
                serde_json::from_str::<AdvancedGeometryProject>(&json)
                    .map_err(|error| error.to_string())
            })
            .and_then(|project| {
                validate_project(&project)?;
                Ok(project)
            }) {
            Ok(project) => {
                self.project = project;
                self.selected_layer = (!self.project.layers.is_empty()).then_some(0);
                self.selected_link = (!self.project.links.is_empty()).then_some(0);
                self.selected_mesh = (!self.project.meshes.is_empty()).then_some(0);
                self.path = Some(path.clone());
                self.dirty = false;
                self.path_result.clear();
                self.ray_result = "Not tested".to_owned();
                self.status = format!("Opened {}", path.display());
            }
            Err(error) => self.status = format!("Open failed: {error}"),
        }
    }

    fn save(&mut self) {
        let Some(path) = self.path.clone() else {
            self.save_as();
            return;
        };
        self.save_to(&path);
    }

    fn save_as(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Advanced Geometry Project", &["json"])
            .set_file_name("world.advanced-geometry.json");
        if let Some(path) = &self.path
            && let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
            }
        let Some(path) = dialog.save_file() else {
            return;
        };
        self.save_to(&path);
    }

    fn save_to(&mut self, path: &Path) {
        let result = validate_project(&self.project)
            .and_then(|()| serde_json::to_string_pretty(&self.project).map_err(|e| e.to_string()))
            .and_then(|json| fs::write(path, json).map_err(|error| error.to_string()));
        match result {
            Ok(()) => {
                self.path = Some(path.to_path_buf());
                self.dirty = false;
                self.status = format!("Saved {}", path.display());
            }
            Err(error) => self.status = format!("Save blocked: {error}"),
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("New").clicked() {
                self.new_document();
            }
            if ui.button("Open...").clicked() {
                self.open_dialog();
            }
            if ui.button("Save").clicked() {
                self.save();
            }
            if ui.button("Save As...").clicked() {
                self.save_as();
            }
            ui.separator();
            for (tab, label) in [
                (GeometryTab::Layers, "NavMesh Layers"),
                (GeometryTab::Links, "Floor Links & Path Test"),
                (GeometryTab::StaticMeshes, "Static Mesh & Raycast"),
            ] {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.tab = tab;
                }
            }
            ui.separator();
            ui.label(
                self.path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Unsaved project".to_owned()),
            );
            if self.dirty {
                ui.strong("Modified");
            }
        });
    }

    fn layer_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Navigation Floors");
        ui.horizontal(|ui| {
            if ui.button("Add Layer").clicked() {
                let layer = NavLayerAsset {
                    id: unique_name(
                        "layer",
                        self.project.layers.iter().map(|item| item.id.as_str()),
                    ),
                    ..Default::default()
                };
                self.project.layers.push(layer);
                self.selected_layer = Some(self.project.layers.len() - 1);
                self.dirty = true;
            }
            if ui
                .add_enabled(
                    self.selected_layer.is_some(),
                    egui::Button::new("Delete Layer"),
                )
                .clicked()
            {
                self.delete_selected_layer();
            }
        });
        selection_combo(
            ui,
            "Layer",
            "selected_layer",
            &self
                .project
                .layers
                .iter()
                .map(|layer| layer.id.clone())
                .collect::<Vec<_>>(),
            &mut self.selected_layer,
        );
        ui.separator();

        let Some(index) = self.selected_layer else {
            ui.label("Add a navigation layer to begin.");
            return;
        };
        if index >= self.project.layers.len() {
            self.selected_layer = None;
            return;
        }

        let mut remove_obstacle = None;
        {
            let layer = &mut self.project.layers[index];
            egui::Grid::new("nav_layer_fields")
                .num_columns(2)
                .striped(true)
                .show(ui, |ui| {
                    ui.label("Stable ID");
                    self.dirty |= ui.text_edit_singleline(&mut layer.id).changed();
                    ui.end_row();
                    self.dirty |= scalar_row(ui, "Minimum height", &mut layer.minimum_height);
                    self.dirty |= scalar_row(ui, "Maximum height", &mut layer.maximum_height);
                    self.dirty |= scalar_row(ui, "Cell size", &mut layer.cell_size);
                    self.dirty |= scalar_row(ui, "Agent radius", &mut layer.agent_radius);
                    self.dirty |= scalar_row(ui, "Walkable height", &mut layer.walkable_height);
                    self.dirty |= scalar_row(ui, "Agent height", &mut layer.agent_height);
                    self.dirty |= vector_row(ui, "World minimum", &mut layer.world_min);
                    self.dirty |= vector_row(ui, "World maximum", &mut layer.world_max);
                });

            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("Obstacles");
                if ui.button("Add AABB").clicked() {
                    layer.obstacles.push(ObstacleAsset {
                        center: [0.0, layer.walkable_height + 0.5, 0.0],
                        half_extents: [0.5, 0.5, 0.5],
                    });
                    self.dirty = true;
                }
            });
            for (obstacle_index, obstacle) in layer.obstacles.iter_mut().enumerate() {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.strong(format!("Obstacle {obstacle_index}"));
                        if ui.small_button("Delete").clicked() {
                            remove_obstacle = Some(obstacle_index);
                        }
                    });
                    self.dirty |= vector_editor(ui, "Center", &mut obstacle.center);
                    self.dirty |= vector_editor(ui, "Half extents", &mut obstacle.half_extents);
                });
            }
        }
        if let Some(obstacle_index) = remove_obstacle {
            self.project.layers[index].obstacles.remove(obstacle_index);
            self.dirty = true;
        }

        ui.separator();
        match build_layer(&self.project.layers[index]) {
            Ok(runtime) => {
                let walkable = runtime
                    .nav_mesh
                    .walkable
                    .iter()
                    .filter(|value| **value)
                    .count();
                ui.label(format!(
                    "Bake preview: {} x {} cells, {walkable} walkable",
                    runtime.nav_mesh.cols, runtime.nav_mesh.rows
                ));
            }
            Err(error) => {
                ui.label(format!("Layer invalid: {error}"));
            }
        }
        validation_ui(ui, &self.project);
    }

    fn delete_selected_layer(&mut self) {
        let Some(index) = self.selected_layer else {
            return;
        };
        if index >= self.project.layers.len() {
            return;
        }
        let removed = self.project.layers.remove(index).id;
        self.project
            .links
            .retain(|link| link.from_layer != removed && link.to_layer != removed);
        self.selected_layer = select_after_remove(index, self.project.layers.len());
        self.selected_link = self
            .selected_link
            .filter(|selected| *selected < self.project.links.len());
        self.dirty = true;
    }

    fn link_tab(&mut self, ui: &mut egui::Ui) {
        let layer_ids = self
            .project
            .layers
            .iter()
            .map(|layer| layer.id.clone())
            .collect::<Vec<_>>();
        ui.heading("Inter-floor Links");
        ui.horizontal(|ui| {
            if ui.button("Add Link").clicked() {
                let from_layer = layer_ids.first().cloned().unwrap_or_default();
                let to_layer = layer_ids
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| from_layer.clone());
                self.project.links.push(NavLinkAsset {
                    from_layer,
                    to_layer,
                    from: [0.0, 0.0, 0.0],
                    to: [0.0, 0.0, 0.0],
                    cost: 0.0,
                    bidirectional: true,
                });
                self.selected_link = Some(self.project.links.len() - 1);
                self.dirty = true;
            }
            if ui
                .add_enabled(
                    self.selected_link.is_some(),
                    egui::Button::new("Delete Link"),
                )
                .clicked()
                && let Some(index) = self.selected_link
                    && index < self.project.links.len() {
                        self.project.links.remove(index);
                        self.selected_link = select_after_remove(index, self.project.links.len());
                        self.dirty = true;
                    }
        });
        selection_combo(
            ui,
            "Link",
            "selected_link",
            &self
                .project
                .links
                .iter()
                .map(|link| format!("{} -> {}", link.from_layer, link.to_layer))
                .collect::<Vec<_>>(),
            &mut self.selected_link,
        );
        ui.separator();

        if let Some(index) = self.selected_link {
            if let Some(link) = self.project.links.get_mut(index) {
                layer_combo(
                    ui,
                    "From layer",
                    "link_from_layer",
                    &layer_ids,
                    &mut link.from_layer,
                    &mut self.dirty,
                );
                layer_combo(
                    ui,
                    "To layer",
                    "link_to_layer",
                    &layer_ids,
                    &mut link.to_layer,
                    &mut self.dirty,
                );
                self.dirty |= vector_editor(ui, "From position", &mut link.from);
                self.dirty |= vector_editor(ui, "To position", &mut link.to);
                ui.horizontal(|ui| {
                    ui.label("Additional cost");
                    self.dirty |= ui
                        .add(
                            egui::DragValue::new(&mut link.cost)
                                .speed(0.05)
                                .range(0.0..=10_000.0),
                        )
                        .changed();
                });
                self.dirty |= ui
                    .checkbox(&mut link.bidirectional, "Bidirectional")
                    .changed();
            }
        } else {
            ui.label("Add a link to connect navigation floors.");
        }

        ui.separator();
        ui.heading("Path Query Test");
        vector_editor(ui, "Start", &mut self.path_start);
        vector_editor(ui, "End", &mut self.path_end);
        if ui.button("Find Path").clicked() {
            match build_layered_navmesh(&self.project).and_then(|nav| {
                nav.find_path(vec3(self.path_start), vec3(self.path_end))
                    .ok_or_else(|| "No route found for the current layers and links".to_owned())
            }) {
                Ok(path) => {
                    self.path_result = path.iter().map(Vec3::to_array).collect();
                    self.status = format!("Path found with {} waypoints", self.path_result.len());
                }
                Err(error) => {
                    self.path_result.clear();
                    self.status = format!("Path test failed: {error}");
                }
            }
        }
        for (index, point) in self.path_result.iter().enumerate() {
            ui.monospace(format!(
                "{index}: [{:.3}, {:.3}, {:.3}]",
                point[0], point[1], point[2]
            ));
        }
        validation_ui(ui, &self.project);
    }

    fn mesh_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Static Triangle Meshes");
        ui.horizontal(|ui| {
            if ui.button("Add Mesh").clicked() {
                let mesh = StaticMeshAsset {
                    id: unique_name(
                        "mesh",
                        self.project.meshes.iter().map(|item| item.id.as_str()),
                    ),
                    ..Default::default()
                };
                self.project.meshes.push(mesh);
                self.selected_mesh = Some(self.project.meshes.len() - 1);
                self.dirty = true;
            }
            if ui
                .add_enabled(
                    self.selected_mesh.is_some(),
                    egui::Button::new("Delete Mesh"),
                )
                .clicked()
                && let Some(index) = self.selected_mesh
                    && index < self.project.meshes.len() {
                        self.project.meshes.remove(index);
                        self.selected_mesh = select_after_remove(index, self.project.meshes.len());
                        self.dirty = true;
                    }
        });
        selection_combo(
            ui,
            "Mesh",
            "selected_mesh",
            &self
                .project
                .meshes
                .iter()
                .map(|mesh| mesh.id.clone())
                .collect::<Vec<_>>(),
            &mut self.selected_mesh,
        );
        ui.separator();

        let mut remove_vertex = None;
        let mut remove_triangle = None;
        if let Some(index) = self.selected_mesh {
            if let Some(mesh) = self.project.meshes.get_mut(index) {
                ui.horizontal(|ui| {
                    ui.label("Stable ID");
                    self.dirty |= ui.text_edit_singleline(&mut mesh.id).changed();
                });
                ui.horizontal(|ui| {
                    if ui.button("Add Vertex").clicked() {
                        mesh.vertices.push([0.0, 0.0, 0.0]);
                        self.dirty = true;
                    }
                    if ui.button("Add Triangle").clicked() {
                        mesh.triangles.push([0, 1, 2]);
                        self.dirty = true;
                    }
                });
                ui.strong("Vertices");
                for (vertex_index, vertex) in mesh.vertices.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(vertex_index.to_string());
                        self.dirty |= vector_components(ui, vertex);
                        if ui.small_button("Delete").clicked() {
                            remove_vertex = Some(vertex_index);
                        }
                    });
                }
                ui.strong("Triangles");
                for (triangle_index, triangle) in mesh.triangles.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(triangle_index.to_string());
                        for vertex_index in triangle.iter_mut() {
                            self.dirty |= ui
                                .add(egui::DragValue::new(vertex_index).speed(1.0))
                                .changed();
                        }
                        if ui.small_button("Delete").clicked() {
                            remove_triangle = Some(triangle_index);
                        }
                    });
                }
            }
            if let Some(vertex_index) = remove_vertex {
                self.project.meshes[index].vertices.remove(vertex_index);
                self.dirty = true;
            }
            if let Some(triangle_index) = remove_triangle {
                self.project.meshes[index].triangles.remove(triangle_index);
                self.dirty = true;
            }
            match build_static_mesh(&self.project.meshes[index]) {
                Ok(runtime) => ui.label(format!(
                    "Bounds: [{:.2}, {:.2}, {:.2}] to [{:.2}, {:.2}, {:.2}]",
                    runtime.minimum().x,
                    runtime.minimum().y,
                    runtime.minimum().z,
                    runtime.maximum().x,
                    runtime.maximum().y,
                    runtime.maximum().z
                )),
                Err(error) => ui.label(format!("Mesh invalid: {error}")),
            };
        } else {
            ui.label("Add a static mesh to begin.");
        }

        ui.separator();
        ui.heading("Finite Raycast Test");
        vector_editor(ui, "Origin", &mut self.ray_origin);
        vector_editor(ui, "Direction", &mut self.ray_direction);
        ui.horizontal(|ui| {
            ui.label("Maximum distance");
            ui.add(
                egui::DragValue::new(&mut self.ray_distance)
                    .speed(0.1)
                    .range(0.0..=1_000_000.0),
            );
            if ui.button("Raycast Selected Mesh").clicked() {
                self.raycast_selected();
            }
        });
        ui.label(&self.ray_result);
        validation_ui(ui, &self.project);
    }

    fn raycast_selected(&mut self) {
        let Some(index) = self.selected_mesh else {
            self.ray_result = "Select a mesh first".to_owned();
            return;
        };
        let Some(mesh) = self.project.meshes.get(index) else {
            self.ray_result = "Selected mesh no longer exists".to_owned();
            return;
        };
        match build_static_mesh(mesh) {
            Ok(runtime) => match runtime.raycast(
                vec3(self.ray_origin),
                vec3(self.ray_direction),
                self.ray_distance,
            ) {
                Some(hit) => {
                    self.ray_result = format!(
                        "Hit triangle {} at {:.3}: [{:.3}, {:.3}, {:.3}], normal [{:.3}, {:.3}, {:.3}]",
                        hit.triangle,
                        hit.distance,
                        hit.position.x,
                        hit.position.y,
                        hit.position.z,
                        hit.normal.x,
                        hit.normal.y,
                        hit.normal.z
                    );
                }
                None => self.ray_result = "No hit".to_owned(),
            },
            Err(error) => self.ray_result = format!("Mesh invalid: {error}"),
        }
    }
}

impl eframe::App for AdvancedGeometryDesigner {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("advanced_geometry_toolbar").show_inside(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("advanced_geometry_status")
            .exact_size(28.0)
            .show_inside(ui, |ui| {
                ui.label(&self.status);
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::both().show(ui, |ui| match self.tab {
                GeometryTab::Layers => self.layer_tab(ui),
                GeometryTab::Links => self.link_tab(ui),
                GeometryTab::StaticMeshes => self.mesh_tab(ui),
            });
        });
    }
}

fn validate_project(project: &AdvancedGeometryProject) -> Result<(), String> {
    if project.schema_version != GEOMETRY_PROJECT_SCHEMA_VERSION {
        return Err(format!(
            "unsupported schema {}; expected {GEOMETRY_PROJECT_SCHEMA_VERSION}",
            project.schema_version
        ));
    }
    build_layered_navmesh(project)?;
    let mut mesh_ids = BTreeSet::new();
    for mesh in &project.meshes {
        if mesh.id.trim().is_empty() {
            return Err("static mesh ID must not be blank".to_owned());
        }
        if !mesh_ids.insert(mesh.id.as_str()) {
            return Err(format!("static mesh ID `{}` is duplicated", mesh.id));
        }
        build_static_mesh(mesh)?;
    }
    Ok(())
}

fn build_layered_navmesh(project: &AdvancedGeometryProject) -> Result<LayeredNavMesh, String> {
    let layers = project
        .layers
        .iter()
        .map(build_layer)
        .collect::<Result<Vec<_>, _>>()?;
    let links = project
        .links
        .iter()
        .map(|link| NavMeshLayerLink {
            from_layer: link.from_layer.clone(),
            to_layer: link.to_layer.clone(),
            from: vec3(link.from),
            to: vec3(link.to),
            cost: link.cost,
            bidirectional: link.bidirectional,
        })
        .collect();
    LayeredNavMesh::new(layers, links).map_err(|error| error.to_string())
}

fn build_layer(layer: &NavLayerAsset) -> Result<NavMeshLayer, String> {
    if layer.id.trim().is_empty() {
        return Err("layer ID must not be blank".to_owned());
    }
    if !layer.cell_size.is_finite() || layer.cell_size <= 0.0 {
        return Err(format!("layer `{}` cell size must be positive", layer.id));
    }
    if !layer.agent_radius.is_finite() || layer.agent_radius < 0.0 {
        return Err(format!("layer `{}` agent radius is invalid", layer.id));
    }
    if layer.world_max[0] <= layer.world_min[0] || layer.world_max[2] <= layer.world_min[2] {
        return Err(format!("layer `{}` world bounds are invalid", layer.id));
    }
    for obstacle in &layer.obstacles {
        if obstacle
            .half_extents
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(format!("layer `{}` has an invalid obstacle", layer.id));
        }
    }
    let settings = NavMeshSettings {
        cell_size: layer.cell_size,
        agent_radius: layer.agent_radius,
        world_min: vec3(layer.world_min),
        world_max: vec3(layer.world_max),
        walkable_height: layer.walkable_height,
        agent_height: layer.agent_height,
    };
    let obstacles = layer
        .obstacles
        .iter()
        .map(|obstacle| (vec3(obstacle.center), vec3(obstacle.half_extents)))
        .collect::<Vec<_>>();
    Ok(NavMeshLayer {
        id: layer.id.clone(),
        minimum_height: layer.minimum_height,
        maximum_height: layer.maximum_height,
        nav_mesh: bake_from_obstacles(&obstacles, &settings),
    })
}

fn build_static_mesh(mesh: &StaticMeshAsset) -> Result<StaticTriangleMesh, String> {
    StaticTriangleMesh::new(
        mesh.vertices.iter().copied().map(vec3).collect(),
        mesh.triangles.clone(),
    )
    .map_err(|error| error.to_string())
}

fn validation_ui(ui: &mut egui::Ui, project: &AdvancedGeometryProject) {
    ui.separator();
    match validate_project(project) {
        Ok(()) => {
            ui.label("Validation: valid");
        }
        Err(error) => {
            ui.label(format!("Validation: {error}"));
        }
    }
}

fn vec3(value: [f32; 3]) -> Vec3 {
    Vec3::from_array(value)
}

fn scalar_row(ui: &mut egui::Ui, label: &str, value: &mut f32) -> bool {
    ui.label(label);
    let changed = ui.add(egui::DragValue::new(value).speed(0.05)).changed();
    ui.end_row();
    changed
}

fn vector_row(ui: &mut egui::Ui, label: &str, value: &mut [f32; 3]) -> bool {
    ui.label(label);
    let changed = vector_components(ui, value);
    ui.end_row();
    changed
}

fn vector_editor(ui: &mut egui::Ui, label: &str, value: &mut [f32; 3]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        changed = vector_components(ui, value);
    });
    changed
}

fn vector_components(ui: &mut egui::Ui, value: &mut [f32; 3]) -> bool {
    let mut changed = false;
    for component in value {
        changed |= ui
            .add(
                egui::DragValue::new(component)
                    .speed(0.05)
                    .fixed_decimals(3),
            )
            .changed();
    }
    changed
}

fn selection_combo(
    ui: &mut egui::Ui,
    label: &str,
    id: impl std::hash::Hash,
    choices: &[String],
    selected: &mut Option<usize>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        let selected_text = selected
            .and_then(|index| choices.get(index))
            .map(String::as_str)
            .unwrap_or("None");
        egui::ComboBox::from_id_salt(id)
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                for (index, choice) in choices.iter().enumerate() {
                    ui.selectable_value(selected, Some(index), choice);
                }
            });
    });
}

fn layer_combo(
    ui: &mut egui::Ui,
    label: &str,
    id: impl std::hash::Hash,
    choices: &[String],
    selected: &mut String,
    dirty: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(id)
            .selected_text(if selected.is_empty() {
                "Select layer"
            } else {
                selected.as_str()
            })
            .show_ui(ui, |ui| {
                for choice in choices {
                    if ui.selectable_label(selected == choice, choice).clicked() {
                        *selected = choice.clone();
                        *dirty = true;
                    }
                }
            });
    });
}

fn select_after_remove(removed: usize, remaining: usize) -> Option<usize> {
    if remaining == 0 {
        None
    } else {
        Some(removed.min(remaining - 1))
    }
}

fn unique_name<'a>(base: &str, names: impl Iterator<Item = &'a str>) -> String {
    let existing = names.collect::<BTreeSet<_>>();
    if !existing.contains(base) {
        return base.to_owned();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}_{suffix}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
        suffix += 1;
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 860.0])
            .with_min_inner_size([900.0, 620.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "GameEngine Advanced Geometry Designer",
        options,
        Box::new(|creation_context| {
            engine_editor::install_editor_fonts(&creation_context.egui_ctx);
            Ok(Box::new(AdvancedGeometryDesigner::default()))
        }),
    )
}
