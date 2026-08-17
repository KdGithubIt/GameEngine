//! Asset reference fields: the catalog of assignable assets, the searchable
//! picker, and the single, list, and unassigned reference editors.

use crate::ui::*;

pub(in crate::ui) fn builtin_asset_id(id: &str) -> AssetId {
    AssetId::from_stable_id(engine_authoring::id::StableId::new(id))
        .expect("built-in asset id constants are valid asset ids")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) struct AssetChoice {
    pub(in crate::ui) label: String,
    pub(in crate::ui) id: AssetId,
}

/// Width reserved for the trailing remove button of a reference list row.
const REFERENCE_LIST_REMOVE_WIDTH: f32 = 34.0;

/// Returns the physical Asset Browser row that owns an asset reference.
///
/// Imported sub-assets do not have files of their own, so they reveal their
/// model source. Built-in assets return `None` because they intentionally do
/// not appear in the project Asset Browser.
pub(in crate::ui) fn asset_reference_source_path(
    manifest: &engine::AssetManifest,
    asset: &AssetId,
) -> Option<PathBuf> {
    manifest
        .get(asset)
        .map(|entry| PathBuf::from(&entry.path))
        .or_else(|| {
            manifest
                .iter()
                .find(|(_, entry)| {
                    entry
                        .import_settings
                        .sub_assets
                        .iter()
                        .any(|sub_asset| sub_asset.id == asset.as_str())
                })
                .map(|(_, entry)| PathBuf::from(&entry.path))
        })
}

/// Resolves the best human-readable label for an existing asset reference.
///
/// Picker choices are preferred because they already contain category-aware
/// labels. Manifest lookup is retained as a repair path for missing files,
/// whose entries are deliberately excluded from new picker choices.
fn asset_reference_display_label(
    manifest: &engine::AssetManifest,
    choices: &[AssetChoice],
    asset: &AssetId,
) -> String {
    if let Some(choice) = choices.iter().find(|choice| &choice.id == asset) {
        return choice.label.clone();
    }
    if let Some(entry) = manifest.get(asset) {
        return entry.name.clone().unwrap_or_else(|| entry.path.clone());
    }
    for (_, source) in manifest.iter() {
        if let Some(sub_asset) = source
            .import_settings
            .sub_assets
            .iter()
            .find(|sub_asset| sub_asset.id == asset.as_str())
        {
            let source_label = source.name.as_deref().unwrap_or(&source.path);
            return format!("{source_label} / {}", sub_asset.name);
        }
    }
    asset.as_str().to_owned()
}

/// Maps authoring asset categories onto the same visual family used by the
/// Asset Browser. Imported model parts deliberately inherit the source model's
/// mesh icon because that is the row authors can reveal in the browser.
pub(in crate::ui) fn asset_reference_browser_kind(
    kind: engine::AssetKind,
) -> crate::asset_browser::AssetKind {
    use crate::asset_browser::AssetKind as BrowserKind;

    match kind {
        engine::AssetKind::Mesh
        | engine::AssetKind::GltfSource
        | engine::AssetKind::Skin
        | engine::AssetKind::Skeleton
        // Morphs and secondary-motion rigs have no file of their own; they are
        // revealed on their owning model's row, so they inherit its icon.
        | engine::AssetKind::Morph
        | engine::AssetKind::SecondaryMotionRig => BrowserKind::Mesh,
        engine::AssetKind::Material => BrowserKind::Material,
        engine::AssetKind::Texture | engine::AssetKind::SpriteAtlas => BrowserKind::Texture,
        engine::AssetKind::AnimationClip | engine::AssetKind::SpriteAnimation => BrowserKind::AnimationClip,
        engine::AssetKind::MotionSource => BrowserKind::MotionSource,
        engine::AssetKind::AnimationGraph | engine::AssetKind::BehaviorTree => BrowserKind::Graph,
        engine::AssetKind::VfxEffect => BrowserKind::Graph,
        engine::AssetKind::AnimationSet => BrowserKind::AnimationSet,
        engine::AssetKind::Audio => BrowserKind::Audio,
        engine::AssetKind::NavMesh => BrowserKind::NavMesh,
        engine::AssetKind::UiDocument => BrowserKind::UiDocument,
        engine::AssetKind::Prefab => BrowserKind::Prefab,
    }
}

