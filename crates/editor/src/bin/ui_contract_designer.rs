use std::fs;
use std::path::{Path, PathBuf};

use eframe::egui;
use engine_authoring::{
    UiAuthoringContract, UiBindingDeclaration, UiBindingKind, UiDocument, UiEventDeclaration,
    UiFocusDirection, UiFocusLink, UiNode,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContractTab {
    Bindings,
    Events,
    Focus,
}

struct UiContractDesigner {
    contract: UiAuthoringContract,
    document: UiDocument,
    contract_path: Option<PathBuf>,
    document_path: Option<PathBuf>,
    tab: ContractTab,
    dirty: bool,
    status: String,
}

impl Default for UiContractDesigner {
    fn default() -> Self {
        Self {
            contract: UiAuthoringContract::default(),
            document: UiDocument::default(),
            contract_path: None,
            document_path: None,
            tab: ContractTab::Bindings,
            dirty: false,
            status: "Load a .ui.json document, then author its project contract".to_owned(),
        }
    }
}

impl UiContractDesigner {
    fn new_contract(&mut self) {
        self.contract = UiAuthoringContract::default();
        self.contract_path = None;
        self.dirty = false;
        self.status = "Created a new UI contract".to_owned();
    }

    fn load_document(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("GameEngine UI Document", &["json"])
            .pick_file()
        else {
            return;
        };
        match fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|json| UiDocument::from_json_str(&json).map_err(|error| error.to_string()))
        {
            Ok(document) => {
                self.document = document;
                self.document_path = Some(path.clone());
                self.status = format!("Loaded UI document {}", path.display());
            }
            Err(error) => self.status = format!("UI document load failed: {error}"),
        }
    }

    fn open_contract(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("UI Authoring Contract", &["json"])
            .pick_file()
        else {
            return;
        };
        match fs::read_to_string(&path)
            .map_err(|error| error.to_string())
            .and_then(|json| serde_json::from_str(&json).map_err(|error| error.to_string()))
        {
            Ok(contract) => {
                self.contract = contract;
                self.contract_path = Some(path.clone());
                self.dirty = false;
                self.status = format!("Opened {}", path.display());
            }
            Err(error) => self.status = format!("Contract open failed: {error}"),
        }
    }

    fn save(&mut self) {
        let Some(path) = self.contract_path.clone() else {
            self.save_as();
            return;
        };
        self.save_to(&path);
    }

    fn save_as(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .add_filter("UI Authoring Contract", &["json"])
            .set_file_name("screen.ui-contract.json");
        if let Some(document_path) = &self.document_path
            && let Some(parent) = document_path.parent() {
                dialog = dialog.set_directory(parent);
            }
        let Some(path) = dialog.save_file() else {
            return;
        };
        self.save_to(&path);
    }

    fn save_to(&mut self, path: &Path) {
        let result = self
            .contract
            .validate(&self.document)
            .map_err(|errors| {
                errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .and_then(|()| {
                serde_json::to_string_pretty(&self.contract).map_err(|error| error.to_string())
            })
            .and_then(|json| fs::write(path, json).map_err(|error| error.to_string()));
        match result {
            Ok(()) => {
                self.contract_path = Some(path.to_path_buf());
                self.dirty = false;
                self.status = format!("Saved {}", path.display());
            }
            Err(error) => self.status = format!("Save blocked: {error}"),
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui.button("New Contract").clicked() {
                self.new_contract();
            }
            if ui.button("Open Contract...").clicked() {
                self.open_contract();
            }
            if ui.button("Save").clicked() {
                self.save();
            }
            if ui.button("Save As...").clicked() {
                self.save_as();
            }
            ui.separator();
            if ui.button("Load UI Document...").clicked() {
                self.load_document();
            }
            ui.separator();
            for (tab, label) in [
                (ContractTab::Bindings, "Bindings"),
                (ContractTab::Events, "Events"),
                (ContractTab::Focus, "Focus Navigation"),
            ] {
                if ui.selectable_label(self.tab == tab, label).clicked() {
                    self.tab = tab;
                }
            }
            ui.separator();
            if self.dirty {
                ui.strong("Modified");
            }
        });
    }

    fn document_summary(&self, ui: &mut egui::Ui) {
        let nodes = node_ids(&self.document);
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("UI nodes: {}", nodes.len()));
            ui.separator();
            ui.label(
                self.document_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Default unsaved UI document".to_owned()),
            );
            ui.separator();
            ui.label(
                self.contract_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Unsaved contract".to_owned()),
            );
        });
    }

    fn bindings_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Typed Binding Candidates");
        ui.label("These names appear in UI Builder pickers instead of requiring manual typing.");
        if ui.button("Add Binding").clicked() {
            self.contract.bindings.push(UiBindingDeclaration {
                name: unique_name(
                    "binding",
                    self.contract
                        .bindings
                        .iter()
                        .map(|binding| binding.name.as_str()),
                ),
                kind: UiBindingKind::Text,
                description: String::new(),
            });
            self.dirty = true;
        }
        ui.separator();
        let mut remove = None;
        egui::Grid::new("ui_binding_grid")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Name");
                ui.strong("Kind");
                ui.strong("Description");
                ui.label("");
                ui.end_row();
                for (index, binding) in self.contract.bindings.iter_mut().enumerate() {
                    if ui.text_edit_singleline(&mut binding.name).changed() {
                        self.dirty = true;
                    }
                    egui::ComboBox::from_id_salt(("binding_kind", index))
                        .selected_text(binding_kind_label(binding.kind))
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(binding.kind == UiBindingKind::Text, "Text")
                                .clicked()
                            {
                                binding.kind = UiBindingKind::Text;
                                self.dirty = true;
                            }
                            if ui
                                .selectable_label(binding.kind == UiBindingKind::Number, "Number")
                                .clicked()
                            {
                                binding.kind = UiBindingKind::Number;
                                self.dirty = true;
                            }
                            if ui
                                .selectable_label(binding.kind == UiBindingKind::Flag, "Flag")
                                .clicked()
                            {
                                binding.kind = UiBindingKind::Flag;
                                self.dirty = true;
                            }
                        });
                    if ui.text_edit_singleline(&mut binding.description).changed() {
                        self.dirty = true;
                    }
                    if ui.small_button("Delete").clicked() {
                        remove = Some(index);
                    }
                    ui.end_row();
                }
            });
        if let Some(index) = remove {
            self.contract.bindings.remove(index);
            self.dirty = true;
        }
    }

    fn events_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("UI Event Candidates");
        ui.label("Buttons and future interactive widgets select stable project event names here.");
        if ui.button("Add Event").clicked() {
            self.contract.events.push(UiEventDeclaration {
                name: unique_name(
                    "event",
                    self.contract.events.iter().map(|event| event.name.as_str()),
                ),
                description: String::new(),
            });
            self.dirty = true;
        }
        ui.separator();
        let mut remove = None;
        egui::Grid::new("ui_event_grid")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                ui.strong("Name");
                ui.strong("Description");
                ui.label("");
                ui.end_row();
                for (index, event) in self.contract.events.iter_mut().enumerate() {
                    if ui.text_edit_singleline(&mut event.name).changed() {
                        self.dirty = true;
                    }
                    if ui.text_edit_singleline(&mut event.description).changed() {
                        self.dirty = true;
                    }
                    if ui.small_button("Delete").clicked() {
                        remove = Some(index);
                    }
                    ui.end_row();
                }
            });
        if let Some(index) = remove {
            self.contract.events.remove(index);
            self.dirty = true;
        }
    }

    fn focus_ui(&mut self, ui: &mut egui::Ui) {
        let nodes = node_ids(&self.document);
        ui.heading("Keyboard / Gamepad Focus Navigation");
        ui.label(
            "Choose the initial node and explicit directional neighbours using real UI node IDs.",
        );
        ui.horizontal(|ui| {
            ui.label("Initial focus");
            let selected = self.contract.initial_focus.as_deref().unwrap_or("None");
            egui::ComboBox::from_id_salt("initial_focus")
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.contract.initial_focus.is_none(), "None")
                        .clicked()
                    {
                        self.contract.initial_focus = None;
                        self.dirty = true;
                    }
                    for node in &nodes {
                        if ui
                            .selectable_label(
                                self.contract.initial_focus.as_deref() == Some(node.as_str()),
                                node,
                            )
                            .clicked()
                        {
                            self.contract.initial_focus = Some(node.clone());
                            self.dirty = true;
                        }
                    }
                });
            if ui.button("Add Focus Link").clicked() {
                let from = self
                    .contract
                    .initial_focus
                    .clone()
                    .or_else(|| nodes.first().cloned())
                    .unwrap_or_default();
                let to = nodes
                    .iter()
                    .find(|node| **node != from)
                    .cloned()
                    .unwrap_or_else(|| from.clone());
                self.contract.focus_links.push(UiFocusLink {
                    from,
                    direction: UiFocusDirection::Down,
                    to,
                });
                self.dirty = true;
            }
        });
        ui.separator();
        let mut remove = None;
        egui::Grid::new("ui_focus_grid")
            .num_columns(4)
            .striped(true)
            .show(ui, |ui| {
                ui.strong("From");
                ui.strong("Direction");
                ui.strong("To");
                ui.label("");
                ui.end_row();
                for (index, link) in self.contract.focus_links.iter_mut().enumerate() {
                    node_combo(
                        ui,
                        ("focus_from", index),
                        &nodes,
                        &mut link.from,
                        &mut self.dirty,
                    );
                    direction_combo(ui, index, link, &mut self.dirty);
                    node_combo(
                        ui,
                        ("focus_to", index),
                        &nodes,
                        &mut link.to,
                        &mut self.dirty,
                    );
                    if ui.small_button("Delete").clicked() {
                        remove = Some(index);
                    }
                    ui.end_row();
                }
            });
        if let Some(index) = remove {
            self.contract.focus_links.remove(index);
            self.dirty = true;
        }
        ui.separator();
        ui.heading("Focus Preview");
        if nodes.is_empty() {
            ui.label("The loaded document contains no nodes.");
        } else {
            for node in &nodes {
                ui.group(|ui| {
                    ui.strong(node);
                    for direction in [
                        UiFocusDirection::Up,
                        UiFocusDirection::Down,
                        UiFocusDirection::Left,
                        UiFocusDirection::Right,
                    ] {
                        let target = self.contract.focus_target(node, direction).unwrap_or("—");
                        ui.label(format!("{} -> {target}", direction_label(direction)));
                    }
                });
            }
        }
    }

    fn validation_ui(&self, ui: &mut egui::Ui) {
        match self.contract.validate(&self.document) {
            Ok(()) => {
                ui.label("Validation: valid");
            }
            Err(errors) => {
                ui.label(format!("Validation: {} issue(s)", errors.len()));
                for error in errors {
                    ui.label(format!("• {error}"));
                }
            }
        }
    }
}

