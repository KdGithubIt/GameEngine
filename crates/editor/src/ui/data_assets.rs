//! Inspector tooling for reusable generic data assets.

use super::*;
use engine::data_asset::{DataAssetDocument, DataAssetRef, DATA_ASSET_FILE_SUFFIX};

const DATA_ASSET_UI_STATE_ID: &str = "data_asset_inspector_state";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum NewDataFieldKind {
    Bool,
    Integer,
    Number,
    #[default]
    Text,
    Vec2,
    Vec3,
}

impl NewDataFieldKind {
    fn label(self) -> &'static str {
        match self {
            Self::Bool => "Bool",
            Self::Integer => "Integer",
            Self::Number => "Number",
            Self::Text => "Text",
            Self::Vec2 => "Vec2",
            Self::Vec3 => "Vec3",
        }
    }

    fn default_value(self) -> Value {
        match self {
            Self::Bool => Value::Bool(false),
            Self::Integer => Value::I64(0),
            Self::Number => Value::F64(0.0),
            Self::Text => Value::String(String::new()),
            Self::Vec2 => Value::Object(std::collections::BTreeMap::from([
                ("x".to_owned(), Value::F64(0.0)),
                ("y".to_owned(), Value::F64(0.0)),
            ])),
            Self::Vec3 => Value::Object(std::collections::BTreeMap::from([
                ("x".to_owned(), Value::F64(0.0)),
                ("y".to_owned(), Value::F64(0.0)),
                ("z".to_owned(), Value::F64(0.0)),
            ])),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct DataAssetUiState {
    selected: Option<AssetId>,
    create_name: String,
    new_field_name: String,
    new_field_kind: NewDataFieldKind,
}

#[derive(Debug, Clone)]
struct DataAssetChoice {
    id: AssetId,
    label: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct ComponentDataAssetReference {
    component_type: ComponentTypeId,
    path: Vec<PropertyPathSegment>,
    asset: Option<AssetId>,
}

/// Geometry and interaction result produced by the Data Asset creation row.
///
/// Keeping both widget responses makes the production layout directly testable:
/// the regression test can verify that the exact button used by the Inspector
/// remains one line high at the Inspector's minimum width.
struct DataAssetCreationControlsResponse {
    create_requested: bool,
    _name_field: egui::Response,
    _create_button: egui::Response,
}

impl EditorApp {
    /// Draws project data-asset authoring and selected-entity reference controls.
    pub(super) fn show_data_asset_tools(&mut self, ui: &mut egui::Ui) {
        let Some(project) = self.project_root.clone() else {
            return;
        };
        let state_id = egui::Id::new(DATA_ASSET_UI_STATE_ID);
        let mut state = ui
            .ctx()
            .data_mut(|data| data.remove_temp::<DataAssetUiState>(state_id))
            .unwrap_or_default();
        let choices = data_asset_choices(&self.asset_manifest);
        if state
            .selected
            .as_ref()
            .is_some_and(|selected| !choices.iter().any(|choice| &choice.id == selected))
        {
            state.selected = None;
        }
        if state.selected.is_none() {
            state.selected = choices.first().map(|choice| choice.id.clone());
        }

        egui::CollapsingHeader::new("Data Assets")
            .id_salt("data_asset_inspector")
            .default_open(false)
            .show(ui, |ui| {
                ui.small(
                    "Reusable project values. Use engine::data_asset::DataAssetRef in a \
                     GameComponent to reference one.",
                );
                self.show_data_asset_creation(ui, &project, &mut state);
                ui.separator();
                self.show_data_asset_document_editor(ui, &project, &choices, &mut state);
            });

        self.show_selected_entity_data_asset_references(ui, &choices);
        ui.ctx().data_mut(|data| data.insert_temp(state_id, state));
    }

    fn show_data_asset_creation(
        &mut self,
        ui: &mut egui::Ui,
        project: &ProjectRoot,
        state: &mut DataAssetUiState,
    ) {
        ui.strong("Create");
        let controls = show_data_asset_creation_controls(ui, &mut state.create_name);
        if controls.create_requested {
            match self.create_data_asset(project, &state.create_name) {
                Ok(asset) => {
                    state.selected = Some(asset);
                    state.create_name.clear();
                }
                Err(error) => {
                    self.report_data_asset_error("editor.data_asset_create_failed", error);
                }
            }
        }
    }

    fn show_data_asset_document_editor(
        &mut self,
        ui: &mut egui::Ui,
        project: &ProjectRoot,
        choices: &[DataAssetChoice],
        state: &mut DataAssetUiState,
    ) {
        ui.strong("Edit");
        if choices.is_empty() {
            ui.label("No data assets have been created in this project.");
            return;
        }

        let selected_label = state
            .selected
            .as_ref()
            .and_then(|selected| choices.iter().find(|choice| &choice.id == selected))
            .map(|choice| choice.label.as_str())
            .unwrap_or("Select data asset…");
        egui::ComboBox::from_id_salt("data_asset_document_picker")
            .selected_text(selected_label)
            .show_ui(ui, |ui| {
                for choice in choices {
                    if ui
                        .selectable_label(
                            state.selected.as_ref() == Some(&choice.id),
                            &choice.label,
                        )
                        .clicked()
                    {
                        state.selected = Some(choice.id.clone());
                        ui.close();
                    }
                }
            });

        let Some(selected) = state.selected.clone() else {
            return;
        };
        let Some(choice) = choices.iter().find(|choice| choice.id == selected) else {
            return;
        };
        let absolute_path = project.assets_root().join(&choice.path);
        let mut document = match load_data_asset_document(&absolute_path) {
            Ok(document) => document,
            Err(error) => {
                ui.colored_label(
                    egui::Color32::from_rgb(230, 92, 92),
                    format!("Could not open {}: {error}", choice.path.display()),
                );
                return;
            }
        };

        ui.label("Display Name");
        let mut changed = ui
            .text_edit_singleline(&mut document.display_name)
            .changed();
        ui.small(format!("Path: {}", choice.path.display()));
        ui.monospace(format!("Asset ID: {}", selected.as_str()));
        ui.separator();
        ui.strong("Fields");

        let field_names = document.fields.keys().cloned().collect::<Vec<_>>();
        let mut remove = None;
        for name in field_names {
            let Some(value) = document.fields.get_mut(&name) else {
                continue;
            };
            ui.push_id(("data_asset_field", &name), |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(&name);
                        if ui.small_button("Remove").clicked() {
                            remove = Some(name.clone());
                        }
                    });
                    changed |= show_data_asset_value_editor(ui, value);
                });
            });
        }
        if let Some(name) = remove {
            document.fields.remove(&name);
            changed = true;
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut state.new_field_name).hint_text("move_speed"));
            egui::ComboBox::from_id_salt("data_asset_new_field_kind")
                .selected_text(state.new_field_kind.label())
                .show_ui(ui, |ui| {
                    for kind in [
                        NewDataFieldKind::Bool,
                        NewDataFieldKind::Integer,
                        NewDataFieldKind::Number,
                        NewDataFieldKind::Text,
                        NewDataFieldKind::Vec2,
                        NewDataFieldKind::Vec3,
                    ] {
                        ui.selectable_value(&mut state.new_field_kind, kind, kind.label());
                    }
                });
        });

        let normalized_field = normalize_field_name(&state.new_field_name);
        let can_add =
            !normalized_field.is_empty() && !document.fields.contains_key(&normalized_field);
        if ui
            .add_enabled(can_add, egui::Button::new("Add Field"))
            .clicked()
        {
            document.fields.insert(
                normalized_field.clone(),
                state.new_field_kind.default_value(),
            );
            state.new_field_name.clear();
            changed = true;
        }
        if !state.new_field_name.trim().is_empty()
            && normalized_field != state.new_field_name.trim()
        {
            ui.small(format!("Stored field name: {normalized_field}"));
        }

        if changed
            && let Err(error) = persist_data_asset_document(&absolute_path, &document) {
                self.report_data_asset_error("editor.data_asset_save_failed", error);
            }
    }

    fn show_selected_entity_data_asset_references(
        &mut self,
        ui: &mut egui::Ui,
        choices: &[DataAssetChoice],
    ) {
        let Some(entity_id) = self.selected_entity.clone() else {
            return;
        };
        let references = self
            .session
            .scene_entity(&entity_id)
            .map(component_data_asset_references)
            .unwrap_or_default();
        if references.is_empty() {
            return;
        }

        egui::CollapsingHeader::new("Data Asset References")
            .id_salt("selected_entity_data_asset_references")
            .default_open(true)
            .show(ui, |ui| {
                ui.small("References declared with DataAssetRef on the selected entity.");
                for reference in references {
                    self.show_component_data_asset_reference(ui, &entity_id, &reference, choices);
                }
            });
    }

    fn show_component_data_asset_reference(
        &mut self,
        ui: &mut egui::Ui,
        entity_id: &EntityId,
        reference: &ComponentDataAssetReference,
        choices: &[DataAssetChoice],
    ) {
        let path_label = property_path_label(&reference.path);
        ui.push_id(
            (
                "data_asset_reference",
                reference.component_type.as_str(),
                &path_label,
            ),
            |ui| {
                ui.label(format!(
                    "{} / {path_label}",
                    reference.component_type.as_str()
                ));
                let mut selected = reference.asset.clone();
                let selected_label = selected
                    .as_ref()
                    .and_then(|asset| choices.iter().find(|choice| &choice.id == asset))
                    .map(|choice| choice.label.as_str())
                    .unwrap_or("None");
                let mut committed = false;
                egui::ComboBox::from_id_salt("data_asset_reference_picker")
                    .selected_text(selected_label)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(selected.is_none(), "None").clicked() {
                            selected = None;
                            committed = true;
                            ui.close();
                        }
                        for choice in choices {
                            if ui
                                .selectable_label(
                                    selected.as_ref() == Some(&choice.id),
                                    &choice.label,
                                )
                                .clicked()
                            {
                                selected = Some(choice.id.clone());
                                committed = true;
                                ui.close();
                            }
                        }
                    });

                if committed && selected != reference.asset {
                    let mut data_ref = DataAssetRef::default();
                    data_ref.set(selected);
                    let value = data_ref.to_authoring_value();
                    let result = if reference.path.is_empty() {
                        self.session.set_scene_component_value(
                            entity_id.clone(),
                            reference.component_type.clone(),
                            value,
                        )
                    } else {
                        self.session.set_scene_component_property(
                            entity_id.clone(),
                            reference.component_type.clone(),
                            reference.path.clone(),
                            value,
                        )
                    };
                    self.apply_ui_result(result);
                    self.refresh_scene_problems();
                }
            },
        );
    }

    fn create_data_asset(
        &mut self,
        project: &ProjectRoot,
        display_name: &str,
    ) -> Result<AssetId, String> {
        let display_name = display_name.trim();
        if display_name.is_empty() {
            return Err("display name must not be blank".to_owned());
        }

        let directory = project.assets_root().join("data");
        fs::create_dir_all(&directory)
            .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
        let stem = normalize_file_stem(display_name);
        let (relative_path, absolute_path) = available_data_asset_path(project, &stem);

        let document = DataAssetDocument::new(display_name);
        persist_data_asset_document(&absolute_path, &document)?;
        let asset = AssetId::generate();
        let mut staged_manifest = self.asset_manifest.clone();
        staged_manifest.insert(
            asset.clone(),
            engine::ManifestEntry {
                path: normalize_asset_path(&relative_path),
                name: Some(display_name.to_owned()),
                import_settings: engine::ImportSettings::default(),
            },
        );
        let manifest_json = staged_manifest
            .to_canonical_json()
            .map_err(|error| format!("manifest serialization failed: {error}"))?;
        if let Err(error) =
            replace_file_contents(&project.path().join("asset_manifest.json"), &manifest_json)
        {
            let _ = fs::remove_file(&absolute_path);
            return Err(format!("manifest save failed: {error}"));
        }

        self.asset_manifest = staged_manifest;
        self.asset_browser.refresh(&project.assets_root());
        Ok(asset)
    }

    fn report_data_asset_error(&mut self, code: &'static str, message: impl Into<String>) {
        self.session
            .push_diagnostic(engine_authoring::Diagnostic::error(code, message.into()));
    }
}