/// Returns the exact symbol and accent color used by the Asset Browser tile
/// for the equivalent authoring asset family.
fn asset_reference_visual(kind: engine::AssetKind) -> (&'static str, egui::Color32) {
    let browser_kind = asset_reference_browser_kind(kind);
    (
        crate::ui::assets::asset_kind_icon(browser_kind),
        crate::ui::assets::asset_kind_color(browser_kind),
    )
}

#[cfg(test)]
pub(in crate::ui) fn asset_choices_from_manifest(
    component_type: &ComponentTypeId,
    manifest: &engine::AssetManifest,
    assets_root: Option<&Path>,
) -> Vec<AssetChoice> {
    let registry = engine::builtin_registry();
    let Some(engine::InspectorHint::AssetRef { kind }) = registry
        .get(component_type)
        .map(|definition| definition.inspector)
    else {
        return Vec::new();
    };
    asset_choices_for_kind(kind, manifest, assets_root)
}

pub(in crate::ui) fn asset_choices_for_kind(
    kind: engine::AssetKind,
    manifest: &engine::AssetManifest,
    assets_root: Option<&Path>,
) -> Vec<AssetChoice> {
    let mut choices = match kind {
        engine::AssetKind::Mesh => vec![
            AssetChoice {
                label: "Built-in Triangle".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_TRIANGLE_ASSET_ID),
            },
            AssetChoice {
                label: "Built-in Quad".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_QUAD_ASSET_ID),
            },
        ],
        engine::AssetKind::Material => vec![
            AssetChoice {
                label: "Built-in White".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_WHITE_MATERIAL_ASSET_ID),
            },
            AssetChoice {
                label: "Built-in Blue".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_BLUE_MATERIAL_ASSET_ID),
            },
            AssetChoice {
                label: "Built-in Orange".into(),
                id: builtin_asset_id(engine::scene_bridge::BUILTIN_ORANGE_MATERIAL_ASSET_ID),
            },
        ],
        engine::AssetKind::UiDocument => vec![AssetChoice {
            label: "Built-in UI".into(),
            id: builtin_asset_id(engine::scene_bridge::BUILTIN_UI_DOCUMENT_ASSET_ID),
        }],
        _ => Vec::new(),
    };
    choices.extend(
        manifest
            .iter()
            .filter(|(_, entry)| {
                manifest_path_matches_asset_kind(kind, Path::new(&entry.path), assets_root)
                    && asset_file_exists(&entry.path, assets_root)
                    && manifest_source_backs_kind_directly(kind, entry)
            })
            .map(|(id, entry)| AssetChoice {
                label: entry.name.clone().unwrap_or_else(|| entry.path.clone()),
                id: id.clone(),
            }),
    );
    for (source_id, source) in manifest.iter() {
        if !asset_file_exists(&source.path, assets_root) {
            continue;
        }
        let source_label = source.name.as_deref().unwrap_or(&source.path);
        for sub_asset in &source.import_settings.sub_assets {
            if !imported_sub_asset_matches_picker_kind(sub_asset.kind, kind) {
                continue;
            }
            if is_legacy_motion_clip_alias(source_id, sub_asset) {
                continue;
            }
            let stable = engine_authoring::StableId::new(&sub_asset.id);
            let Ok(id) = AssetId::from_stable_id(stable) else {
                // Malformed persisted IDs are surfaced by manifest/import
                // validation; the picker must not manufacture an unusable ref.
                continue;
            };
            let target_label = sub_asset
                .target_model_source
                .as_deref()
                .and_then(|target| {
                    AssetId::from_stable_id(engine_authoring::StableId::new(target)).ok()
                })
                .and_then(|target| manifest.get(&target))
                .map(|entry| entry.name.as_deref().unwrap_or(&entry.path));
            let label = match target_label {
                Some(target) => format!("{source_label} / {} — {target}", sub_asset.name),
                None => format!("{source_label} / {}", sub_asset.name),
            };
            choices.push(AssetChoice { label, id });
        }
    }
    choices
}