impl eframe::App for UiContractDesigner {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("ui_contract_toolbar").show_inside(ui, |ui| self.toolbar(ui));
        egui::Panel::top("ui_contract_document")
            .exact_size(32.0)
            .show_inside(ui, |ui| self.document_summary(ui));
        egui::Panel::bottom("ui_contract_status")
            .resizable(true)
            .default_size(120.0)
            .show_inside(ui, |ui| {
                ui.label(&self.status);
                ui.separator();
                self.validation_ui(ui);
            });
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::both().show(ui, |ui| match self.tab {
                ContractTab::Bindings => self.bindings_ui(ui),
                ContractTab::Events => self.events_ui(ui),
                ContractTab::Focus => self.focus_ui(ui),
            });
        });
    }
}

fn collect_node_ids(node: &UiNode, output: &mut Vec<String>) {
    output.push(node.id.clone());
    for child in &node.children {
        collect_node_ids(child, output);
    }
}

fn node_ids(document: &UiDocument) -> Vec<String> {
    let mut nodes = Vec::new();
    collect_node_ids(&document.root, &mut nodes);
    nodes.sort();
    nodes.dedup();
    nodes
}

fn binding_kind_label(kind: UiBindingKind) -> &'static str {
    match kind {
        UiBindingKind::Text => "Text",
        UiBindingKind::Number => "Number",
        UiBindingKind::Flag => "Flag",
    }
}

