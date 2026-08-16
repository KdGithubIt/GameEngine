//! Asset browser and project browser panels.
//!
//! Draws the asset grid, the folder tree, and the sub-asset drag sources, and
//! reports what the author activated as an `AssetBrowserAction` for the
//! surrounding panels to act on.

use crate::ui::*;
use super::manifest::normalize_manifest_path;

impl EditorApp {
    pub(in crate::ui) fn notify_registered_assets(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let file_names = paths
            .iter()
            .map(|path| {
                path.file_name()
                    .unwrap_or(path.as_os_str())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        let noun = if file_names.len() == 1 {
            "asset"
        } else {
            "assets"
        };
        self.push_notification(
            EditorNotificationLevel::Success,
            format!(
                "Registered {} {noun}: {}",
                file_names.len(),
                file_names.join(", ")
            ),
        );
    }

    pub(in crate::ui) fn notify_asset_error(&mut self, message: impl Into<String>) {
        self.report_error("editor.asset_error", message.into());
    }

    /// Opens the OS file manager at the folder containing an asset.
    pub(in crate::ui) fn show_asset_in_explorer(&mut self, relative: &Path) {
        let Some(project) = self.project_root.as_ref() else {
            return;
        };
        let absolute = project.assets_root().join(relative);
        let target = if absolute.is_dir() {
            absolute.clone()
        } else {
            absolute
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or(absolute.clone())
        };
        if let Err(error) = open::that(&target) {
            self.session
                .push_diagnostic(engine_authoring::Diagnostic::error(
                    "editor.show_in_explorer_failed",
                    format!("could not open {}: {error}", target.display()),
                ));
        }
    }

}

/// Sub-assets of the selected glTF source shown as draggable rows.
///
/// Imported meshes, materials, and textures were previously invisible until
/// a picker was opened; listing them here also provides the drag source for
/// Inspector and Scene View drops. Clips are listed for discoverability —
/// the Animator's clip source takes the glTF file itself.
///
/// This is a drag source only — searching, filtering, selecting, and editing
/// all belong on the Inspector's own sub-asset list, which lives in the same
/// panel as the detail/edit fields it drives.
fn show_selected_gltf_sub_assets(
    ui: &mut egui::Ui,
    browser: &AssetBrowser,
    manifest: &engine::AssetManifest,
) {
    let mut selected = browser.selected_paths();
    let (Some(path), None) = (selected.next().cloned(), selected.next()) else {
        return;
    };
    let relative = path.to_string_lossy().replace('\\', "/");
    let Some((source_id, entry)) = manifest.iter().find(|(_, entry)| entry.path == relative)
    else {
        return;
    };
    if entry.import_settings.sub_assets.is_empty() {
        return;
    }
    ui.separator();
    ui.strong(format!("Sub-assets of {relative}"));
    for sub_asset in &entry.import_settings.sub_assets {
        if is_legacy_motion_clip_alias(source_id, sub_asset) {
            continue;
        }
        let (badge, kind) = match sub_asset.kind {
            engine::ImportedSubAssetKind::Mesh => ("[mesh]", Some(AssetKind::Mesh)),
            engine::ImportedSubAssetKind::Material => ("[mat]", Some(AssetKind::Material)),
            engine::ImportedSubAssetKind::Texture => ("[tex]", Some(AssetKind::Texture)),
            engine::ImportedSubAssetKind::Animation => ("[clip]", Some(AssetKind::AnimationClip)),
            _ => ("[sub]", None),
        };
        ui.horizontal(|ui| {
            ui.add_space(12.0);
            let target_label = sub_asset
                .target_model_source
                .as_deref()
                .and_then(|target| {
                    AssetId::from_stable_id(engine_authoring::StableId::new(target)).ok()
                })
                .and_then(|target| manifest.get(&target))
                .map(|entry| entry.name.as_deref().unwrap_or(&entry.path));
            let display_name = target_label.map_or_else(
                || sub_asset.name.clone(),
                |target| format!("{} — {target}", sub_asset.name),
            );
            let override_label = sub_asset_override_target(entry, sub_asset).map(|target| {
                AssetId::from_stable_id(engine_authoring::StableId::new(target))
                    .ok()
                    .and_then(|target| override_target_label(&target, manifest))
                    .unwrap_or_else(|| format!("missing: {target}"))
            });
            let display_name = override_label.map_or(display_name.clone(), |target| {
                format!("{display_name} [overridden -> {target}]")
            });
            let response = ui.add(
                egui::Label::new(format!("{badge} {display_name}"))
                    .sense(egui::Sense::click_and_drag()),
            );
            if let Some(kind) = kind {
                let stable = engine_authoring::StableId::new(&sub_asset.id);
                if let Ok(asset_id) = AssetId::from_stable_id(stable) {
                    response
                        .on_hover_text("Drag onto the Scene View or an Inspector field")
                        .dnd_set_drag_payload(DragPayload {
                            asset_id,
                            relative_path: path.clone(),
                            kind,
                            // A sub-asset has no file of its own, so it can be
                            // referenced but never relocated.
                            paths: Vec::new(),
                        });
                }
            }
        });
    }
}

pub(in crate::ui) enum AssetBrowserAction {
    Open(usize),
    Register(usize),
    Reimport(usize),
    InstantiatePrefab(usize),
    InstantiateModel(usize),
    CreatePrefabFromEntity {
        entity: EntityId,
        destination_folder: PathBuf,
    },
    RenameAsset(usize),
    MoveAsset(usize),
    TrashAsset(usize),
    NewUiDocument,
    NewScene,
    NewAnimationGraph {
        destination_folder: PathBuf,
    },
    NewAnimationSet {
        graph: Option<AssetId>,
        destination_folder: PathBuf,
    },
    NewMaterial,
    NewRhaiScript,
    NewRustScript,
    NewFolder,
    ShowInExplorer(PathBuf),
    AddMeshToScene(usize),
    RenameFolder(PathBuf),
    TrashFolder(PathBuf),
    MoveSelectionToFolder(PathBuf),
    /// Opens the Import Settings window for a registered glTF/GLB source
    /// (contact-bones override editing + contact interval display, AP-5).
    EditImportSettings(usize),
    /// Creates a `*.retarget.json` map for `source` (a glTF/GLB row) onto
    /// `target_source_id`'s skeleton (AP-5 creation flow for
    /// `anim.retarget_map_missing`).
    CreateRetargetMap {
        source: usize,
        target_source_id: AssetId,
    },
}

#[derive(Clone)]
struct AssetPathDragPayload {
    paths: Vec<PathBuf>,
}

/// Reports whether a file drag from the Asset Browser was released here.
///
/// A registered asset carries its paths inside [`DragPayload`] so the Scene
/// View and folder targets can both read one payload (egui keeps only one);
/// unregistered files use [`AssetPathDragPayload`]. Folder targets accept
/// either.
fn dropped_asset_paths(response: &egui::Response) -> bool {
    if let Some(payload) = response.dnd_release_payload::<DragPayload>() {
        return !payload.paths.is_empty();
    }
    response
        .dnd_release_payload::<AssetPathDragPayload>()
        .is_some_and(|payload| !payload.paths.is_empty())
}

/// Selects which project content occupies the Assets utility dock.
///
/// Runtime assets and Rust game code use separate full-height views so the
/// asset folder tree and asset grid never need an enclosing shared scroll area.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum ProjectBrowserTab {
    Assets,
}

/// Width of one asset grid cell.
pub(in crate::ui) const ASSET_GRID_CELL_WIDTH: f32 = 112.0;

/// Height of one asset grid cell.
///
/// The bottom dock's minimum height is derived from this so a resize cannot
/// be dragged below what a single row of assets needs.
pub(in crate::ui) const ASSET_GRID_CELL_HEIGHT: f32 = 108.0;

/// Renders the one physical project asset tree, including user-authored code.
#[allow(clippy::too_many_arguments)]
pub(in crate::ui) fn show_project_browser(
    ui: &mut egui::Ui,
    assets: &mut AssetBrowser,
    asset_search: &mut String,
    asset_thumbnails: &mut std::collections::BTreeMap<PathBuf, TexturePreview>,
    content_scroll_reset: &mut bool,
    active_tab: &mut ProjectBrowserTab,
    project: Option<&ProjectRoot>,
    manifest: &engine::AssetManifest,
    can_create_rust_script: bool,
) -> Option<AssetBrowserAction> {
    let Some(project) = project else {
        ui.label("No project open");
        return None;
    };

    *active_tab = ProjectBrowserTab::Assets;
    show_asset_browser(
        ui,
        assets,
        asset_search,
        asset_thumbnails,
        content_scroll_reset,
        Some(project.assets_root().as_path()),
        manifest,
        can_create_rust_script,
    )
}

/// Returns whether `folder` is a direct child of `parent` in the physical
/// asset-folder hierarchy.
pub(in crate::ui) fn is_direct_asset_folder_child(folder: &Path, parent: &Path) -> bool {
    !folder.as_os_str().is_empty() && folder.parent().unwrap_or(Path::new("")) == parent
}

/// Returns whether a folder owns at least one direct child folder and should
/// therefore display an expand/collapse affordance.
pub(in crate::ui) fn asset_folder_has_children(folder: &Path, folders: &[crate::AssetFolder]) -> bool {
    folders
        .iter()
        .any(|candidate| is_direct_asset_folder_child(&candidate.relative_path, folder))
}

/// Produces the full user-facing path of a folder for hover text.
fn asset_folder_hover_path(folder: &Path) -> String {
    if folder.as_os_str().is_empty() {
        "Assets".to_owned()
    } else {
        format!("Assets/{}", folder.display())
    }
}

/// Produces the user-facing final path component for a physical asset folder.
fn asset_folder_label(folder: &Path) -> String {
    if folder.as_os_str().is_empty() {
        "Assets".to_owned()
    } else {
        folder
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

/// Draws a geometrically centered disclosure triangle inside a fixed-size
/// button. Painting the triangle directly avoids font-specific baseline and
/// glyph-bounds differences that made the previous `v` / `>` text sit lower
/// than the adjacent folder label.
fn asset_folder_toggle_button(ui: &mut egui::Ui, collapsed: bool) -> egui::Response {
    let response = ui.add_sized(egui::vec2(20.0, 20.0), egui::Button::new(""));
    let center = response.rect.center();
    let points = if collapsed {
        vec![
            center + egui::vec2(-2.5, -4.0),
            center + egui::vec2(-2.5, 4.0),
            center + egui::vec2(3.5, 0.0),
        ]
    } else {
        vec![
            center + egui::vec2(-4.0, -2.5),
            center + egui::vec2(4.0, -2.5),
            center + egui::vec2(0.0, 3.5),
        ]
    };
    let color = if response.hovered() {
        ui.visuals().widgets.hovered.fg_stroke.color
    } else {
        ui.visuals().widgets.inactive.fg_stroke.color
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
    response
}

/// Renders the runtime-asset portion of the project browser.
///
/// Returns the index of the entry that was double-clicked, if any.  The
/// caller is responsible for resolving the path and opening the document.
///
/// When `assets_root` is `None` (no project open), a placeholder message is
/// shown.  Selection state is stored in `browser`.
#[allow(clippy::too_many_arguments)]
fn show_asset_browser(
    ui: &mut egui::Ui,
    browser: &mut AssetBrowser,
    asset_search: &mut String,
    asset_thumbnails: &mut std::collections::BTreeMap<PathBuf, TexturePreview>,
    content_scroll_reset: &mut bool,
    assets_root: Option<&std::path::Path>,
    manifest: &engine::AssetManifest,
    can_create_rust_script: bool,
) -> Option<AssetBrowserAction> {
    match assets_root {
        None => {
            ui.label("No project open");
            None
        }
        Some(root) => {
            let mut action = None;
            let mut refresh_requested = false;
            let folders = browser.folders().to_vec();
            let reveal_folder = browser.take_pending_reveal();
            control_row(ui, |ui| {
                ui.strong("Assets");
                ui.separator();
                ui.add(
                    egui::TextEdit::singleline(asset_search)
                        .hint_text("Search assets...")
                        .desired_width(220.0),
                );
                if ui.small_button("Clear").clicked() {
                    asset_search.clear();
                }
            });
            ui.separator();

            // The browser is deliberately split into a fixed-width tree and a
            // flexible content area. This keeps folder navigation visible
            // while the right side changes to show only the selected folder.
            let available_width = ui.available_width();
            // No enclosing vertical ScrollArea exists here. Both children
            // receive the same finite height and maintain independent offsets.
            let browser_height = ui.available_height().max(1.0);
            let tree_width = (available_width * 0.24).clamp(180.0, 280.0);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(tree_width, browser_height),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        ui.heading("Folders");
                        let tree_viewport = ui.available_rect_before_wrap();
                        egui::ScrollArea::vertical()
                            .id_salt("asset_folder_tree_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                // Same rule as the content pane: the tree
                                // background answers everywhere no folder row
                                // does, instead of only on a trailing strip.
                                let tree_background = ui.interact(
                                    tree_viewport,
                                    ui.id().with("asset_folder_tree_background"),
                                    egui::Sense::click(),
                                );
                                for folder in &folders {
                                    if !browser.folder_row_is_visible(&folder.relative_path) {
                                        continue;
                                    }
                                    let label = asset_folder_label(&folder.relative_path);
                                    let selected =
                                        browser.selected_folder() == folder.relative_path;
                                    let has_children = asset_folder_has_children(
                                        &folder.relative_path,
                                        &folders,
                                    );
                                    let collapsed =
                                        browser.is_folder_collapsed(&folder.relative_path);
                                    let (toggle_clicked, response) = ui
                                        .allocate_ui_with_layout(
                                            egui::vec2(ui.available_width(), 24.0),
                                            egui::Layout::left_to_right(egui::Align::Center),
                                            |ui| {
                                            ui.add_space(folder.depth as f32 * 14.0);
                                            let toggle_clicked = if has_children {
                                                asset_folder_toggle_button(ui, collapsed)
                                                    .on_hover_text(if collapsed {
                                                        "Expand folder"
                                                    } else {
                                                        "Collapse folder"
                                                    })
                                                    .clicked()
                                            } else {
                                                ui.add_sized(
                                                    egui::vec2(20.0, 20.0),
                                                    egui::Label::new(""),
                                                );
                                                false
                                            };
                                            let response = ui.selectable_label(selected, label);
                                            (toggle_clicked, response)
                                        },
                                        )
                                        .inner;
                                    if toggle_clicked {
                                        browser.toggle_folder_collapsed(&folder.relative_path);
                                    }
                                    if reveal_folder.as_deref() == Some(&folder.relative_path) {
                                        response.scroll_to_me(None);
                                    }
                                    if response.clicked()
                                        && !selected
                                        && browser
                                            .set_selected_folder(folder.relative_path.clone())
                                    {
                                        *content_scroll_reset = true;
                                    }
                                    if dropped_asset_paths(&response) {
                                        action = Some(AssetBrowserAction::MoveSelectionToFolder(
                                            folder.relative_path.clone(),
                                        ));
                                    }
                                    if let Some(payload) =
                                        response.dnd_release_payload::<HierarchyDragPayload>()
                                    {
                                        action = Some(AssetBrowserAction::CreatePrefabFromEntity {
                                            entity: payload.entity.clone(),
                                            destination_folder: folder.relative_path.clone(),
                                        });
                                    }
                                    response.context_menu(|ui| {
                                        if ui.button("Create Child Folder...").clicked() {
                                            browser
                                                .set_selected_folder(folder.relative_path.clone());
                                            *content_scroll_reset = true;
                                            action = Some(AssetBrowserAction::NewFolder);
                                            ui.close();
                                        }
                                        if ui.button("Create Animation Graph").clicked() {
                                            action = Some(AssetBrowserAction::NewAnimationGraph {
                                                destination_folder: folder.relative_path.clone(),
                                            });
                                            ui.close();
                                        }
                                        if !folder.relative_path.as_os_str().is_empty() {
                                            if ui.button("Rename Folder...").clicked() {
                                                action = Some(AssetBrowserAction::RenameFolder(
                                                    folder.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Delete Folder...").clicked() {
                                                action = Some(AssetBrowserAction::TrashFolder(
                                                    folder.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                        }
                                    });
                                }
                                ui.add_sized(
                                    [ui.available_width(), 32.0],
                                    egui::Label::new(
                                        egui::RichText::new("Right-click to create a folder")
                                            .small()
                                            .color(egui::Color32::GRAY),
                                    ),
                                );
                                tree_background.context_menu(|ui| {
                                    if ui.button("Create Folder...").clicked() {
                                        action = Some(AssetBrowserAction::NewFolder);
                                        ui.close();
                                    }
                                });
                            });
                    },
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(
                        (available_width - tree_width - 12.0).max(160.0),
                        browser_height,
                    ),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        control_row(ui, |ui| {
                            // Each step of the working path navigates to that
                            // level, so a nested folder can be left one
                            // component at a time without hunting for the row
                            // in the tree.
                            let breadcrumbs =
                                crate::asset_browser::folder_breadcrumbs(browser.selected_folder());
                            let last = breadcrumbs.len().saturating_sub(1);
                            for (index, breadcrumb) in breadcrumbs.iter().enumerate() {
                                if index > 0 {
                                    ui.label("/");
                                }
                                if index == last {
                                    ui.strong(&breadcrumb.label);
                                    continue;
                                }
                                if ui
                                    .link(&breadcrumb.label)
                                    .on_hover_text(format!(
                                        "Open {}",
                                        asset_folder_hover_path(&breadcrumb.folder)
                                    ))
                                    .clicked()
                                    && browser.set_selected_folder(breadcrumb.folder.clone())
                                {
                                    *content_scroll_reset = true;
                                }
                            }
                            ui.separator();
                            if ui.small_button("New Folder").clicked() {
                                action = Some(AssetBrowserAction::NewFolder);
                            }
                            if ui.small_button("Refresh").clicked() {
                                refresh_requested = true;
                            }
                            let selected_count = browser.selected_paths().count();
                            if let Some(primary) = browser.selected().filter(|_| selected_count > 0) {
                                ui.separator();
                                ui.label(format!("{selected_count} selected"));
                                let move_label = if selected_count > 1 {
                                    "Move Selected..."
                                } else {
                                    "Move..."
                                };
                                if ui.small_button(move_label).clicked() {
                                    action = Some(AssetBrowserAction::MoveAsset(primary));
                                }
                                let delete_label = if selected_count > 1 {
                                    "Delete Selected..."
                                } else {
                                    "Delete..."
                                };
                                if ui.small_button(delete_label).clicked() {
                                    action = Some(AssetBrowserAction::TrashAsset(primary));
                                }
                            }
                        });
                        ui.separator();

                        // Captured before the scroll area so the background
                        // covers the visible viewport rather than the scrolled
                        // content origin.
                        let content_viewport = ui.available_rect_before_wrap();
                        let mut content_scroll = egui::ScrollArea::vertical()
                            .id_salt("asset_content_scroll")
                            .auto_shrink([false, false]);
                        if std::mem::take(content_scroll_reset) {
                            content_scroll = content_scroll.vertical_scroll_offset(0.0);
                        }
                        content_scroll.show(ui, |ui| {
                            // The folder background owns the create menu and the
                            // Entity drop, so every gap between tiles, beside a
                            // short last row, and below the rows answers alike.
                            // Registering it before the rows keeps the tiles on
                            // top: egui resolves a click to the last widget that
                            // contains the pointer, so an asset still wins over
                            // the background wherever an asset actually is.
                            let background = ui.interact(
                                content_viewport,
                                ui.id().with("asset_content_background"),
                                egui::Sense::click(),
                            );
                            let search = asset_search.trim().to_ascii_lowercase();
                            let selected_folder = browser.selected_folder().to_path_buf();
                            let visible_folders = folders
                                .iter()
                                .filter(|folder| {
                                    is_direct_asset_folder_child(
                                        &folder.relative_path,
                                        &selected_folder,
                                    )
                                })
                                .filter(|folder| {
                                    let label = asset_folder_label(&folder.relative_path)
                                        .to_ascii_lowercase();
                                    search.is_empty()
                                        || label.contains(&search)
                                        || folder
                                            .relative_path
                                            .to_string_lossy()
                                            .to_ascii_lowercase()
                                            .contains(&search)
                                })
                                .cloned()
                                .collect::<Vec<_>>();
                            let visible_entries = browser
                                .visible_entry_indices()
                                .into_iter()
                                .filter_map(|index| {
                                    browser
                                        .entries()
                                        .get(index)
                                        .cloned()
                                        .map(|entry| (index, entry))
                                })
                                .filter(|(_, entry)| {
                                    search.is_empty()
                                        || entry.display_name.to_ascii_lowercase().contains(&search)
                                        || entry
                                            .relative_path
                                            .to_string_lossy()
                                            .to_ascii_lowercase()
                                            .contains(&search)
                                })
                                .collect::<Vec<_>>();

                            if visible_folders.is_empty() && visible_entries.is_empty() {
                                ui.label("(empty folder)");
                            }

                            // Compute model-only choices once so every row's
                            // context menu reuses the same filtered list.
                            let skeleton_source_choices =
                                retarget_map_model_source_choices(manifest);

                            let tile_width = ASSET_GRID_CELL_WIDTH;
                            let tile_height = ASSET_GRID_CELL_HEIGHT;
                            let columns =
                                ((ui.available_width() / tile_width).floor() as usize).max(1);
                            for row in visible_folders.chunks(columns) {
                                ui.horizontal(|ui| {
                                    for folder in row {
                                        let selected = browser.selected_folder_tile()
                                            == Some(folder.relative_path.as_path());
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(tile_width, tile_height),
                                            egui::Sense::click_and_drag(),
                                        );
                                        let preview_rect = egui::Rect::from_center_size(
                                            egui::pos2(rect.center().x, rect.top() + 42.0),
                                            egui::vec2(66.0, 66.0),
                                        );
                                        ui.painter().rect_filled(
                                            preview_rect.shrink2(egui::vec2(5.0, 12.0)),
                                            5.0,
                                            egui::Color32::from_rgb(184, 142, 48),
                                        );
                                        ui.painter().text(
                                            preview_rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "DIR",
                                            egui::FontId::proportional(20.0),
                                            egui::Color32::WHITE,
                                        );
                                        ui.painter().with_clip_rect(rect.shrink(4.0)).text(
                                            egui::pos2(rect.center().x, rect.bottom() - 7.0),
                                            egui::Align2::CENTER_BOTTOM,
                                            asset_folder_label(&folder.relative_path),
                                            egui::FontId::proportional(11.0),
                                            egui::Color32::WHITE,
                                        );
                                        if selected {
                                            ui.painter().rect_stroke(
                                                rect,
                                                4.0,
                                                egui::Stroke::new(
                                                    1.5_f32,
                                                    egui::Color32::LIGHT_BLUE,
                                                ),
                                                egui::StrokeKind::Inside,
                                            );
                                        }

                                        if response.clicked() {
                                            browser.select_folder_tile(&folder.relative_path);
                                        }
                                        if response.double_clicked()
                                            && browser.set_selected_folder(
                                                folder.relative_path.clone(),
                                            )
                                        {
                                            *content_scroll_reset = true;
                                        }
                                        if dropped_asset_paths(&response) {
                                            action = Some(
                                                AssetBrowserAction::MoveSelectionToFolder(
                                                    folder.relative_path.clone(),
                                                ),
                                            );
                                        }
                                        if let Some(payload) =
                                            response.dnd_release_payload::<HierarchyDragPayload>()
                                        {
                                            action = Some(
                                                AssetBrowserAction::CreatePrefabFromEntity {
                                                    entity: payload.entity.clone(),
                                                    destination_folder: folder
                                                        .relative_path
                                                        .clone(),
                                                },
                                            );
                                        }
                                        if response.secondary_clicked() {
                                            browser.select_folder_tile(&folder.relative_path);
                                        }
                                        response.context_menu(|ui| {
                                            if ui.button("Open Folder").clicked() {
                                                if browser.set_selected_folder(
                                                    folder.relative_path.clone(),
                                                ) {
                                                    *content_scroll_reset = true;
                                                }
                                                ui.close();
                                            }
                                            ui.menu_button("Create", |ui| {
                                                ui.menu_button("General", |ui| {
                                                    if ui.button("Child Folder...").clicked() {
                                                        browser.set_selected_folder(
                                                            folder.relative_path.clone(),
                                                        );
                                                        *content_scroll_reset = true;
                                                        action = Some(AssetBrowserAction::NewFolder);
                                                        ui.close();
                                                    }
                                                    if ui.button("Scene").clicked() {
                                                        action = Some(AssetBrowserAction::NewScene);
                                                        ui.close();
                                                    }
                                                });
                                                ui.menu_button("Rendering", |ui| {
                                                    if ui.button("Material").clicked() {
                                                        action = Some(AssetBrowserAction::NewMaterial);
                                                        ui.close();
                                                    }
                                                });
                                                ui.menu_button("Animation", |ui| {
                                                    if ui.button("Animation Graph").clicked() {
                                                        action = Some(
                                                            AssetBrowserAction::NewAnimationGraph {
                                                                destination_folder: folder
                                                                    .relative_path
                                                                    .clone(),
                                                            },
                                                        );
                                                        ui.close();
                                                    }
                                                    if ui.button("Animation Set").clicked() {
                                                        action = Some(
                                                            AssetBrowserAction::NewAnimationSet {
                                                                graph: None,
                                                                destination_folder: folder
                                                                    .relative_path
                                                                    .clone(),
                                                            },
                                                        );
                                                        ui.close();
                                                    }
                                                });
                                                ui.menu_button("UI", |ui| {
                                                    if ui.button("UI Document").clicked() {
                                                        action = Some(
                                                            AssetBrowserAction::NewUiDocument,
                                                        );
                                                        ui.close();
                                                    }
                                                });
                                                ui.menu_button("Scripting", |ui| {
                                                    if ui.button("Rhai Script...").clicked() {
                                                        action = Some(
                                                            AssetBrowserAction::NewRhaiScript,
                                                        );
                                                        ui.close();
                                                    }
                                                    if ui
                                                        .add_enabled(
                                                            can_create_rust_script,
                                                            egui::Button::new("Rust Script..."),
                                                        )
                                                        .on_disabled_hover_text(
                                                            "Initialize the Rust Game first",
                                                        )
                                                        .clicked()
                                                    {
                                                        browser.set_selected_folder(
                                                            folder.relative_path.clone(),
                                                        );
                                                        *content_scroll_reset = true;
                                                        action = Some(
                                                            AssetBrowserAction::NewRustScript,
                                                        );
                                                        ui.close();
                                                    }
                                                });
                                            });
                                            if ui.button("Rename Folder...").clicked() {
                                                action = Some(AssetBrowserAction::RenameFolder(
                                                    folder.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Delete Folder...").clicked() {
                                                action = Some(AssetBrowserAction::TrashFolder(
                                                    folder.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                        });
                                    }
                                });
                            }
                            for row in visible_entries.chunks(columns) {
                                ui.horizontal(|ui| {
                                    for (index, entry) in row {
                                        let selected = browser
                                            .selected_paths()
                                            .any(|path| path == &entry.relative_path);
                                        let registered_asset =
                                            manifest.iter().find(|(_, manifest_entry)| {
                                                normalize_manifest_path(&manifest_entry.path)
                                                    == normalize_manifest_path(
                                                        &entry.relative_path.to_string_lossy(),
                                                    )
                                            });
                                        let registered = registered_asset.is_some();
                                        let humanoid_configurable =
                                            registered_asset.is_some_and(|(_, manifest_entry)| {
                                                !manifest_entry
                                                    .import_settings
                                                    .skeleton_records
                                                    .is_empty()
                                            });
                                        let gltf_source = engine::asset_path_matches_kind(
                                            engine::AssetKind::GltfSource,
                                            &entry.relative_path,
                                        );
                                        // Reimport and Import Settings apply
                                        // to `.vmd` motions too; Create
                                        // Retarget Map stays model-only,
                                        // since a motion owns no rig to map
                                        // between.
                                        let import_source =
                                            is_importable_source_path(&entry.relative_path);
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(tile_width, tile_height),
                                            egui::Sense::click_and_drag(),
                                        );

                                        // Texture thumbnails use the same decoder as
                                        // the full preview window, but remain cached
                                        // for the lifetime of the open editor.
                                        let thumbnail = if entry.kind == AssetKind::Texture {
                                            let key = entry.relative_path.clone();
                                            if !asset_thumbnails.contains_key(&key)
                                                && let Ok(preview) = load_texture_preview(
                                                    ui.ctx(),
                                                    &root.join(&key),
                                                    key.clone(),
                                                ) {
                                                    asset_thumbnails.insert(key.clone(), preview);
                                                }
                                            asset_thumbnails.get(&key)
                                        } else {
                                            None
                                        };
                                        let preview_rect = egui::Rect::from_center_size(
                                            egui::pos2(rect.center().x, rect.top() + 42.0),
                                            egui::vec2(66.0, 66.0),
                                        );
                                        if let Some(thumbnail) = thumbnail {
                                            ui.painter().image(
                                                thumbnail.texture.id(),
                                                preview_rect,
                                                egui::Rect::from_min_max(
                                                    egui::pos2(0.0, 0.0),
                                                    egui::pos2(1.0, 1.0),
                                                ),
                                                egui::Color32::WHITE,
                                            );
                                        } else {
                                            ui.painter().text(
                                                preview_rect.center(),
                                                egui::Align2::CENTER_CENTER,
                                                asset_kind_icon(entry.kind),
                                                egui::FontId::proportional(38.0),
                                                asset_kind_color(entry.kind),
                                            );
                                        }
                                        ui.painter().with_clip_rect(rect.shrink(4.0)).text(
                                            egui::pos2(rect.center().x, rect.bottom() - 7.0),
                                            egui::Align2::CENTER_BOTTOM,
                                            &entry.display_name,
                                            egui::FontId::proportional(11.0),
                                            egui::Color32::WHITE,
                                        );
                                        if selected {
                                            ui.painter().rect_stroke(
                                                rect,
                                                4.0,
                                                egui::Stroke::new(1.5_f32, egui::Color32::LIGHT_BLUE),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
                                        if registered {
                                            ui.painter().text(
                                                rect.right_top() - egui::vec2(8.0, -8.0),
                                                egui::Align2::RIGHT_TOP,
                                                "✓",
                                                egui::FontId::proportional(14.0),
                                                egui::Color32::LIGHT_GREEN,
                                            );
                                        }

                                        if response.clicked() {
                                            let additive = ui.input(|input| {
                                                input.modifiers.ctrl || input.modifiers.command
                                            });
                                            browser.select_path(&entry.relative_path, additive);
                                        }
                                        if response.double_clicked() {
                                            action = Some(AssetBrowserAction::Open(*index));
                                        }
                                        let mut registered_drag = None;
                                        if let Some((asset_id, _)) = registered_asset {
                                            // Meshes drop into the Scene View to
                                            // spawn entities; materials/textures
                                            // drop onto entities and Inspector
                                            // fields to assign references. A
                                            // model source drops as its whole
                                            // generated hierarchy (ADR 0075).
                                            let draggable = matches!(
                                                entry.kind,
                                                AssetKind::Mesh
                                                    | AssetKind::Material
                                                    | AssetKind::Texture
                                                    | AssetKind::AnimationSet
                                                    | AssetKind::AnimationClip
                                            );
                                            registered_drag = draggable.then(|| asset_id.clone());
                                        }
                                        let drag_paths = if selected {
                                            browser.selected_paths().cloned().collect::<Vec<_>>()
                                        } else {
                                            vec![entry.relative_path.clone()]
                                        };
                                        // egui holds one payload per drag, so
                                        // setting a second one here would
                                        // silently replace the first and break
                                        // whichever drop target reads it.
                                        match registered_drag {
                                            Some(asset_id) => response.clone().dnd_set_drag_payload(
                                                DragPayload {
                                                    asset_id,
                                                    relative_path: entry.relative_path.clone(),
                                                    kind: entry.kind,
                                                    paths: drag_paths,
                                                },
                                            ),
                                            None => response.clone().dnd_set_drag_payload(
                                                AssetPathDragPayload { paths: drag_paths },
                                            ),
                                        }
                                        if let Some(payload) =
                                            response.dnd_release_payload::<HierarchyDragPayload>()
                                        {
                                            action =
                                                Some(AssetBrowserAction::CreatePrefabFromEntity {
                                                    entity: payload.entity.clone(),
                                                    destination_folder: browser
                                                        .selected_folder()
                                                        .to_path_buf(),
                                                });
                                        }
                                        let selected_count = if selected {
                                            browser.selected_paths().count()
                                        } else {
                                            1
                                        };
                                        response.context_menu(|ui| {
                                            if ui.button("Open").clicked() {
                                                action = Some(AssetBrowserAction::Open(*index));
                                                ui.close();
                                            }
                                            let registerable =
                                                is_registerable_asset(&entry.relative_path);
                                            if registerable
                                                && !registered
                                                && ui.button("Register Asset").clicked()
                                            {
                                                action = Some(AssetBrowserAction::Register(*index));
                                                ui.close();
                                            }
                                            if registered
                                                && import_source
                                                && ui.button("Reimport").clicked()
                                            {
                                                action = Some(AssetBrowserAction::Reimport(*index));
                                                ui.close();
                                            }
                                            let import_settings_label = if humanoid_configurable {
                                                "Configure Humanoid / Import Settings..."
                                            } else {
                                                "Edit Import Settings..."
                                            };
                                            if registered
                                                && import_source
                                                && ui.button(import_settings_label).clicked()
                                            {
                                                action = Some(
                                                    AssetBrowserAction::EditImportSettings(*index),
                                                );
                                                ui.close();
                                            }
                                            if registered && gltf_source {
                                                let other_targets: Vec<&(AssetId, String)> =
                                                    skeleton_source_choices
                                                        .iter()
                                                        .filter(|(id, _)| {
                                                            registered_asset
                                                                .is_none_or(|(this_id, _)| this_id != id)
                                                        })
                                                        .collect();
                                                if !other_targets.is_empty() {
                                                    ui.menu_button(
                                                        "Create Retarget Map",
                                                        |ui| {
                                                            for (target_id, label) in other_targets {
                                                                if ui.button(label).clicked() {
                                                                    action = Some(
                                                                        AssetBrowserAction::CreateRetargetMap {
                                                                            source: *index,
                                                                            target_source_id: target_id.clone(),
                                                                        },
                                                                    );
                                                                    ui.close();
                                                                }
                                                            }
                                                        },
                                                    );
                                                }
                                            }
                                            if entry.kind == AssetKind::Graph
                                                && crate::ui::inspector::manifest_path_matches_asset_kind(
                                                    engine::AssetKind::AnimationGraph,
                                                    &entry.relative_path,
                                                    Some(root),
                                                )
                                                && let Some((graph_id, _)) = registered_asset
                                                    && ui
                                                        .button("Create Animation Set")
                                                        .clicked()
                                                    {
                                                        action = Some(
                                                            AssetBrowserAction::NewAnimationSet {
                                                                graph: Some(graph_id.clone()),
                                                                destination_folder: entry
                                                                    .relative_path
                                                                    .parent()
                                                                    .unwrap_or(Path::new(""))
                                                                    .to_path_buf(),
                                                            },
                                                        );
                                                        ui.close();
                                                    }
                                            if entry.kind == AssetKind::Prefab
                                                && ui.button("Instantiate in Scene").clicked()
                                            {
                                                action = Some(
                                                    AssetBrowserAction::InstantiatePrefab(*index),
                                                );
                                                ui.close();
                                            }
                                            // The model source row is what the
                                            // author places; the prefab behind
                                            // it stays hidden (ADR 0075).
                                            if registered
                                                && gltf_source
                                                && ui.button("Instantiate in Scene").clicked()
                                            {
                                                action = Some(
                                                    AssetBrowserAction::InstantiateModel(*index),
                                                );
                                                ui.close();
                                            }
                                            if entry.kind == AssetKind::Mesh
                                                && registered
                                                && !gltf_source
                                                && ui.button("Add to Scene").clicked()
                                            {
                                                action = Some(AssetBrowserAction::AddMeshToScene(
                                                    *index,
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Show in Explorer").clicked() {
                                                action = Some(AssetBrowserAction::ShowInExplorer(
                                                    entry.relative_path.clone(),
                                                ));
                                                ui.close();
                                            }
                                            ui.separator();
                                            if ui.button("Rename...").clicked() {
                                                action =
                                                    Some(AssetBrowserAction::RenameAsset(*index));
                                                ui.close();
                                            }
                                            let move_label = if selected_count > 1 {
                                                format!("Move {selected_count} Assets...")
                                            } else {
                                                "Move...".to_owned()
                                            };
                                            if ui.button(move_label).clicked() {
                                                action =
                                                    Some(AssetBrowserAction::MoveAsset(*index));
                                                ui.close();
                                            }
                                            let delete_label = if selected_count > 1 {
                                                format!("Delete {selected_count} Assets...")
                                            } else {
                                                "Delete...".to_owned()
                                            };
                                            if ui.button(delete_label).clicked() {
                                                action =
                                                    Some(AssetBrowserAction::TrashAsset(*index));
                                                ui.close();
                                            }
                                        });
                                    }
                                });
                            }

                            show_selected_gltf_sub_assets(ui, browser, manifest);

                            // A hint only: it must not sense clicks, or it would
                            // sit on top of the background and reintroduce a
                            // strip where the create menu behaves differently.
                            ui.add_sized(
                                [ui.available_width(), 36.0],
                                egui::Label::new(
                                    egui::RichText::new(
                                        "Right-click to create; drop an Entity here to create a Prefab",
                                    )
                                    .small()
                                    .color(egui::Color32::GRAY),
                                ),
                            );
                            // Read last, so a drop that a folder tile or asset
                            // row already claimed has taken the payload and only
                            // genuinely empty drops reach the selected folder.
                            if let Some(payload) =
                                background.dnd_release_payload::<HierarchyDragPayload>()
                            {
                                action = Some(AssetBrowserAction::CreatePrefabFromEntity {
                                    entity: payload.entity.clone(),
                                    destination_folder: browser.selected_folder().to_path_buf(),
                                });
                            }
                            background.context_menu(|ui| {
                                ui.menu_button("Create", |ui| {
                                    ui.menu_button("General", |ui| {
                                        if ui.button("Folder...").clicked() {
                                            action = Some(AssetBrowserAction::NewFolder);
                                            ui.close();
                                        }
                                        if ui.button("Scene").clicked() {
                                            action = Some(AssetBrowserAction::NewScene);
                                            ui.close();
                                        }
                                    });
                                    ui.menu_button("Rendering", |ui| {
                                        if ui.button("Material").clicked() {
                                            action = Some(AssetBrowserAction::NewMaterial);
                                            ui.close();
                                        }
                                    });
                                    ui.menu_button("Animation", |ui| {
                                        if ui.button("Animation Graph").clicked() {
                                            action = Some(
                                                AssetBrowserAction::NewAnimationGraph {
                                                    destination_folder: browser
                                                        .selected_folder()
                                                        .to_path_buf(),
                                                },
                                            );
                                            ui.close();
                                        }
                                        if ui.button("Animation Set").clicked() {
                                            action = Some(AssetBrowserAction::NewAnimationSet {
                                                graph: None,
                                                destination_folder: browser
                                                    .selected_folder()
                                                    .to_path_buf(),
                                            });
                                            ui.close();
                                        }
                                    });
                                    ui.menu_button("UI", |ui| {
                                        if ui.button("UI Document").clicked() {
                                            action = Some(AssetBrowserAction::NewUiDocument);
                                            ui.close();
                                        }
                                    });
                                    ui.menu_button("Scripting", |ui| {
                                        if ui.button("Rhai Script...").clicked() {
                                            action = Some(AssetBrowserAction::NewRhaiScript);
                                            ui.close();
                                        }
                                        if ui
                                            .add_enabled(
                                                can_create_rust_script,
                                                egui::Button::new("Rust Script..."),
                                            )
                                            .on_disabled_hover_text(
                                                "Initialize the Rust Game first",
                                            )
                                            .clicked()
                                        {
                                            action = Some(AssetBrowserAction::NewRustScript);
                                            ui.close();
                                        }
                                    });
                                });
                                if ui.button("Refresh").clicked() {
                                    refresh_requested = true;
                                    ui.close();
                                }
                            });
                        });
                    },
                );
            });
            if refresh_requested {
                let previous_folder = browser.selected_folder().to_path_buf();
                browser.refresh(root);
                if browser.selected_folder() != previous_folder {
                    *content_scroll_reset = true;
                }
                asset_thumbnails.retain(|path, _| root.join(path).is_file());
            }
            action
        }
    }
}

/// Returns the compact visual symbol used for an asset tile when no raster
/// thumbnail is available. The symbol keeps the browser readable even for
/// formats that require a renderer-backed preview.
pub(in crate::ui) fn asset_kind_icon(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Scene => "▣",
        AssetKind::Graph | AssetKind::GraphView => "◇",
        AssetKind::AnimationSet => "◎",
        AssetKind::AnimationClip => "▶",
        AssetKind::MotionSource => "↝",
        AssetKind::Texture => "▤",
        AssetKind::Mesh => "△",
        AssetKind::Audio => "♪",
        AssetKind::Material => "◈",
        AssetKind::Prefab => "◆",
        AssetKind::UiDocument => "▦",
        AssetKind::NavMesh => "⌁",
        AssetKind::RetargetMap => "⇄",
        AssetKind::Script
        | AssetKind::RustComponent
        | AssetKind::RustResource
        | AssetKind::RustSystem
        | AssetKind::RustModule => "‹›",
    }
}

/// Returns a stable accent color for each asset family.
pub(in crate::ui) fn asset_kind_color(kind: AssetKind) -> egui::Color32 {
    match kind {
        AssetKind::Texture => egui::Color32::from_rgb(100, 190, 255),
        AssetKind::Mesh => egui::Color32::from_rgb(255, 190, 90),
        AssetKind::Prefab => egui::Color32::from_rgb(220, 130, 255),
        AssetKind::Scene => egui::Color32::from_rgb(120, 220, 170),
        AssetKind::Material => egui::Color32::from_rgb(255, 150, 150),
        AssetKind::Audio => egui::Color32::from_rgb(250, 220, 110),
        AssetKind::Graph | AssetKind::GraphView => egui::Color32::from_rgb(150, 180, 255),
        AssetKind::AnimationSet => egui::Color32::from_rgb(190, 150, 255),
        AssetKind::AnimationClip | AssetKind::MotionSource => {
            egui::Color32::from_rgb(120, 210, 255)
        }
        _ => egui::Color32::from_gray(180),
    }
}