/// Returns whether a catalog row is the hidden compatibility alias retained
/// for an Animation Set authored before target-specific VMD clip IDs existed.
pub(in crate::ui) fn is_legacy_motion_clip_alias(
    source_id: &AssetId,
    sub_asset: &engine::ImportedSubAsset,
) -> bool {
    if sub_asset.kind != engine::ImportedSubAssetKind::Animation {
        return false;
    }
    let Some(target) = sub_asset.target_model_source.as_deref() else {
        return false;
    };
    let Ok(target) = AssetId::from_stable_id(engine_authoring::StableId::new(target)) else {
        return false;
    };
    sub_asset.id
        != engine::imported_motion_sub_asset_id(source_id, &target, sub_asset.index as usize)
            .as_str()
}

/// Returns whether a model source file may be referenced directly as `kind`,
/// rather than only through the sub-assets its import produced.
///
/// `asset_path_matches_kind` accepts a model source for every kind its
/// extension can carry, so this narrows that answer to the references the
/// engine actually resolves.
fn manifest_source_backs_kind_directly(
    kind: engine::AssetKind,
    entry: &engine::ManifestEntry,
) -> bool {
    let path = Path::new(&entry.path);
    if kind == engine::AssetKind::AnimationClip {
        // Model and motion sources only produce clips through import. Exposing
        // either top-level source here lets Animation Sets persist a reference
        // that runtime validation must reject instead of the imported clip.
        return !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, path)
            && !engine::asset_path_matches_kind(engine::AssetKind::MotionSource, path);
    }
    if !engine::asset_path_matches_kind(engine::AssetKind::GltfSource, path) {
        return true;
    }
    match kind {
        // An imported model hands its geometry to the sub-asset rows, but a
        // source that was never imported stays selectable so a plain mesh
        // file keeps working.
        engine::AssetKind::Mesh => entry.import_settings.sub_assets.is_empty(),
        _ => true,
    }
}

pub(in crate::ui) fn imported_sub_asset_matches_picker_kind(
    imported: engine::ImportedSubAssetKind,
    requested: engine::AssetKind,
) -> bool {
    matches!(
        (imported, requested),
        (engine::ImportedSubAssetKind::Mesh, engine::AssetKind::Mesh)
            | (
                engine::ImportedSubAssetKind::Material,
                engine::AssetKind::Material
            )
            | (
                engine::ImportedSubAssetKind::Texture,
                engine::AssetKind::Texture
            )
            | (
                engine::ImportedSubAssetKind::Skeleton,
                engine::AssetKind::Skeleton
            )
            | (
                // Validation already accepts animation sub-asset references
                // for clip fields; without this arm the picker could not
                // offer them at all.
                engine::ImportedSubAssetKind::Animation,
                engine::AssetKind::AnimationClip
            )
            | (engine::ImportedSubAssetKind::Skin, engine::AssetKind::Skin)
            | (
                engine::ImportedSubAssetKind::SecondaryMotionRig,
                engine::AssetKind::SecondaryMotionRig
            )
    )
}

/// Returns whether a registered asset still has its backing file.
///
/// A file deleted outside the editor leaves its manifest entry behind, and
/// offering that entry would let the author create a reference that is broken
/// the moment it is made. Existing references keep resolving to the entry and
/// are reported by scene validation instead.
///
/// Without an assets root there is nothing to check against, so entries are
/// kept; the picker must stay usable in tests and in previews that run
/// without a project on disk.
fn asset_file_exists(relative_path: &str, assets_root: Option<&Path>) -> bool {
    match assets_root {
        Some(root) => root.join(relative_path).is_file(),
        None => true,
    }
}