/// Draws the name input and create action without allowing the action label to
/// collapse into a vertical column in a narrow Inspector.
///
/// The Inspector intentionally enables text wrapping for long descriptions and
/// identifiers. A plain horizontal row lets the default-width text field consume
/// nearly all horizontal space, after which the button inherits wrapping and is
/// compressed to a few pixels. The shared wrapping control row instead moves the
/// complete button to the next row when necessary, while the dock action button
/// truncates rather than wrapping as a final safeguard.
fn show_data_asset_creation_controls(
    ui: &mut egui::Ui,
    create_name: &mut String,
) -> DataAssetCreationControlsResponse {
    control_row(ui, |ui| {
        let name_field =
            ui.add(egui::TextEdit::singleline(create_name).hint_text("Enemy Stats"));
        let valid = !create_name.trim().is_empty();
        let create_button = ui.add_enabled(
            valid,
            dock_action_button("New Data Asset", 116.0),
        );

        DataAssetCreationControlsResponse {
            create_requested: create_button.clicked(),
            _name_field: name_field,
            _create_button: create_button,
        }
    })
}

fn available_data_asset_path(project: &ProjectRoot, stem: &str) -> (PathBuf, PathBuf) {
    let mut suffix = 1_u32;
    loop {
        let file_name = if suffix == 1 {
            format!("{stem}{DATA_ASSET_FILE_SUFFIX}")
        } else {
            format!("{stem}_{suffix}{DATA_ASSET_FILE_SUFFIX}")
        };
        let relative_path = PathBuf::from("data").join(file_name);
        let absolute_path = project.assets_root().join(&relative_path);
        if !absolute_path.exists() {
            return (relative_path, absolute_path);
        }
        suffix = suffix.saturating_add(1);
    }
}