fn direction_label(direction: UiFocusDirection) -> &'static str {
    match direction {
        UiFocusDirection::Up => "Up",
        UiFocusDirection::Down => "Down",
        UiFocusDirection::Left => "Left",
        UiFocusDirection::Right => "Right",
    }
}

fn direction_combo(ui: &mut egui::Ui, index: usize, link: &mut UiFocusLink, dirty: &mut bool) {
    egui::ComboBox::from_id_salt(("focus_direction", index))
        .selected_text(direction_label(link.direction))
        .show_ui(ui, |ui| {
            for direction in [
                UiFocusDirection::Up,
                UiFocusDirection::Down,
                UiFocusDirection::Left,
                UiFocusDirection::Right,
            ] {
                if ui
                    .selectable_label(link.direction == direction, direction_label(direction))
                    .clicked()
                {
                    link.direction = direction;
                    *dirty = true;
                }
            }
        });
}

fn node_combo(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    nodes: &[String],
    selected: &mut String,
    dirty: &mut bool,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(if selected.is_empty() {
            "Select node"
        } else {
            selected.as_str()
        })
        .show_ui(ui, |ui| {
            for node in nodes {
                if ui.selectable_label(selected == node, node).clicked() {
                    *selected = node.clone();
                    *dirty = true;
                }
            }
        });
}

fn unique_name<'a>(base: &str, names: impl Iterator<Item = &'a str>) -> String {
    let existing = names.collect::<std::collections::BTreeSet<_>>();
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
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([940.0, 640.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "GameEngine UI Contract Designer",
        options,
        Box::new(|creation_context| {
            engine_editor::install_editor_fonts(&creation_context.egui_ctx);
            Ok(Box::new(UiContractDesigner::default()))
        }),
    )
}