pub(in crate::ui) fn manifest_path_matches_asset_kind(
    kind: engine::AssetKind,
    relative_path: &Path,
    assets_root: Option<&Path>,
) -> bool {
    if !engine::asset_path_matches_kind(kind, relative_path) {
        return false;
    }
    match kind {
        engine::AssetKind::AnimationGraph => {
            graph_asset_has_kind(relative_path, assets_root, "anim.graph")
        }
        engine::AssetKind::BehaviorTree => {
            graph_asset_has_kind(relative_path, assets_root, "behavior_tree.graph")
        }
        _ => true,
    }
}

fn graph_asset_has_kind(
    relative_path: &Path,
    assets_root: Option<&Path>,
    expected_kind: &str,
) -> bool {
    let is_graph = relative_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".graph.json"));
    if !is_graph {
        return false;
    }
    let Some(assets_root) = assets_root else {
        // Tests and unopened-project fallback can still use conventional,
        // unambiguous suffixes without touching the filesystem.
        let name = relative_path.to_string_lossy().to_ascii_lowercase();
        return match expected_kind {
            "anim.graph" => name.ends_with(".anim.graph.json"),
            "behavior_tree.graph" => {
                name.ends_with(".behavior.graph.json") || name.ends_with(".bt.graph.json")
            }
            _ => false,
        };
    };
    std::fs::read_to_string(assets_root.join(relative_path))
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|document| document.get("kind")?.as_str().map(str::to_owned))
        .is_some_and(|kind| kind == expected_kind)
}

/// Whether an Asset Browser drag payload can be assigned to a field of the
/// given engine asset kind.
fn payload_matches_engine_kind(
    payload: &crate::drag_drop::DragPayload,
    kind: engine::AssetKind,
) -> bool {
    use crate::asset_browser::AssetKind as BrowserKind;
    matches!(
        (payload.kind, kind),
        (BrowserKind::Mesh, engine::AssetKind::Mesh)
            | (BrowserKind::Material, engine::AssetKind::Material)
            | (BrowserKind::Texture, engine::AssetKind::Texture)
            | (BrowserKind::AnimationSet, engine::AssetKind::AnimationSet)
            | (BrowserKind::AnimationClip, engine::AssetKind::AnimationClip)
    )
}

/// Accepts an Asset Browser drop on an AssetRef widget.
///
/// Returns the dropped asset when the payload kind matches; a hover with a
/// compatible payload highlights the widget so the drop target is obvious.
fn asset_drop_on_response(
    ui: &egui::Ui,
    response: &egui::Response,
    kind: engine::AssetKind,
) -> Option<AssetId> {
    if let Some(payload) = response.dnd_hover_payload::<crate::drag_drop::DragPayload>()
        && payload_matches_engine_kind(&payload, kind) {
            ui.painter().rect_stroke(
                response.rect,
                2.0,
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(90, 180, 255)),
                egui::StrokeKind::Outside,
            );
        }
    response
        .dnd_release_payload::<crate::drag_drop::DragPayload>()
        .filter(|payload| payload_matches_engine_kind(payload, kind))
        .map(|payload| payload.asset_id.clone())
}