fn data_asset_choices(manifest: &engine::AssetManifest) -> Vec<DataAssetChoice> {
    let mut choices = manifest
        .iter()
        .filter_map(|(id, entry)| {
            let path = PathBuf::from(&entry.path);
            engine::data_asset::is_data_asset_path(&path).then(|| DataAssetChoice {
                id: id.clone(),
                label: entry
                    .name
                    .clone()
                    .unwrap_or_else(|| data_asset_display_name_from_path(&path)),
                path,
            })
        })
        .collect::<Vec<_>>();
    choices.sort_by(|left, right| {
        left.label
            .to_ascii_lowercase()
            .cmp(&right.label.to_ascii_lowercase())
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    choices
}

fn component_data_asset_references(entity: &AuthoringEntity) -> Vec<ComponentDataAssetReference> {
    let mut references = Vec::new();
    for (component_type, value) in &entity.components {
        collect_data_asset_references(component_type, value, &mut Vec::new(), &mut references);
    }
    references
}

fn collect_data_asset_references(
    component_type: &ComponentTypeId,
    value: &Value,
    path: &mut Vec<PropertyPathSegment>,
    output: &mut Vec<ComponentDataAssetReference>,
) {
    if let Ok(reference) = DataAssetRef::from_authoring_value(value) {
        output.push(ComponentDataAssetReference {
            component_type: component_type.clone(),
            path: path.clone(),
            asset: reference.asset_id().cloned(),
        });
        return;
    }

    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                path.push(PropertyPathSegment::Field { name: name.clone() });
                collect_data_asset_references(component_type, value, path, output);
                path.pop();
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                path.push(PropertyPathSegment::Index { index });
                collect_data_asset_references(component_type, value, path, output);
                path.pop();
            }
        }
        _ => {}
    }
}