/// Draws a searchable object-picker popup anchored to an AssetRef selector.
///
/// `CloseOnClickOutside` is essential here: a normal ComboBox closes on every
/// internal click, including the click needed to focus its search field.
fn show_asset_choice_picker(
    ui: &mut egui::Ui,
    selector_response: &egui::Response,
    current: Option<&AssetId>,
    kind: engine::AssetKind,
    choices: &[AssetChoice],
    manifest: &engine::AssetManifest,
    allow_none: bool,
) -> Option<ReferencePickerAction<AssetId>> {
    let picker_id = selector_response.id.with("asset_reference_picker");
    let search_id = picker_id.with("search");
    let mut search = ui
        .data_mut(|data| data.get_temp::<String>(search_id))
        .unwrap_or_default();
    let mut action = None;
    let (icon, icon_color) = asset_reference_visual(kind);

    egui::Popup::menu(selector_response)
        .id(picker_id)
        .width(380.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_min_width(360.0);

            if allow_none {
                if ui
                    .selectable_label(current.is_none(), "—  None")
                    .on_hover_text("Leave this reference unassigned")
                    .clicked()
                {
                    action = Some(ReferencePickerAction::Clear);
                    ui.close();
                }
                ui.separator();
            }

            let search_response = ui.add(
                egui::TextEdit::singleline(&mut search)
                    .hint_text("Search name, path, or ID...")
                    .desired_width(f32::INFINITY),
            );
            if selector_response.clicked() {
                search_response.request_focus();
            }
            ui.separator();

            let normalized_search = search.trim().to_ascii_lowercase();
            let mut visible_count = 0_usize;
            egui::ScrollArea::vertical()
                .id_salt(picker_id.with("scroll"))
                .max_height(300.0)
                .show(ui, |ui| {
                    for choice in choices {
                        let source_path = asset_reference_source_path(manifest, &choice.id);
                        let path_text = source_path
                            .as_ref()
                            .map(|path| path.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "Built-in engine asset".to_owned());
                        let matches = normalized_search.is_empty()
                            || choice
                                .label
                                .to_ascii_lowercase()
                                .contains(&normalized_search)
                            || path_text.to_ascii_lowercase().contains(&normalized_search)
                            || choice
                                .id
                                .as_str()
                                .to_ascii_lowercase()
                                .contains(&normalized_search);
                        if !matches {
                            continue;
                        }
                        visible_count += 1;
                        let selected = current == Some(&choice.id);
                        let row = ui.horizontal(|ui| {
                            ui.colored_label(icon_color, egui::RichText::new(icon).size(16.0));
                            let clicked = ui.selectable_label(selected, &choice.label).clicked();
                            ui.weak(path_text.as_str());
                            clicked
                        });
                        row.response.on_hover_text(format!(
                            "{}\nStable ID: {}",
                            path_text,
                            choice.id.as_str()
                        ));
                        if row.inner {
                            action = Some(ReferencePickerAction::Assign(choice.id.clone()));
                            ui.close();
                        }
                    }
                });
            if visible_count == 0 {
                ui.weak("No compatible assets match the search.");
            }
        });
    ui.data_mut(|data| data.insert_temp(search_id, search));
    action
}

pub(in crate::ui) fn show_asset_reference_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    kind: engine::AssetKind,
    context: &InspectorEditContext<'_>,
    allow_none: bool,
    clear_requested: &mut bool,
) -> Option<ComponentEdit> {
    let Value::AssetRef(current) = value else {
        ui.colored_label(egui::Color32::RED, "invalid asset reference");
        return None;
    };
    let choices = asset_choices_for_kind(kind, context.manifest, context.assets_root);
    let source_path = asset_reference_source_path(context.manifest, current);
    let label = asset_reference_display_label(context.manifest, &choices, current);
    let known = choices.iter().any(|choice| choice.id == *current) || source_path.is_some();
    let backing_file_missing = source_path.as_ref().is_some_and(|path| {
        context
            .assets_root
            .is_some_and(|assets_root| !assets_root.join(path).is_file())
    });
    let (icon, mut icon_color) = asset_reference_visual(kind);
    if !known {
        icon_color = egui::Color32::from_rgb(230, 92, 92);
    }
    let field_label = if known {
        label
    } else {
        format!("Missing: {}", current.as_str())
    };
    let location = match &source_path {
        Some(path) if backing_file_missing => format!("{} (file missing)", path.display()),
        Some(path) => path.display().to_string(),
        None if known => "Built-in engine asset".to_owned(),
        None => "The referenced asset is not registered.".to_owned(),
    };
    let tooltip = format!("{location}\nStable ID: {}", current.as_str());
    let (field_response, selector_response) =
        show_compact_reference_field(ui, icon, icon_color, &field_label, &tooltip);

    if field_response.clicked() && source_path.is_some() {
        context
            .reference_navigation
            .replace(Some(InspectorReferenceNavigation::RevealAsset(
                current.clone(),
            )));
    }

    let mut action = show_asset_choice_picker(
        ui,
        &selector_response,
        Some(current),
        kind,
        &choices,
        context.manifest,
        allow_none,
    );
    if let Some(dropped) = asset_drop_on_response(ui, &field_response, kind)
        .or_else(|| asset_drop_on_response(ui, &selector_response, kind))
    {
        action = Some(ReferencePickerAction::Assign(dropped));
    }

    match action {
        Some(ReferencePickerAction::Assign(selected)) if selected != *current => {
            Some(ComponentEdit::Property {
                path: Vec::new(),
                value: Value::AssetRef(selected),
            })
        }
        Some(ReferencePickerAction::Clear) if allow_none => {
            *clear_requested = true;
            None
        }
        _ => None,
    }
}