fn show_data_asset_value_editor(ui: &mut egui::Ui, value: &mut Value) -> bool {
    match value {
        Value::Null => {
            ui.weak("null");
            false
        }
        Value::Bool(value) => ui.checkbox(value, "Enabled").changed(),
        Value::I64(value) => ui.add(egui::DragValue::new(value)).changed(),
        Value::U64(value) => ui.add(egui::DragValue::new(value)).changed(),
        Value::F64(value) => ui.add(egui::DragValue::new(value).speed(0.1)).changed(),
        Value::String(value) => ui.text_edit_singleline(value).changed(),
        Value::Array(values) => {
            let mut changed = false;
            for (index, value) in values.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("[{index}]"));
                    changed |= show_data_asset_value_editor(ui, value);
                });
            }
            changed
        }
        Value::Object(fields) => {
            let mut changed = false;
            for (name, value) in fields.iter_mut() {
                ui.horizontal(|ui| {
                    ui.label(name);
                    changed |= show_data_asset_value_editor(ui, value);
                });
            }
            changed
        }
        Value::EntityRef(id) => {
            ui.monospace(format!("entity_ref: {}", id.as_str()));
            false
        }
        Value::AssetRef(id) => {
            ui.monospace(format!("asset_ref: {}", id.as_str()));
            false
        }
    }
}

fn load_data_asset_document(path: &Path) -> Result<DataAssetDocument, String> {
    let json = fs::read_to_string(path).map_err(|error| error.to_string())?;
    DataAssetDocument::from_json(&json).map_err(|error| error.to_string())
}