pub(in crate::ui) fn show_unassigned_asset_reference_editor(
    ui: &mut egui::Ui,
    kind: engine::AssetKind,
    context: &InspectorEditContext<'_>,
) -> Option<AssetId> {
    let choices = asset_choices_for_kind(kind, context.manifest, context.assets_root);
    let (icon, icon_color) = asset_reference_visual(kind);
    let (field_response, selector_response) =
        show_compact_reference_field(ui, icon, icon_color, "None", "No asset is assigned.");
    let mut action = show_asset_choice_picker(
        ui,
        &selector_response,
        None,
        kind,
        &choices,
        context.manifest,
        true,
    );
    if let Some(dropped) = asset_drop_on_response(ui, &field_response, kind)
        .or_else(|| asset_drop_on_response(ui, &selector_response, kind))
    {
        action = Some(ReferencePickerAction::Assign(dropped));
    }
    match action {
        Some(ReferencePickerAction::Assign(asset)) => Some(asset),
        Some(ReferencePickerAction::Clear) | None => None,
    }
}

/// Edits a deterministic ordered list of same-kind asset references.
pub(in crate::ui) fn show_asset_reference_list_editor(
    ui: &mut egui::Ui,
    value: &mut Value,
    kind: engine::AssetKind,
    context: &InspectorEditContext<'_>,
) -> Option<ComponentEdit> {
    let Value::Array(values) = value else {
        ui.colored_label(egui::Color32::RED, "invalid asset reference list");
        return None;
    };
    let mut replacement = values.clone();
    let mut changed = false;
    let mut remove = None;
    // An imported model contributes one slot per submesh, so this list is as
    // long as the source material set and must not grow without bound.
    bounded_list_scroll_area(ui, "asset_reference_list", |ui| {
        for (index, item) in replacement.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("#{index}"));
                let mut ignored_clear = false;
                // The reference field fills the row it is given, so the remove
                // button's own space is reserved before the field is drawn.
                let height = reference_row_height(ui);
                let reference_width = remaining_reference_row_width(
                    ui.available_width(),
                    REFERENCE_LIST_REMOVE_WIDTH,
                    ui.spacing().item_spacing.x,
                );
                let reference_edit = ui
                    .allocate_ui(egui::vec2(reference_width, height), |ui| {
                        show_asset_reference_editor(
                            ui,
                            item,
                            kind,
                            context,
                            false,
                            &mut ignored_clear,
                        )
                    })
                    .inner;
                if let Some(ComponentEdit::Property { value, .. }) = reference_edit {
                    *item = value;
                    changed = true;
                }
                if ui
                    .add_sized(
                        [REFERENCE_LIST_REMOVE_WIDTH, height],
                        egui::Button::new("−").small(),
                    )
                    .clicked()
                {
                    remove = Some(index);
                }
            });
        }
    });
    if let Some(index) = remove {
        replacement.remove(index);
        changed = true;
    }
    if ui.small_button("+ Add Material Slot").clicked() {
        let default = asset_choices_for_kind(kind, context.manifest, context.assets_root)
            .into_iter()
            .next()
            .map(|choice| Value::AssetRef(choice.id));
        if let Some(default) = default {
            replacement.push(default);
            changed = true;
        }
    }
    changed.then(|| ComponentEdit::Property {
        path: Vec::new(),
        value: Value::Array(replacement),
    })
}