fn persist_data_asset_document(path: &Path, document: &DataAssetDocument) -> Result<(), String> {
    let json = document
        .to_canonical_json()
        .map_err(|error| error.to_string())?;
    replace_file_contents(path, &json).map_err(|error| error.to_string())
}

fn property_path_label(path: &[PropertyPathSegment]) -> String {
    if path.is_empty() {
        return "value".to_owned();
    }

    let mut label = String::new();
    for segment in path {
        match segment {
            PropertyPathSegment::Field { name } => {
                if !label.is_empty() {
                    label.push('.');
                }
                label.push_str(name);
            }
            PropertyPathSegment::Index { index } => {
                label.push('[');
                label.push_str(&index.to_string());
                label.push(']');
            }
        }
    }
    label
}

fn data_asset_display_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(DATA_ASSET_FILE_SUFFIX))
        .unwrap_or("Data Asset")
        .replace('_', " ")
}

fn normalize_file_stem(input: &str) -> String {
    let normalized = normalize_field_name(input);
    if normalized.is_empty() {
        "data_asset".to_owned()
    } else {
        normalized
    }
}

fn normalize_field_name(input: &str) -> String {
    let mut output = String::new();
    let mut pending_separator = false;
    for character in input.trim().chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            if pending_separator && !output.is_empty() && !output.ends_with('_') {
                output.push('_');
            }
            output.push(character.to_ascii_lowercase());
            pending_separator = false;
        } else {
            pending_separator = true;
        }
    }
    while output.ends_with('_') {
        output.pop();
    }
    if output
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        output.insert(0, '_');
    }
    output
}

fn normalize_asset_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduces the minimum-width Inspector configuration from the reported
    /// screenshot and verifies the production Data Asset controls themselves.
    #[test]
    fn data_asset_create_button_stays_horizontal_in_narrow_inspector() {
        let context = egui::Context::default();
        context.data_mut(|data| {
            data.insert_persisted(
                egui::Id::new("inspector_panel"),
                egui::PanelState {
                    rect: egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(INSPECTOR_MIN_WIDTH, 600.0),
                    ),
                },
            );
        });
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            ..egui::RawInput::default()
        };
        let mut controls = None;
        let mut inspector_rect = None;
        let mut create_name = "Enemy Stats".to_owned();

        let _ = context.run_ui(input, |ui| {
            let maximum_width = inspector_max_width(ui.available_width());
            let inspector = show_inspector_panel(ui, maximum_width, |ui| {
                egui::CollapsingHeader::new("Data Assets")
                    .default_open(true)
                    .show(ui, |ui| {
                        controls = Some(show_data_asset_creation_controls(ui, &mut create_name));
                    });
            });
            inspector_rect = Some(inspector.response.rect);
        });

        let controls = controls.expect("open Data Assets section must draw its creation controls");
        let inspector_rect = inspector_rect.expect("Inspector must be laid out");

        assert!(
            controls._create_button.rect.height() <= 28.0,
            "New Data Asset grew to {} points tall, so its label wrapped vertically",
            controls._create_button.rect.height()
        );
        assert!(
            controls._create_button.rect.right() <= inspector_rect.right() + 1.0,
            "New Data Asset ended at {} outside the Inspector's right edge {}",
            controls._create_button.rect.right(),
            inspector_rect.right()
        );
        assert!(
            controls._create_button.rect.top() > controls._name_field.rect.top(),
            "the narrow Inspector should move the complete action below the name field"
        );
    }

    #[test]
    fn field_names_are_normalized_for_persistence() {
        assert_eq!(normalize_field_name("Move Speed"), "move_speed");
        assert_eq!(normalize_field_name(" 2D Radius "), "_2d_radius");
    }

    #[test]
    fn nested_component_references_are_discovered() {
        let asset = AssetId::generate();
        let value = Value::Object(std::collections::BTreeMap::from([(
            "stats".to_owned(),
            DataAssetRef::new(asset.clone()).to_authoring_value(),
        )]));
        let component_type = ComponentTypeId::new("game.enemy");
        let mut output = Vec::new();

        collect_data_asset_references(&component_type, &value, &mut Vec::new(), &mut output);

        assert_eq!(output.len(), 1);
        assert_eq!(output[0].component_type, component_type);
        assert_eq!(output[0].asset.as_ref(), Some(&asset));
        assert_eq!(property_path_label(&output[0].path), "stats");
    }
}
