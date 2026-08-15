//! Integration-style unit tests for the editor UI shell.

use super::hierarchy::release_drag_payload_in_rect;
use super::viewport::dropped_asset_is_model_source;
use super::*;
use std::collections::BTreeSet;
use std::io::Cursor;

/// The unified toolbar must keep the shell launcher clear while document tabs
/// remain on one horizontal row.
#[test]
fn unified_toolbar_reserves_launcher_space_and_does_not_wrap_tabs() {
    let context = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(520.0, 40.0),
        )),
        ..egui::RawInput::default()
    };
    let mut available_width = None;
    let mut toolbar_rect = None;
    let mut tab_tops = Vec::new();

    let _ = context.run_ui(input, |ui| {
        available_width = Some(ui.available_width());

        let response = show_main_toolbar_content(ui, |ui| {
            let _ = ui.button("Save");
            ui.separator();

            show_toolbar_document_tab_strip(ui, |ui| {
                ui.horizontal(|ui| {
                    for index in 0..12 {
                        tab_tops.push(
                            ui.button(format!("Scene {index}"))
                                .rect
                                .top()
                                .round() as i32,
                        );
                    }
                });
            });
        });

        toolbar_rect = Some(response.response.rect);
    });

    let available_width = available_width.expect("root toolbar width must be measured");
    let toolbar_rect = toolbar_rect.expect("toolbar content must be allocated");
    let expected_width =
        (available_width - AUTHORING_TOOLS_LAUNCHER_RESERVED_WIDTH).max(0.0);

    assert!(
        (toolbar_rect.width() - expected_width).abs() <= 1.0,
        "toolbar content width {} did not preserve the {} point launcher reserve",
        toolbar_rect.width(),
        AUTHORING_TOOLS_LAUNCHER_RESERVED_WIDTH
    );

    let rows = tab_tops.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(
        rows.len(),
        1,
        "overflowing document tabs wrapped onto multiple toolbar rows"
    );
}

/// Confirms each authoring surface keeps only the navigation it needs.
#[test]
fn left_dock_visibility_follows_active_authoring_surface() {
    // A scene needs Hierarchy even when it was opened outside a project.
    assert!(should_show_left_dock(false, true, false));

    // A project-only or graph workspace keeps Systems reachable.
    assert!(should_show_left_dock(true, false, false));

    // UI Builder owns a dedicated UI hierarchy and must use the reclaimed
    // width for its palette and responsive preview instead.
    assert!(!should_show_left_dock(true, false, true));

    // The project hub should not create an empty navigation dock.
    assert!(!should_show_left_dock(false, false, false));
}

/// Reproduces the long unbroken text that previously enlarged a clipped
/// left-panel response and left a black strip before the central panel.
#[test]
fn left_dock_long_text_does_not_create_clipped_layout_gap() {
    // A headless egui frame is sufficient to verify panel geometry without
    // opening a native window or creating a renderer surface.
    let context = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 600.0),
        )),
        ..egui::RawInput::default()
    };
    let mut measured_left_width = None;
    let mut measured_central_left = None;

    let _ = context.run_ui(input, |ui| {
        let maximum_width = left_dock_max_width(ui.available_width());
        let left_response = show_primary_left_dock_panel(ui, maximum_width, |ui| {
            ui.horizontal(|ui| {
                // System IDs contain no natural spaces, so a repeated ID
                // is the strictest regression case for dock-local wrapping.
                ui.monospace("engine.extremely_long_system_identifier.".repeat(32));
            });
        });
        measured_left_width = Some(left_response.response.rect.width());

        let central_response = egui::CentralPanel::default().show_inside(ui, |_ui| {
            // Only the central panel's starting coordinate is relevant.
        });
        measured_central_left = Some(central_response.response.rect.left());
    });

    let left_width = measured_left_width.expect("left dock must be laid out");
    let central_left = measured_central_left.expect("central panel must be laid out");

    // The response rect, not merely the paint clip, must respect the limit;
    // otherwise egui advances the central panel past an invisible region.
    assert!(
        left_width <= left_dock_max_width(900.0) + 1.0,
        "left dock width {left_width} exceeded its window-relative limit"
    );

    // With a zero-origin test screen, the central panel should begin at the
    // left dock's measured width, allowing only UI coordinate rounding.
    assert!(
        (central_left - left_width).abs() <= 1.0,
        "central panel began at {central_left}, leaving a gap after left dock width {left_width}"
    );
}

/// A widened Inspector must paint out to the window edge even when its body
/// is short.
///
/// egui measures a panel by its contents, and a right-hand panel grows from
/// its left edge, so a narrow Inspector body used to leave the strip between
/// the contents and the window edge unpainted and shrink the dock back to its
/// minimum on the next frame.
#[test]
fn widened_inspector_paints_and_keeps_its_full_width_with_short_content() {
    let context = egui::Context::default();
    let screen_width = 900.0;
    let resized_width = 520.0;
    context.data_mut(|data| {
        data.insert_persisted(
            egui::Id::new("inspector_panel"),
            egui::PanelState {
                rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(resized_width, 600.0)),
            },
        );
    });
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(screen_width, 600.0),
        )),
        ..egui::RawInput::default()
    };
    let mut measured_inspector = None;
    let mut measured_central_right = None;

    let _ = context.run_ui(input, |ui| {
        let maximum_width = inspector_max_width(ui.available_width());
        let inspector_response = show_inspector_panel(ui, maximum_width, |ui| {
            // The graph Inspector with nothing selected is the shortest body
            // the dock ever draws, and the one that exposed the gap.
            ui.heading("Inspector");
            ui.label("Select a node or transition to edit it.");
        });
        measured_inspector = Some(inspector_response.response.rect);

        let central_response = egui::CentralPanel::default().show_inside(ui, |_ui| {});
        measured_central_right = Some(central_response.response.rect.right());
    });

    let inspector = measured_inspector.expect("Inspector must be laid out");
    let central_right = measured_central_right.expect("central panel must be laid out");

    // The painted frame, not merely the reserved rectangle, must span the
    // dragged width; anything narrower is the unpainted strip.
    assert!(
        (inspector.width() - resized_width).abs() <= 1.0,
        "short Inspector content painted {} points of a {resized_width} point dock",
        inspector.width()
    );
    assert!(
        (inspector.right() - screen_width).abs() <= 1.0,
        "Inspector ended at {} instead of the window edge {screen_width}",
        inspector.right()
    );
    assert!(
        (central_right - inspector.left()).abs() <= 1.0,
        "central panel ended at {central_right}, leaving a gap before Inspector left edge {}",
        inspector.left()
    );

    // The stored width is what the next frame restores, so it has to survive
    // a frame of short content rather than springing back to the minimum.
    let persisted = context
        .data_mut(|data| data.get_persisted::<egui::PanelState>(egui::Id::new("inspector_panel")))
        .expect("Inspector must persist its size");
    assert!(
        (persisted.rect.width() - resized_width).abs() <= 1.0,
        "persisted width {resized_width} became {}",
        persisted.rect.width()
    );
}

/// An oversized descendant must not become input to the resizable Inspector.
///
/// This uses a deliberately hostile desired width across several frames because
/// the original failure accumulated: each frame persisted a wider dock and
/// moved the central workspace boundary farther left on the next frame.
#[test]
fn oversized_inspector_child_cannot_change_the_dock_or_workspace_width() {
    let context = egui::Context::default();
    let screen_width = 900.0;
    let inspector_width = INSPECTOR_DEFAULT_WIDTH;
    context.data_mut(|data| {
        data.insert_persisted(
            egui::Id::new("inspector_panel"),
            egui::PanelState {
                rect: egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(inspector_width, 600.0),
                ),
            },
        );
    });

    for frame in 0..4 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(screen_width, 600.0),
            )),
            ..egui::RawInput::default()
        };
        let mut measured_inspector = None;
        let mut measured_central_right = None;

        let _ = context.run_ui(input, |ui| {
            let maximum_width = inspector_max_width(ui.available_width());
            let response = show_inspector_panel(ui, maximum_width, |ui| {
                ui.allocate_exact_size(egui::vec2(2_400.0, 24.0), egui::Sense::hover());
            });
            measured_inspector = Some(response.response.rect);
            measured_central_right = Some(
                egui::CentralPanel::default()
                    .show_inside(ui, |_ui| {})
                    .response
                    .rect
                    .right(),
            );
        });

        let inspector = measured_inspector.expect("Inspector must be laid out");
        let central_right = measured_central_right.expect("central panel must be laid out");
        assert!(
            (inspector.width() - inspector_width).abs() <= 1.0,
            "frame {frame} widened Inspector to {}",
            inspector.width()
        );
        assert!(
            (central_right - inspector.left()).abs() <= 1.0,
            "frame {frame} moved central boundary to {central_right} instead of {}",
            inspector.left()
        );

        let persisted = context
            .data_mut(|data| {
                data.get_persisted::<egui::PanelState>(egui::Id::new("inspector_panel"))
            })
            .expect("Inspector must persist its size");
        assert!(
            (persisted.rect.width() - inspector_width).abs() <= 1.0,
            "frame {frame} persisted hostile child width {}",
            persisted.rect.width()
        );
    }
}

/// A row of actions in a narrow Inspector must break between buttons.
///
/// The Inspector truncates text so an unbreakable value cannot widen the dock,
/// while `control_row` moves whole buttons to the next row when needed.
#[test]
fn narrow_inspector_action_row_keeps_button_labels_on_one_line() {
    const ACTIONS: [&str; 5] = [
        "Open Prefab",
        "Apply",
        "Revert",
        "Unpack",
        "Place Repeatedly",
    ];

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
    let mut measured_buttons = Vec::new();
    let mut measured_inspector = None;

    let _ = context.run_ui(input, |ui| {
        let maximum_width = inspector_max_width(ui.available_width());
        let inspector_response = show_inspector_panel(ui, maximum_width, |ui| {
            control_row(ui, |ui| {
                for label in ACTIONS {
                    measured_buttons.push((label, ui.button(label).rect));
                }
            });
        });
        measured_inspector = Some(inspector_response.response.rect);
    });

    let inspector = measured_inspector.expect("Inspector must be laid out");
    assert_eq!(measured_buttons.len(), ACTIONS.len());

    let single_line_height = 28.0;
    for (label, rect) in &measured_buttons {
        assert!(
            rect.height() <= single_line_height,
            "{label} grew to {} points tall, so its text wrapped inside the button",
            rect.height()
        );
        assert!(
            rect.right() <= inspector.right() + 1.0,
            "{label} ended at {} outside the Inspector's right edge {}",
            rect.right(),
            inspector.right()
        );
    }

    // Buttons that no longer fit must move down instead of being compressed.
    let rows = measured_buttons
        .iter()
        .map(|(_, rect)| rect.top().round() as i32)
        .collect::<BTreeSet<_>>();
    assert!(
        rows.len() > 1,
        "all {} actions stayed on one row inside a {INSPECTOR_MIN_WIDTH} point dock",
        ACTIONS.len()
    );
}

/// Even a plain horizontal row must not turn its last control into vertical text.
#[test]
fn narrow_inspector_plain_row_truncates_instead_of_wrapping_each_glyph() {
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
    let mut button_rect = None;
    let mut inspector_rect = None;

    let _ = context.run_ui(input, |ui| {
        let maximum_width = inspector_max_width(ui.available_width());
        let response = show_inspector_panel(ui, maximum_width, |ui| {
            ui.horizontal(|ui| {
                let reserved_width = (ui.available_width() - 40.0).max(0.0);
                ui.add_sized(
                    [reserved_width, 20.0],
                    egui::Label::new("Reserved field width").truncate(),
                );
                button_rect = Some(ui.button("Place Repeatedly").rect);
            });
        });
        inspector_rect = Some(response.response.rect);
    });

    let button = button_rect.expect("button must be laid out");
    let inspector = inspector_rect.expect("Inspector must be laid out");
    assert!(
        button.height() <= 28.0,
        "button text wrapped into a {} point vertical column",
        button.height()
    );
    assert!(button.right() <= inspector.right() + 1.0);
}

/// Generated fields switch to a full-width second line in a narrow Inspector.
#[test]
fn narrow_inspector_field_row_places_a_usable_editor_below_its_label() {
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
    let mut row_top = None;
    let mut editor_rect = None;

    let _ = context.run_ui(input, |ui| {
        let maximum_width = inspector_max_width(ui.available_width());
        show_inspector_panel(ui, maximum_width, |ui| {
            row_top = Some(ui.next_widget_position().y);
            super::inspector::inspector_field_row(
                ui,
                "Material Slots",
                "Materials assigned to mesh submeshes",
                |ui| {
                    let width = ui.available_width();
                    editor_rect = Some(
                        ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover())
                            .0,
                    );
                },
            );
        });
    });

    let top = row_top.expect("field row must be laid out");
    let editor = editor_rect.expect("field editor must be laid out");
    assert!(
        editor.top() > top + 20.0,
        "editor stayed beside the label at y={} instead of moving below it",
        editor.top()
    );
    assert!(
        editor.width() >= 100.0,
        "narrow field editor received only {} points",
        editor.width()
    );
}

/// A user-resized Animation Graph dock must paint and claim its complete
/// persisted width while keeping action labels on one line.
#[test]
fn resized_animation_graph_dock_has_no_gap_or_vertical_action_button() {
    let context = egui::Context::default();
    let resized_width = 220.0;
    context.data_mut(|data| {
        data.insert_persisted(
            egui::Id::new("editor_left_dock"),
            egui::PanelState {
                rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(resized_width, 600.0)),
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
    let mut measured_left_width = None;
    let mut measured_central_left = None;
    let mut measured_button_rect = None;
    let mut measured_header_rect = None;

    let _ = context.run_ui(input, |ui| {
        let maximum_width = left_dock_max_width(ui.available_width());
        let left_response = show_primary_left_dock_panel(ui, maximum_width, |ui| {
            let header = ui.scope(|ui| {
                dock_section_header(ui, "Animation Sets", |ui| {
                    measured_button_rect = Some(ui.add(dock_action_button("Create", 64.0)).rect);
                });
            });
            measured_header_rect = Some(header.response.rect);
        });
        measured_left_width = Some(left_response.response.rect.width());

        let central_response = egui::CentralPanel::default().show_inside(ui, |_ui| {});
        measured_central_left = Some(central_response.response.rect.left());
    });

    let left_width = measured_left_width.expect("left dock must be laid out");
    let central_left = measured_central_left.expect("central panel must be laid out");
    let button_rect = measured_button_rect.expect("Create button must be laid out");
    let header_rect = measured_header_rect.expect("section header must be laid out");

    assert!(
        (left_width - resized_width).abs() <= 1.0,
        "persisted width {resized_width} became {left_width}"
    );
    assert!(
        (central_left - left_width).abs() <= 1.0,
        "central panel began at {central_left}, leaving a gap after left dock width {left_width}"
    );
    assert!(button_rect.width() >= 64.0);
    assert!(
        button_rect.height() <= 28.0,
        "Create button became vertically stretched to {} points",
        button_rect.height()
    );

    // The dock wraps text, and a wrapped title inside a right-to-left layout
    // used to spill onto further rows and paint over the action button.
    assert!(
        header_rect.height() <= 32.0,
        "section header grew to {} points, so its title wrapped onto another row",
        header_rect.height()
    );
    assert!(
        header_rect.right() - button_rect.right() <= 8.0,
        "action button stopped at {} instead of the header's right edge {}",
        button_rect.right(),
        header_rect.right()
    );
}

/// Panel limits must grow with the window instead of imposing the previous
/// fixed 420/440/520 point ceilings.
#[test]
fn dock_limits_scale_with_available_editor_space() {
    assert!(bottom_dock_max_height(1200.0) > bottom_dock_max_height(700.0));
    assert!(left_dock_max_width(1600.0) > left_dock_max_width(1100.0));
    assert!(inspector_max_width(1200.0) > inspector_max_width(700.0));

    assert_eq!(bottom_dock_max_height(100.0), BOTTOM_DOCK_MIN_HEIGHT);
    assert_eq!(left_dock_max_width(100.0), LEFT_DOCK_MIN_WIDTH);
    assert_eq!(inspector_max_width(100.0), INSPECTOR_MIN_WIDTH);
}

/// Hierarchy の末尾空白領域が、現在のビューポート内の残り高さだけを
/// 使用し、スクロール量によって増加しないことを確認する。
#[test]
fn hierarchy_empty_space_height_is_bounded_by_available_viewport_height() {
    // コンテンツが短い場合は、残りのビューポートをそのまま使用する。
    assert_eq!(hierarchy_empty_space_height(400.0), 400.0);

    // コンテンツがビューポート末尾まで到達した場合は、
    // 操作可能領域として最低 48px だけを確保する。
    assert_eq!(hierarchy_empty_space_height(0.0), 48.0);

    // 長いコンテンツで available_height が実質的に残っていない場合も、
    // スクロール範囲を増やす大きな空白領域は作らない。
    assert_eq!(hierarchy_empty_space_height(12.0), 48.0);

    // 48px ちょうどの場合は、その値を維持する。
    assert_eq!(hierarchy_empty_space_height(48.0), 48.0);
}

/// Entity行ではないHierarchy背景でも、MeshペイロードをScene Root配置用として取得できる。
///
/// 個別行のテストではなく可視ビューポート全体のフォールバックを直接検証することで、
/// 行間、インデント、一般的な背景でドロップが失われる回帰を検出する。
#[test]
fn hierarchy_viewport_accepts_mesh_drop_outside_entity_rows() {
    // ヘッドレスeguiコンテキストで、Hierarchyの可視領域とポインター位置を再現する。
    let context = egui::Context::default();
    let viewport = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(240.0, 180.0));
    let release_position = viewport.center();

    // FBXなどのモデルソースもAsset Browser上ではMeshペイロードとして渡される。
    let asset_id = AssetId::generate();
    let payload = DragPayload {
        asset_id: asset_id.clone(),
        relative_path: PathBuf::from("meshes/character.fbx"),
        kind: AssetKind::Mesh,
        paths: vec![PathBuf::from("meshes/character.fbx")],
    };

    // ドラッグ開始済みの状態をeguiの共有D&Dストレージへ設定する。
    egui::DragAndDrop::set_payload(&context, payload);

    // 可視Hierarchy背景の中央でボタンを離す入力を作る。
    // 実際のEntity行は配置せず、ビューポートフォールバックだけを通す。
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 300.0),
        )),
        events: vec![
            egui::Event::PointerMoved(release_position),
            egui::Event::PointerButton {
                pos: release_position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ],
        ..egui::RawInput::default()
    };
    let mut dropped = None;

    // 編集可能状態でフォールバック処理を実行し、Scene Root用ペイロードを受け取る。
    let _ = context.run_ui(input, |ui| {
        dropped = hierarchy_viewport_mesh_drop(ui, viewport, true);
    });

    // 取得したペイロードが元のFBXアセットを保持していることを確認する。
    let dropped = dropped.expect("Hierarchy viewport must accept the mesh drop");
    assert_eq!(dropped.asset_id, asset_id);
    assert_eq!(dropped.relative_path, PathBuf::from("meshes/character.fbx"));
}

/// Hierarchy上のEntity型確認が、Asset Browserのペイロードを破棄しないことを確認する。
///
/// Hierarchyは同じドロップ対象でEntity移動とアセット配置の2種類を受け入れる。
/// 先にEntity型を確認しても、続くアセット型の処理へFBX/Meshペイロードが渡ることを保証する。
#[test]
fn hierarchy_type_probe_does_not_discard_asset_payload() {
    // Asset Browserと同じ共有ドラッグ状態を用意し、Entity一行分を表す矩形内で解放する。
    let context = egui::Context::default();
    let target_rect = egui::Rect::from_min_size(egui::pos2(20.0, 20.0), egui::vec2(240.0, 24.0));
    let release_position = target_rect.center();
    let asset_id = AssetId::generate();
    let payload = DragPayload {
        asset_id: asset_id.clone(),
        relative_path: PathBuf::from("meshes/character.fbx"),
        kind: AssetKind::Mesh,
        paths: vec![PathBuf::from("meshes/character.fbx")],
    };
    egui::DragAndDrop::set_payload(&context, payload);

    // 最初の確認では意図的に別のペイロード型を指定し、次の確認でアセット型を取得する。
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 300.0),
        )),
        events: vec![
            egui::Event::PointerMoved(release_position),
            egui::Event::PointerButton {
                pos: release_position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ],
        ..egui::RawInput::default()
    };
    let mut entity_payload_was_taken = false;
    let mut dropped_asset = None;

    let _ = context.run_ui(input, |ui| {
        // 型違いの確認は非破壊である必要があるため、その直後に元のアセットを取得できることを検証する。
        entity_payload_was_taken =
            release_drag_payload_in_rect::<HierarchyDragPayload>(ui, target_rect).is_some();
        dropped_asset = release_drag_payload_in_rect::<DragPayload>(ui, target_rect);
    });

    assert!(!entity_payload_was_taken);
    let dropped_asset = dropped_asset.expect("asset payload must survive the entity type probe");
    assert_eq!(dropped_asset.asset_id, asset_id);
    assert_eq!(
        dropped_asset.relative_path,
        PathBuf::from("meshes/character.fbx")
    );
}

/// トップレベルFBXと、そのFBXから派生したMeshサブアセットを区別できる。
#[test]
fn dropped_model_source_is_distinguished_from_its_mesh_sub_asset() {
    // 本番と同じ決定的ID生成で、ソースと派生Meshを用意する。
    let source_id = AssetId::generate();
    let mesh_id = engine::imported_sub_asset_id(&source_id, engine::ImportedSubAssetKind::Mesh, 0);

    // マニフェストにはモデルソースだけをトップレベル登録し、
    // MeshはImportSettings内の派生サブアセットとして保持する。
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        source_id.clone(),
        engine::ManifestEntry {
            path: "meshes/character.fbx".into(),
            name: Some("Character".into()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![engine::ImportedSubAsset {
                    id: mesh_id.as_str().to_owned(),
                    kind: engine::ImportedSubAssetKind::Mesh,
                    name: "Body".into(),
                    index: 0,
                    target_model_source: None,
                }],
                ..engine::ImportSettings::default()
            },
        },
    );

    // FBXタイル自身はモデル全体のインスタンス化対象になる。
    let source_payload = DragPayload {
        asset_id: source_id,
        relative_path: PathBuf::from("meshes/character.fbx"),
        kind: AssetKind::Mesh,
        paths: vec![PathBuf::from("meshes/character.fbx")],
    };

    // `[mesh]`行は同じrelative_pathを持つが、AssetIdは派生Meshを指している。
    let mesh_payload = DragPayload {
        asset_id: mesh_id,
        relative_path: PathBuf::from("meshes/character.fbx"),
        kind: AssetKind::Mesh,
        paths: Vec::new(),
    };

    assert!(dropped_asset_is_model_source(&manifest, &source_payload));
    assert!(!dropped_asset_is_model_source(&manifest, &mesh_payload));
}

/// ドラッグ表示にはカテゴリーと人が識別できるファイル名が含まれる。
#[test]
fn asset_drag_preview_identifies_the_dragged_asset() {
    let payload = DragPayload {
        asset_id: AssetId::generate(),
        relative_path: PathBuf::from("meshes/Flair.fbx"),
        kind: AssetKind::Mesh,
        paths: vec![PathBuf::from("meshes/Flair.fbx")],
    };

    assert_eq!(
        asset_drag_preview_text(&payload),
        "Dragging [mesh] Flair.fbx"
    );
}

/// The consolidated Hierarchy menu must communicate all three independent
/// states without requiring three permanently visible row controls.
#[test]
fn hierarchy_state_summary_reports_each_non_default_state() {
    assert_eq!(
        hierarchy_entity_state_summary(true, false, false),
        "Enabled · Visible · Editable"
    );
    assert_eq!(
        hierarchy_entity_state_summary(false, true, true),
        "Disabled · Hidden · Locked"
    );
}

/// Position, Rotation, and Scale each keep all three axes on one row at a
/// normal Inspector width and retain usable controls in a narrow dock.
#[test]
fn transform_row_widths_keep_three_axes_compact_and_usable() {
    let (title, axis) = transform_row_widths(420.0, 8.0);
    assert_eq!(title, 76.0);
    assert_eq!(axis, 92.0);
    assert!(title + axis * 3.0 + 8.0 * 3.0 < 420.0);

    let (narrow_title, narrow_axis) = transform_row_widths(160.0, 8.0);
    assert_eq!(narrow_title, 58.0);
    assert_eq!(narrow_axis, 44.0);
}

/// A reference row must stay inside the width it was offered, including the
/// space a trailing widget still needs.
#[test]
fn reference_row_cells_stay_inside_the_reserved_row() {
    let row_rect = egui::Rect::from_min_size(egui::pos2(24.0, 37.0), egui::vec2(260.0, 22.0));
    let (body, selector) = super::inspector::reference_row_rects(row_rect);

    assert_eq!(body.left(), row_rect.left());
    assert_eq!(selector.right(), row_rect.right());
    assert!(body.right() <= selector.left());
    for cell in [body, selector] {
        assert_eq!(cell.top(), row_rect.top());
        assert_eq!(cell.bottom(), row_rect.bottom());
    }

    // A degenerate row must collapse the body rather than produce a negative
    // rectangle or push the selector past the row's right edge.
    let (narrow_body, narrow_selector) = super::inspector::reference_row_rects(
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(8.0, 22.0)),
    );
    assert_eq!(narrow_body.width(), 0.0);
    assert_eq!(narrow_selector.right(), 8.0);

    // Rows that carry a trailing button must hand the field less than the
    // full width, otherwise that button lands outside the row.
    let reserved = super::inspector::remaining_reference_row_width(260.0, 34.0, 8.0);
    assert_eq!(reserved, 218.0);
    assert!(reserved + 34.0 + 8.0 <= 260.0);
}

/// Reproduces the Inspector widening itself on every frame until it reached
/// its maximum width, which happened when a reference row's selector reported
/// a larger desired size than the cell reserved for it.
#[test]
fn reference_row_does_not_widen_the_inspector_panel() {
    let context = egui::Context::default();
    let mut widths = Vec::new();

    for _ in 0..4 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 600.0),
            )),
            ..egui::RawInput::default()
        };
        let _ = context.run_ui(input, |ui| {
            let response = egui::Panel::right("inspector_panel")
                .resizable(true)
                .default_size(INSPECTOR_DEFAULT_WIDTH)
                .min_size(INSPECTOR_MIN_WIDTH)
                .max_size(inspector_max_width(ui.available_width()))
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Mesh");
                            super::inspector::show_compact_reference_field(
                                ui,
                                "▣",
                                egui::Color32::WHITE,
                                "Cube",
                                "Assets/cube.gltf",
                            );
                        });
                    });
                });
            widths.push(response.response.rect.width());
        });
    }

    // Growth was cumulative, so the later frames are the meaningful ones.
    for width in &widths {
        assert_eq!(
            *width, INSPECTOR_DEFAULT_WIDTH,
            "reference row resized the Inspector panel: {widths:?}"
        );
    }
}

/// Draws one scroll area inside `scope` and reports the offset it settled on.
///
/// `rows` controls the content height, which is what decides whether a stored
/// offset survives or is clamped away.
fn scroll_offset_after_frame(
    context: &egui::Context,
    scope: egui::Id,
    rows: usize,
    forced_offset: Option<f32>,
) -> f32 {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 300.0),
        )),
        ..egui::RawInput::default()
    };
    let mut offset = 0.0;
    let _ = context.run_ui(input, |ui| {
        ui.push_id(scope, |ui| {
            let mut area = egui::ScrollArea::vertical();
            if let Some(forced) = forced_offset {
                area = area.vertical_scroll_offset(forced);
            }
            offset = area
                .show(ui, |ui| {
                    for row in 0..rows {
                        ui.label(format!("row {row}"));
                    }
                })
                .state
                .offset
                .y;
        });
    });
    offset
}

/// Reproduces the Inspector returning to the top after a round trip through an
/// Animation Graph tab.
///
/// Both documents resolved to the same scroll area id, so the short Graph
/// Inspector clamped away the offset the long Scene Inspector had stored.
#[test]
fn dock_surfaces_keep_one_scroll_offset_per_document_tab() {
    const SCROLLED: f32 = 120.0;
    const TALL: usize = 200;
    const SHORT: usize = 2;

    let scene_scope = dock_surface_id("entity_inspector", 1);
    let graph_scope = dock_surface_id("entity_inspector", 2);
    let context = egui::Context::default();
    assert_eq!(
        scroll_offset_after_frame(&context, scene_scope, TALL, Some(SCROLLED)),
        SCROLLED
    );
    scroll_offset_after_frame(&context, graph_scope, SHORT, None);
    assert_eq!(
        scroll_offset_after_frame(&context, scene_scope, TALL, None),
        SCROLLED,
        "the Scene Inspector lost its offset while another tab was drawn"
    );

    // One shared scope is the arrangement that used to lose it.
    let shared_scope = egui::Id::new("one_scope_for_every_document");
    let context = egui::Context::default();
    assert_eq!(
        scroll_offset_after_frame(&context, shared_scope, TALL, Some(SCROLLED)),
        SCROLLED
    );
    scroll_offset_after_frame(&context, shared_scope, SHORT, None);
    assert_ne!(
        scroll_offset_after_frame(&context, shared_scope, TALL, None),
        SCROLLED
    );
}

/// The Animation Sets limit must always leave the Motion Slots section the
/// height its own chrome occupies, and must stay usable in a short dock.
#[test]
fn animation_sets_limit_reserves_the_motion_slots_minimum() {
    assert_eq!(
        animation_sets_max_height(600.0),
        600.0 - MOTION_SLOTS_MIN_HEIGHT
    );
    assert_eq!(animation_sets_max_height(100.0), ANIMATION_SETS_MIN_HEIGHT);
}

/// Reproduces the Motion Slots header, hint line, and new-slot strip piling up
/// on each other at the top of the left dock after the Animation Sets panel had
/// been dragged upward.
///
/// The sets panel declared a minimum but no maximum, so a persisted drag height
/// let it claim every point the Motion Slots section needed for its own chrome.
/// That section's strip is placed relative to the bottom edge and does not
/// shrink to fit, so it was drawn over the header instead of below the list.
#[test]
fn animation_sets_panel_cannot_squeeze_out_the_motion_slots_section() {
    let context = egui::Context::default();
    // A drag that leaves the Motion Slots section almost nothing.
    context.data_mut(|data| {
        data.insert_persisted(
            egui::Id::new("animation_graph_sets_panel"),
            egui::PanelState {
                rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 580.0)),
            },
        );
    });
    let mut remaining_height = None;

    for _ in 0..2 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(300.0, 600.0),
            )),
            ..egui::RawInput::default()
        };
        let _ = context.run_ui(input, |ui| {
            let available_height = ui.available_height();
            egui::Panel::bottom("animation_graph_sets_panel")
                .resizable(true)
                .default_size(available_height * 0.5)
                .min_size(ANIMATION_SETS_MIN_HEIGHT)
                .max_size(animation_sets_max_height(available_height))
                .show_inside(ui, |ui| {
                    // The real Animation Sets list claims every point it is
                    // offered, so the panel never shrinks back on its own.
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.label("set");
                        });
                });
            remaining_height = Some(ui.available_height());
        });
    }

    let remaining_height = remaining_height.expect("left dock must be laid out");
    assert!(
        remaining_height >= MOTION_SLOTS_MIN_HEIGHT,
        "Motion Slots section kept only {remaining_height} points, \
         below its {MOTION_SLOTS_MIN_HEIGHT} point minimum"
    );
}

/// Transform axis cells must share one vertical origin even when they are
/// placed independently by the Inspector.
#[test]
fn transform_row_cells_share_identical_vertical_bounds() {
    // A non-zero origin catches implementations that accidentally rebuild
    // cells from zero instead of preserving the allocated Inspector row.
    let row_rect = egui::Rect::from_min_size(egui::pos2(24.0, 37.0), egui::vec2(420.0, 26.0));
    let cells = transform_row_rects(row_rect, 8.0);

    // The title, X, Y, and Z cells must all occupy the complete row height.
    // Any difference here would reintroduce the visible downward staircase.
    for cell in &cells {
        assert_eq!(cell.top(), row_rect.top());
        assert_eq!(cell.bottom(), row_rect.bottom());
        assert_eq!(cell.height(), row_rect.height());
    }

    // Columns must still progress horizontally with the configured spacing.
    assert_eq!(cells[1].left(), cells[0].right() + 8.0);
    assert_eq!(cells[2].left(), cells[1].right() + 8.0);
    assert_eq!(cells[3].left(), cells[2].right() + 8.0);
}

/// Folder rows below a collapsed ancestor are hidden, while the collapsed
/// folder itself remains available for expansion and direct-child queries
/// match the folders shown in the right-hand asset grid.
#[test]
fn asset_folder_helpers_support_collapsible_tree_and_direct_child_grid() {
    let folders = vec![
        crate::AssetFolder {
            relative_path: PathBuf::new(),
            depth: 0,
        },
        crate::AssetFolder {
            relative_path: PathBuf::from("characters"),
            depth: 1,
        },
        crate::AssetFolder {
            relative_path: PathBuf::from("characters/enemies"),
            depth: 2,
        },
        crate::AssetFolder {
            relative_path: PathBuf::from("materials"),
            depth: 1,
        },
    ];
    let dir = tempfile::tempdir().expect("temporary asset root");
    let assets_root = dir.path();
    std::fs::create_dir_all(assets_root.join("characters/enemies")).expect("nested folders");
    std::fs::create_dir_all(assets_root.join("materials")).expect("sibling folder");
    let mut browser = AssetBrowser::new();
    browser.refresh(assets_root);
    browser.toggle_folder_collapsed(Path::new("characters"));

    assert!(browser.folder_row_is_visible(Path::new("characters")));
    assert!(!browser.folder_row_is_visible(Path::new("characters/enemies")));
    assert!(asset_folder_has_children(Path::new("characters"), &folders));
    assert!(is_direct_asset_folder_child(
        Path::new("characters"),
        Path::new("")
    ));
    assert!(!is_direct_asset_folder_child(
        Path::new("characters/enemies"),
        Path::new("")
    ));
}

/// Navigating to a folder must make its tree row reachable.
///
/// Selection used to change the working folder while leaving a collapsed
/// ancestor in place, so revealing an asset from the Inspector, the Problems
/// panel, or a script-creation flow could land on a row nothing displayed.
#[test]
fn navigating_to_a_folder_expands_its_collapsed_ancestors() {
    let dir = tempfile::tempdir().expect("temporary asset root");
    let assets_root = dir.path();
    std::fs::create_dir_all(assets_root.join("characters/enemies")).expect("nested folders");
    std::fs::write(assets_root.join("characters/enemies/boss.obj"), "").expect("nested asset");
    let mut browser = AssetBrowser::new();
    browser.refresh(assets_root);
    browser.toggle_folder_collapsed(Path::new("characters"));
    assert!(!browser.folder_row_is_visible(Path::new("characters/enemies")));

    assert!(browser.select_relative_path(Path::new("characters/enemies/boss.obj")));
    assert_eq!(browser.selected_folder(), Path::new("characters/enemies"));
    assert!(
        browser.folder_row_is_visible(Path::new("characters/enemies")),
        "revealing an asset must expand every collapsed ancestor of its folder"
    );
    assert_eq!(
        browser.take_pending_reveal().as_deref(),
        Some(Path::new("characters/enemies")),
        "the revealed folder must be reported once so its row can be scrolled to"
    );
    assert_eq!(browser.take_pending_reveal(), None);

    browser.toggle_folder_collapsed(Path::new("characters"));
    assert!(browser.set_selected_folder(PathBuf::from("characters/enemies")));
    assert!(browser.folder_row_is_visible(Path::new("characters/enemies")));
}

/// A folder keeps its own collapsed state when it is opened.
///
/// Expanding the target itself would silently discard how the author had
/// arranged a large subtree every time they visited its parent.
#[test]
fn opening_a_folder_preserves_its_own_collapsed_state() {
    let dir = tempfile::tempdir().expect("temporary asset root");
    let assets_root = dir.path();
    std::fs::create_dir_all(assets_root.join("characters/enemies")).expect("nested folders");
    let mut browser = AssetBrowser::new();
    browser.refresh(assets_root);
    browser.toggle_folder_collapsed(Path::new("characters"));

    assert!(browser.set_selected_folder(PathBuf::from("characters")));
    assert!(browser.is_folder_collapsed(Path::new("characters")));
}

/// Long component cards start collapsed, and the author's own choice wins.
///
/// A Skinned Model lists one read-only row per renderer that binds it, so an
/// imported character used to open with a card that buried every other
/// component on the entity.
#[test]
fn long_component_cards_start_collapsed_until_the_author_opens_them() {
    let builtins = engine::builtin_registry();
    let mut preferences = EditorPreferences::default();
    let skinned_model = ComponentTypeId::new(engine::scene_bridge::SKINNED_MODEL_COMPONENT);
    let transform = ComponentTypeId::new("engine.transform");

    assert!(
        !super::inspector::component_card_is_open(&preferences, &skinned_model, &builtins),
        "a component declared long must start collapsed"
    );
    assert!(
        super::inspector::component_card_is_open(&preferences, &transform, &builtins),
        "the most-edited component must stay open"
    );

    preferences
        .component_card_open
        .insert(skinned_model.as_str().to_owned(), true);
    assert!(
        super::inspector::component_card_is_open(&preferences, &skinned_model, &builtins),
        "an explicit choice must outrank the declared default"
    );

    preferences
        .component_card_open
        .insert(transform.as_str().to_owned(), false);
    assert!(!super::inspector::component_card_is_open(
        &preferences,
        &transform,
        &builtins
    ));
}

/// Components with no built-in declaration stay open.
///
/// A missing or project-owned component is the case an author most needs to
/// see, so the unknown path must not inherit a collapsed default.
#[test]
fn components_without_a_builtin_declaration_stay_open() {
    let builtins = engine::builtin_registry();
    let preferences = EditorPreferences::default();
    assert!(super::inspector::component_card_is_open(
        &preferences,
        &ComponentTypeId::new("game.health"),
        &builtins
    ));
}

/// The working path is offered as one navigable step per level.
#[test]
fn folder_breadcrumbs_expose_every_ancestor_level() {
    let breadcrumbs = crate::asset_browser::folder_breadcrumbs(Path::new("characters/enemies"));
    let steps: Vec<(&str, &Path)> = breadcrumbs
        .iter()
        .map(|breadcrumb| (breadcrumb.label.as_str(), breadcrumb.folder.as_path()))
        .collect();
    assert_eq!(
        steps,
        vec![
            ("Assets", Path::new("")),
            ("characters", Path::new("characters")),
            ("enemies", Path::new("characters/enemies")),
        ]
    );

    let root = crate::asset_browser::folder_breadcrumbs(Path::new(""));
    assert_eq!(root.len(), 1, "the asset root is one inert step");
    assert_eq!(root[0].folder, PathBuf::new());
}

#[test]
fn hierarchy_rows_preserve_depth_and_include_ancestors_of_search_hits() {
    let mut scene = AuthoringScene::new();
    let root_id = EntityId::generate();
    let child_id = EntityId::generate();
    let leaf_id = EntityId::generate();
    let mut transaction = Transaction::begin(&scene);
    for (id, name, parent) in [
        (root_id.clone(), "root", None),
        (child_id.clone(), "child", Some(root_id.clone())),
        (leaf_id.clone(), "leaf", Some(child_id.clone())),
    ] {
        transaction.apply(AuthoringCommand::CreateEntity {
            id,
            name: name.into(),
            parent,
        });
    }
    transaction.apply(AuthoringCommand::AddComponent {
        entity: leaf_id.clone(),
        component_type: ComponentTypeId::new("gameplay.health"),
        value: Value::Object(Default::default()),
    });
    transaction.commit(&mut scene).expect("hierarchy fixture");

    let rows = scene_hierarchy_rows(&scene, "gameplay.health", &Default::default());
    assert_eq!(
        rows.iter()
            .map(|row| (row.id.clone(), row.depth))
            .collect::<Vec<_>>(),
        vec![(root_id, 0), (child_id, 1), (leaf_id, 2)]
    );
}

#[test]
fn collapsing_a_row_folds_its_subtree_but_a_search_still_reaches_it() {
    let mut scene = AuthoringScene::new();
    let root_id = EntityId::generate();
    let child_id = EntityId::generate();
    let leaf_id = EntityId::generate();
    let sibling_id = EntityId::generate();
    let mut transaction = Transaction::begin(&scene);
    for (id, name, parent) in [
        (root_id.clone(), "model_root", None),
        (child_id.clone(), "body", Some(root_id.clone())),
        (leaf_id.clone(), "fur", Some(child_id.clone())),
        (sibling_id.clone(), "camera", None),
    ] {
        transaction.apply(AuthoringCommand::CreateEntity {
            id,
            name: name.into(),
            parent,
        });
    }
    transaction.commit(&mut scene).expect("hierarchy fixture");
    let collapsed = std::collections::BTreeSet::from([root_id.clone()]);

    let rows = scene_hierarchy_rows(&scene, "", &collapsed);

    // Root ordering follows entity ID, so compare membership rather than
    // the order two unrelated roots happen to fall in.
    assert_eq!(
        rows.iter()
            .map(|row| row.id.clone())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([root_id.clone(), sibling_id]),
        "a folded model must occupy one row without hiding its siblings"
    );
    let folded = rows
        .iter()
        .find(|row| row.id == root_id)
        .expect("the folded root stays visible");
    assert!(
        folded.has_children,
        "a foldable row must advertise that it has children"
    );

    // Folding must not make a match unreachable through search.
    let found = scene_hierarchy_rows(&scene, "fur", &collapsed);
    assert!(found.iter().any(|row| row.id == leaf_id));
}

#[test]
fn addable_registry_offers_unified_mesh_renderer_components() {
    let registry = engine::builtin_registry();

    for component in [
        engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
        engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT,
    ] {
        let definition = registry
            .get(&ComponentTypeId::new(component))
            .unwrap_or_else(|| panic!("{component} schema must be registered"));
        assert!(
            matches!(definition.schema.default_value(), Value::Object(_)),
            "{component} default must be an object"
        );
    }
    for component in [
        engine::scene_bridge::NAV_MESH_AGENT_COMPONENT,
        engine::scene_bridge::RUNTIME_METADATA_COMPONENT,
    ] {
        let definition = registry
            .get(&ComponentTypeId::new(component))
            .unwrap_or_else(|| panic!("{component} schema must be registered"));
        assert!(matches!(
            definition.schema.default_value(),
            Value::Object(_)
        ));
    }
}

#[test]
fn inspector_uses_display_name_only_for_project_components() {
    let game_id = ComponentTypeId::new("game.c_01example");
    let header = inspector_component_header(&game_id, Some("YokaiComponent"));
    assert_eq!(header, "YokaiComponent");
    assert!(!header.contains(game_id.as_str()));

    let engine_id = ComponentTypeId::new("engine.transform");
    assert_eq!(
        inspector_component_header(&engine_id, None),
        "engine.transform"
    );
}

#[test]
fn inspector_hides_prefab_metadata_but_keeps_unknown_components_visible() {
    let prefab_marker = ComponentTypeId::new(crate::EDITOR_PREFAB_INSTANCE_COMPONENT);
    assert!(!inspector_lists_component(&prefab_marker));

    let unknown = ComponentTypeId::new("future.component");
    assert!(
        inspector_lists_component(&unknown),
        "genuinely unknown components must remain visible for recovery"
    );
}

#[test]
fn orphan_project_component_is_reported_without_a_source_or_compiled_schema() {
    let mut scene = AuthoringScene::new();
    let entity = EntityId::generate();
    let component_type = ComponentTypeId::new("game.c_01kxtq56q3qxhqnqh86mmp758j");
    let mut transaction = Transaction::begin(&scene);
    transaction.apply(AuthoringCommand::CreateEntity {
        id: entity.clone(),
        name: "orphan".to_owned(),
        parent: None,
    });
    transaction.apply(AuthoringCommand::AddComponent {
        entity: entity.clone(),
        component_type: component_type.clone(),
        value: Value::Object(Default::default()),
    });
    transaction.commit(&mut scene).unwrap();

    let diagnostics =
        orphan_game_component_diagnostics(&scene, &ComponentSourceIndex::default(), |_| false);

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].code, "editor.scene.orphan_game_component");
    assert!(matches!(
        diagnostics[0].target.as_ref(),
        Some(engine_authoring::DiagnosticTarget::Component {
            entity: target_entity,
            component_type: target_type,
        }) if target_entity == &entity && target_type == &component_type
    ));
}

#[test]
fn inspector_conditions_and_typed_array_defaults_are_schema_driven() {
    let fields = std::collections::BTreeMap::from([
        ("enabled".to_owned(), Value::Bool(true)),
        ("shape".to_owned(), Value::String("sphere".to_owned())),
    ]);
    assert!(inspector_condition_matches(
        engine::InspectorFieldCondition::Bool {
            field: "enabled",
            equals: true,
        },
        &fields,
    ));
    assert!(inspector_condition_matches(
        engine::InspectorFieldCondition::String {
            field: "shape",
            equals: "sphere",
        },
        &fields,
    ));
    assert_eq!(
        default_value_for_field_type(&engine_authoring::FieldType::String),
        Some(Value::String(String::new()))
    );
    assert!(value_matches_field_type(
        &Value::Array(vec![Value::String("ally".to_owned())]),
        &engine_authoring::FieldType::Array(Box::new(engine_authoring::FieldType::String)),
    ));
}

#[test]
fn scalar_reference_none_row_matches_unassigned_authoring_semantics() {
    let field = |field_type, required, default_value| engine_authoring::FieldSchema {
        name: "reference".to_owned(),
        display_name: "Reference".to_owned(),
        description: "Reference clearability fixture.".to_owned(),
        field_type,
        required,
        default_value,
    };

    assert!(
        super::inspector::field_reference_can_be_unassigned(&field(
            engine_authoring::FieldType::EntityRef,
            false,
            None,
        )),
        "optional entity references must offer the None picker row"
    );
    assert!(
        !super::inspector::field_reference_can_be_unassigned(&field(
            engine_authoring::FieldType::EntityRef,
            true,
            None,
        )),
        "required entity references must not become silently unassigned"
    );
    assert!(
        super::inspector::field_reference_can_be_unassigned(&field(
            engine_authoring::FieldType::AssetRef,
            false,
            None,
        )),
        "optional asset references must offer the None picker row"
    );
    assert!(
        super::inspector::field_reference_can_be_unassigned(&field(
            engine_authoring::FieldType::AssetRef,
            true,
            None,
        )),
        "required asset references without defaults use ADR 0069's inactive state"
    );
    assert!(
        !super::inspector::field_reference_can_be_unassigned(&field(
            engine_authoring::FieldType::AssetRef,
            true,
            Some(Value::AssetRef(AssetId::generate())),
        )),
        "an absent asset reference with a default would select the default, not None"
    );
    assert!(
        !super::inspector::field_reference_can_be_unassigned(&field(
            engine_authoring::FieldType::Array(Box::new(engine_authoring::FieldType::AssetRef,)),
            true,
            Some(Value::Array(Vec::new())),
        )),
        "asset-reference lists keep their existing row-removal interaction"
    );
}

#[test]
fn mesh_asset_choices_include_manifest_obj_entries() {
    let asset_id = engine_authoring::id::AssetId::generate();
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        asset_id.clone(),
        engine::ManifestEntry {
            path: "meshes/ship.obj".into(),
            name: Some("ship_mesh".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );
    manifest.insert(
        engine_authoring::id::AssetId::generate(),
        engine::ManifestEntry {
            path: "textures/ship.png".into(),
            name: Some("ship_texture".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );

    let choices =
        crate::ui::inspector::asset_choices_for_kind(engine::AssetKind::Mesh, &manifest, None);

    assert!(
        choices
            .iter()
            .any(|choice| choice.id == asset_id && choice.label == "ship_mesh"),
        "registered OBJ mesh must be selectable"
    );
    assert!(
        !choices.iter().any(|choice| choice.label == "ship_texture"),
        "non-OBJ manifest entries must not appear in the mesh picker"
    );
}

#[test]
fn skeleton_sub_assets_are_available_to_skeleton_pickers() {
    assert!(super::inspector::imported_sub_asset_matches_picker_kind(
        engine::ImportedSubAssetKind::Skeleton,
        engine::AssetKind::Skeleton,
    ));
}

#[test]
fn mesh_picker_uses_stable_gltf_sub_asset_instead_of_source_id() {
    let source_id = AssetId::generate();
    let mesh_id = AssetId::derive(&source_id, "mesh:2");
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        source_id.clone(),
        engine::ManifestEntry {
            path: "characters/hero.glb".into(),
            name: Some("Hero".into()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![engine::ImportedSubAsset {
                    id: mesh_id.as_str().to_owned(),
                    kind: engine::ImportedSubAssetKind::Mesh,
                    name: "Body".into(),
                    index: 2,
                    target_model_source: None,
                }],
                ..engine::ImportSettings::default()
            },
        },
    );

    let choices = asset_choices_for_kind(engine::AssetKind::Mesh, &manifest, None);

    assert!(choices
        .iter()
        .any(|choice| choice.id == mesh_id && choice.label == "Hero / Body"));
    assert!(!choices.iter().any(|choice| choice.id == source_id));
}

#[test]
fn clip_picker_offers_animation_sub_assets_instead_of_model_sources() {
    let imported_id = AssetId::generate();
    let clip_id = AssetId::derive(&imported_id, "animation:1");
    let unimported_id = AssetId::generate();
    let motion_id = AssetId::generate();
    let motion_clip_id = AssetId::derive(&motion_id, "animation:0");
    let unimported_motion_id = AssetId::generate();
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        imported_id.clone(),
        engine::ManifestEntry {
            path: "meshes/flair.fbx".into(),
            name: Some("flair".into()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![engine::ImportedSubAsset {
                    id: clip_id.as_str().to_owned(),
                    kind: engine::ImportedSubAssetKind::Animation,
                    name: "mixamo.com".into(),
                    index: 1,
                    target_model_source: None,
                }],
                ..engine::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        unimported_id.clone(),
        engine::ManifestEntry {
            path: "meshes/prop.fbx".into(),
            name: Some("prop".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );
    manifest.insert(
        motion_id.clone(),
        engine::ManifestEntry {
            path: "motions/dance.vmd".into(),
            name: Some("dance".into()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![engine::ImportedSubAsset {
                    id: motion_clip_id.as_str().to_owned(),
                    kind: engine::ImportedSubAssetKind::Animation,
                    name: "dance".into(),
                    index: 0,
                    target_model_source: None,
                }],
                ..engine::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        unimported_motion_id.clone(),
        engine::ManifestEntry {
            path: "motions/face.vmd".into(),
            name: Some("face".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );

    let choices = asset_choices_for_kind(engine::AssetKind::AnimationClip, &manifest, None);

    assert!(choices
        .iter()
        .any(|choice| choice.id == clip_id && choice.label == "flair / mixamo.com"));
    assert!(choices
        .iter()
        .any(|choice| choice.id == motion_clip_id && choice.label == "dance / dance"));
    assert!(
        !choices.iter().any(|choice| choice.id == imported_id),
        "an Animation Set binding must not be able to name the model source file"
    );
    assert!(
        !choices.iter().any(|choice| choice.id == unimported_id),
        "a model source without imported clips offers no animation either"
    );
    assert!(
        !choices.iter().any(|choice| choice.id == motion_id),
        "an Animation Set binding must not be able to name the VMD source file"
    );
    assert!(
        !choices
            .iter()
            .any(|choice| choice.id == unimported_motion_id),
        "an unimported VMD source offers no animation either"
    );
}

#[test]
fn clip_picker_distinguishes_one_vmd_baked_for_multiple_models() {
    let motion = AssetId::generate();
    let hero = AssetId::generate();
    let villain = AssetId::generate();
    let hero_clip = engine::imported_motion_sub_asset_id(&motion, &hero, 0);
    let villain_clip = engine::imported_motion_sub_asset_id(&motion, &villain, 0);
    let legacy_alias = engine::imported_sub_asset_id(
        &motion,
        engine::ImportedSubAssetKind::Animation,
        0,
    );
    let mut manifest = engine::AssetManifest::default();
    for (id, path, name) in [
        (hero.clone(), "models/hero.pmx", "Hero"),
        (villain.clone(), "models/villain.pmx", "Villain"),
    ] {
        manifest.insert(
            id,
            engine::ManifestEntry {
                path: path.into(),
                name: Some(name.into()),
                import_settings: engine::ImportSettings::default(),
            },
        );
    }
    manifest.insert(
        motion.clone(),
        engine::ManifestEntry {
            path: "motions/dance.vmd".into(),
            name: Some("dance".into()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![
                    engine::ImportedSubAsset {
                        id: hero_clip.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Animation,
                        name: "dance".into(),
                        index: 0,
                        target_model_source: Some(hero.as_str().to_owned()),
                    },
                    engine::ImportedSubAsset {
                        id: villain_clip.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Animation,
                        name: "dance".into(),
                        index: 0,
                        target_model_source: Some(villain.as_str().to_owned()),
                    },
                    engine::ImportedSubAsset {
                        id: legacy_alias.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Animation,
                        name: "dance".into(),
                        index: 0,
                        target_model_source: Some(hero.as_str().to_owned()),
                    },
                ],
                ..engine::ImportSettings::default()
            },
        },
    );

    let choices = asset_choices_for_kind(engine::AssetKind::AnimationClip, &manifest, None);

    assert!(choices
        .iter()
        .any(|choice| choice.id == hero_clip && choice.label == "dance / dance — Hero"));
    assert!(choices.iter().any(|choice| {
        choice.id == villain_clip && choice.label == "dance / dance — Villain"
    }));
    assert!(
        !choices.iter().any(|choice| choice.id == legacy_alias),
        "the compatibility alias must remain resolvable without appearing as a duplicate choice"
    );
}

/// Animation Setの最終防御が、親VMDと生成済みClipを正しく区別することを確認する。
///
/// ピッカー候補のテストだけでは、ドラッグ＆ドロップや古いJSONから親ソースIDが
/// 渡された場合を保証できないため、保存前検証を直接テストする。
#[test]
fn animation_set_validation_accepts_only_imported_animation_clip_sub_assets() {
    // 1つのVMD親ソースと、そこから生成された2つのAnimation Clipを用意する。
    // 親とサブアセットは別の安定IDでなければならない。
    let motion_source = AssetId::generate();
    let primary_clip = AssetId::derive(&motion_source, "animation:0");
    let overlay_clip = AssetId::derive(&motion_source, "animation:1");

    // 実際のインポート後マニフェストと同じく、親ソースのImportSettings内へ
    // サブアセット一覧を保持する。
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        motion_source.clone(),
        engine::ManifestEntry {
            path: "motions/dance.vmd".into(),
            name: Some("dance".into()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![
                    engine::ImportedSubAsset {
                        id: primary_clip.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Animation,
                        name: "body".into(),
                        index: 0,
                        target_model_source: None,
                    },
                    engine::ImportedSubAsset {
                        id: overlay_clip.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Animation,
                        name: "face".into(),
                        index: 1,
                        target_model_source: None,
                    },
                ],
                ..engine::ImportSettings::default()
            },
        },
    );

    // primaryとoverlayの両方がAnimation Clipサブアセットなら、文書全体の
    // 保存前検証に成功する。
    let slot = engine_authoring::MotionSlotId::generate();
    let mut document = engine_authoring::AnimationSet::new(AssetId::generate());
    document.bindings.insert(
        slot.clone(),
        engine_authoring::AnimationBinding {
            name: "Dance".into(),
            clip: engine_authoring::MotionSourceRef::native(primary_clip.clone()),
            overlays: vec![engine_authoring::MotionSourceRef::native(
                overlay_clip,
            )],
            events: Vec::new(),
        },
    );
    assert!(
        super::assets::validate_animation_set_clip_references(&document, &manifest).is_ok()
    );

    // primaryを親VMDへ差し替えた場合は、保存可能なClipではないため拒否する。
    document
        .bindings
        .get_mut(&slot)
        .expect("the test binding must exist")
        .clip = motion_source.clone();
    let primary_error =
        super::assets::validate_animation_set_clip_references(&document, &manifest)
            .expect_err("a parent VMD source must not be accepted as a primary clip");
    assert!(primary_error.contains("primary clip"));
    assert!(primary_error.contains("source asset"));

    // primaryを戻し、overlayだけを親VMDへした場合も同じ契約で拒否する。
    let binding = document
        .bindings
        .get_mut(&slot)
        .expect("the test binding must exist");
    binding.clip = primary_clip;
    binding.overlays = vec![motion_source];

    let overlay_error =
        super::assets::validate_animation_set_clip_references(&document, &manifest)
            .expect_err("a parent VMD source must not be accepted as an overlay");
    assert!(overlay_error.contains("overlay 1"));
    assert!(overlay_error.contains("source asset"));
}

#[test]
fn asset_reference_navigation_reveals_direct_and_imported_source_rows() {
    let source_id = AssetId::generate();
    let mesh_id = AssetId::derive(&source_id, "mesh:0");
    let texture_id = AssetId::generate();
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        source_id.clone(),
        engine::ManifestEntry {
            path: "characters/hero.fbx".into(),
            name: Some("Hero".into()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![engine::ImportedSubAsset {
                    id: mesh_id.as_str().to_owned(),
                    kind: engine::ImportedSubAssetKind::Mesh,
                    name: "Body".into(),
                    index: 0,
                    target_model_source: None,
                }],
                ..engine::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        texture_id.clone(),
        engine::ManifestEntry {
            path: "textures/hero.png".into(),
            name: Some("Hero Texture".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );

    assert_eq!(
        super::inspector::asset_reference_source_path(&manifest, &texture_id),
        Some(PathBuf::from("textures/hero.png")),
        "top-level references must reveal their own Asset Browser row"
    );
    assert_eq!(
        super::inspector::asset_reference_source_path(&manifest, &mesh_id),
        Some(PathBuf::from("characters/hero.fbx")),
        "imported sub-assets must reveal their owning model source"
    );
    assert_eq!(
        super::inspector::asset_reference_source_path(
            &manifest,
            &builtin_asset_id(engine::scene_bridge::BUILTIN_TRIANGLE_ASSET_ID),
        ),
        None,
        "built-in assets intentionally have no project source row"
    );
}

#[test]
fn inspector_asset_reference_icons_use_asset_browser_visual_families() {
    use crate::asset_browser::AssetKind as BrowserKind;

    let mappings = [
        (engine::AssetKind::Mesh, BrowserKind::Mesh),
        (engine::AssetKind::GltfSource, BrowserKind::Mesh),
        (engine::AssetKind::Skin, BrowserKind::Mesh),
        (engine::AssetKind::Skeleton, BrowserKind::Mesh),
        (engine::AssetKind::Material, BrowserKind::Material),
        (engine::AssetKind::Texture, BrowserKind::Texture),
        (engine::AssetKind::AnimationClip, BrowserKind::AnimationClip),
        (engine::AssetKind::AnimationGraph, BrowserKind::Graph),
        (engine::AssetKind::BehaviorTree, BrowserKind::Graph),
        (engine::AssetKind::AnimationSet, BrowserKind::AnimationSet),
        (engine::AssetKind::Audio, BrowserKind::Audio),
        (engine::AssetKind::NavMesh, BrowserKind::NavMesh),
        (engine::AssetKind::UiDocument, BrowserKind::UiDocument),
        (engine::AssetKind::Prefab, BrowserKind::Prefab),
    ];

    for (reference_kind, browser_kind) in mappings {
        assert_eq!(
            super::inspector::asset_reference_browser_kind(reference_kind),
            browser_kind,
            "Inspector object fields must reuse the Asset Browser icon family"
        );
    }
}

#[test]
fn entity_reference_hierarchy_path_uses_readable_labels_without_changing_identity() {
    let root_id = EntityId::generate();
    let child_id = EntityId::generate();
    let scene_json = format!(
        r#"{{
            "schema_version": 1,
            "entities": [
                {{
                    "id": "{root_id}",
                    "name": "root",
                    "display_name": "World",
                    "description": "",
                    "enabled": true,
                    "components": {{}}
                }},
                {{
                    "id": "{child_id}",
                    "name": "player",
                    "display_name": "Player",
                    "description": "",
                    "parent": "{root_id}",
                    "enabled": true,
                    "components": {{}}
                }}
            ]
        }}"#,
        root_id = root_id.as_str(),
        child_id = child_id.as_str(),
    );
    let scene =
        engine_authoring::load_scene_from_json(&scene_json).expect("scene fixture must load");

    assert_eq!(
        super::inspector::entity_reference_hierarchy_path(&scene, &child_id),
        "World / Player"
    );
}

#[test]
fn picker_drops_entries_whose_file_was_deleted_outside_the_editor() {
    let assets_root = tempfile::tempdir().expect("temporary assets root");
    std::fs::create_dir_all(assets_root.path().join("meshes")).expect("mesh folder");
    std::fs::write(assets_root.path().join("meshes/kept.obj"), b"v 0 0 0\n").expect("kept mesh");
    let kept = AssetId::generate();
    let deleted = AssetId::generate();
    let mut manifest = engine::AssetManifest::default();
    for (id, path, name) in [
        (&kept, "meshes/kept.obj", "kept"),
        (&deleted, "meshes/deleted.obj", "deleted"),
    ] {
        manifest.insert(
            id.clone(),
            engine::ManifestEntry {
                path: path.into(),
                name: Some(name.into()),
                import_settings: engine::ImportSettings::default(),
            },
        );
    }

    let choices =
        asset_choices_for_kind(engine::AssetKind::Mesh, &manifest, Some(assets_root.path()));

    assert!(choices.iter().any(|choice| choice.id == kept));
    assert!(
        !choices.iter().any(|choice| choice.id == deleted),
        "an entry with no file must not be offered as a new reference"
    );
}

#[test]
fn picker_drops_sub_assets_whose_source_file_was_deleted() {
    let assets_root = tempfile::tempdir().expect("temporary assets root");
    let source_id = AssetId::generate();
    let mesh_id = AssetId::derive(&source_id, "mesh:0");
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        source_id,
        engine::ManifestEntry {
            path: "characters/hero.glb".into(),
            name: Some("Hero".into()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![engine::ImportedSubAsset {
                    id: mesh_id.as_str().to_owned(),
                    kind: engine::ImportedSubAssetKind::Mesh,
                    name: "Body".into(),
                    index: 0,
                    target_model_source: None,
                }],
                ..engine::ImportSettings::default()
            },
        },
    );

    let choices =
        asset_choices_for_kind(engine::AssetKind::Mesh, &manifest, Some(assets_root.path()));

    assert!(
        !choices.iter().any(|choice| choice.id == mesh_id),
        "sub-assets of a deleted source must not be offered either"
    );
}

#[test]
fn ui_document_asset_choices_offer_builtin_and_manifest_ui_json_entries() {
    let asset_id = engine_authoring::id::AssetId::generate();
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        asset_id.clone(),
        engine::ManifestEntry {
            path: "ui/hud.ui.json".into(),
            name: Some("hud".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );
    manifest.insert(
        engine_authoring::id::AssetId::generate(),
        engine::ManifestEntry {
            path: "meshes/ship.obj".into(),
            name: Some("ship_mesh".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );

    let choices = asset_choices_from_manifest(
        &ComponentTypeId::new(engine::scene_bridge::UI_DOCUMENT_COMPONENT),
        &manifest,
        None,
    );

    assert!(
        choices.iter().any(|choice| choice.label == "Built-in UI"
            && choice.id == builtin_asset_id(engine::scene_bridge::BUILTIN_UI_DOCUMENT_ASSET_ID)),
        "picker must offer the built-in UI document"
    );
    assert!(
        choices
            .iter()
            .any(|choice| choice.id == asset_id && choice.label == "hud"),
        "registered .ui.json document must be selectable"
    );
    assert!(
        !choices.iter().any(|choice| choice.label == "ship_mesh"),
        "non-.ui.json manifest entries must not appear in the UI document picker"
    );
}

#[test]
fn is_registerable_ui_document_matches_only_ui_json_suffix() {
    assert!(is_registerable_ui_document(Path::new("hud.ui.json")));
    assert!(is_registerable_ui_document(Path::new("UI/HUD.UI.JSON")));
    assert!(!is_registerable_ui_document(Path::new("hud.json")));
    assert!(!is_registerable_ui_document(Path::new("hud.scene.json")));
}

#[test]
fn asset_registration_recognizes_editor_ready_runtime_formats() {
    for path in [
        "character.glb",
        "albedo.png",
        "emissive.webp",
        "footstep.ogg",
        "enemy.bt.graph.json",
        "arena.navmesh.json",
        "enemy.prefab.json",
        "surface.material.json",
    ] {
        assert!(
            is_registerable_asset(Path::new(path)),
            "{path} should be registerable"
        );
    }
    assert!(!is_registerable_asset(Path::new("notes.txt")));
}

/// Right-clicks the Asset Browser at `position` in a headless frame and
/// reports whether a context menu opened.
///
/// The pointer is moved, pressed, and released across three passes because
/// egui resolves a click against the widget rects recorded by the previous
/// pass.
fn asset_browser_menu_opens_at(
    root: &ProjectRoot,
    manifest: &engine::AssetManifest,
    position: egui::Pos2,
) -> bool {
    let context = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
    let mut browser = AssetBrowser::new();
    browser.refresh(root.assets_root().as_path());
    let mut search = String::new();
    let mut thumbnails = std::collections::BTreeMap::new();
    let mut scroll_reset = false;
    let mut tab = ProjectBrowserTab::Assets;

    let secondary = |pressed: bool| egui::Event::PointerButton {
        pos: position,
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    for events in [
        vec![egui::Event::PointerMoved(position)],
        vec![secondary(true)],
        vec![secondary(false)],
    ] {
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..egui::RawInput::default()
        };
        let _ = context.run_ui(input, |ui| {
            show_project_browser(
                ui,
                &mut browser,
                &mut search,
                &mut thumbnails,
                &mut scroll_reset,
                &mut tab,
                Some(root),
                manifest,
                false,
            );
        });
    }
    egui::Popup::is_any_open(&context)
}

/// Every spot the browser leaves empty must offer the same menu.
///
/// The create menus used to sit on fixed-height strips placed right after the
/// last row, so a right-click below them or beside a short row silently did
/// nothing while an identical-looking spot one row up worked.
#[test]
fn right_clicking_any_empty_spot_in_the_asset_browser_opens_a_menu() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "AssetBrowserEmptySpaceTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    std::fs::write(root.assets_root().join("only.obj"), b"v 0 0 0\n")
        .expect("one asset file must be written");
    let manifest = engine::AssetManifest::default();

    // Well below and to the right of the single tile, in the content pane.
    assert!(
        asset_browser_menu_opens_at(&root, &manifest, egui::pos2(700.0, 520.0)),
        "empty space in the folder contents must offer the create menu"
    );

    // The folder tree carried the same trailing-strip limitation.
    assert!(
        asset_browser_menu_opens_at(&root, &manifest, egui::pos2(100.0, 520.0)),
        "empty space in the folder tree must offer the create menu"
    );
}

/// Clicking a step of the working path opens that folder.
///
/// The path used to be inert text, so leaving a nested folder meant finding
/// its parent row in the tree.
#[test]
fn clicking_a_working_path_step_opens_that_folder() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "AssetBrowserBreadcrumbTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    std::fs::create_dir_all(root.assets_root().join("characters/enemies"))
        .expect("nested folders must be created");
    let manifest = engine::AssetManifest::default();

    let mut browser = AssetBrowser::new();
    browser.refresh(root.assets_root().as_path());
    assert!(browser.set_selected_folder(PathBuf::from("characters/enemies")));
    let mut search = String::new();
    let mut thumbnails = std::collections::BTreeMap::new();
    let mut scroll_reset = false;
    let mut tab = ProjectBrowserTab::Assets;

    let context = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
    let mut step_position = None;
    // The first pass lays the path row out and reports where its steps were
    // painted; the press and release are delivered in the passes after that.
    for pass in 0..3 {
        let events = match step_position {
            Some(position) if pass > 0 => vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: pass == 1,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            _ => Vec::new(),
        };
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events,
            ..egui::RawInput::default()
        };
        let output = context.run_ui(input, |ui| {
            show_project_browser(
                ui,
                &mut browser,
                &mut search,
                &mut thumbnails,
                &mut scroll_reset,
                &mut tab,
                Some(&root),
                &manifest,
                false,
            );
        });
        if pass == 0 {
            let painted = painted_text_centers(&output, "characters");
            assert_eq!(
                painted.len(),
                2,
                "`characters` must be painted once as a tree row and once as a path step"
            );
            // The tree occupies the left column and the path row sits above
            // the content grid, so the rightmost of the two is the path step
            // regardless of the browser's exact panel arithmetic.
            step_position = painted
                .into_iter()
                .max_by(|left, right| left.x.total_cmp(&right.x));
        }
    }

    assert_eq!(
        browser.selected_folder(),
        Path::new("characters"),
        "clicking the `characters` step of the path must open that folder"
    );
}

/// Returns the center of every painted text run equal to `text`.
///
/// Locating widgets by what was actually drawn keeps the test independent of
/// the browser's exact panel arithmetic.
fn painted_text_centers(output: &egui::FullOutput, text: &str) -> Vec<egui::Pos2> {
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(shape) if shape.galley.job.text == text => {
                Some(egui::Rect::from_min_size(shape.pos, shape.galley.size()).center())
            }
            _ => None,
        })
        .collect()
}

#[test]
fn graph_asset_picker_filters_using_persisted_graph_kind() {
    let directory = tempfile::tempdir().expect("temporary asset root must be created");
    std::fs::write(
        directory.path().join("locomotion.graph.json"),
        r#"{"kind":"anim.graph"}"#,
    )
    .expect("animation graph fixture must be written");
    std::fs::write(
        directory.path().join("enemy.graph.json"),
        r#"{"kind":"behavior_tree.graph"}"#,
    )
    .expect("behavior graph fixture must be written");
    let animation_id = AssetId::generate();
    let behavior_id = AssetId::generate();
    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        animation_id.clone(),
        engine::ManifestEntry {
            path: "locomotion.graph.json".into(),
            name: Some("locomotion".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );
    manifest.insert(
        behavior_id,
        engine::ManifestEntry {
            path: "enemy.graph.json".into(),
            name: Some("enemy".into()),
            import_settings: engine::ImportSettings::default(),
        },
    );

    let choices = asset_choices_for_kind(
        engine::AssetKind::AnimationGraph,
        &manifest,
        Some(directory.path()),
    );

    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].id, animation_id);
}

#[test]
fn material_editor_saves_typed_changes_through_project_path_boundary() {
    let directory = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "MaterialSaveTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project fixture");
    let material_path = root.assets_root().join("surface.material.json");
    std::fs::write(
        &material_path,
        engine_authoring::MaterialAsset::default()
            .to_json()
            .expect("default material JSON"),
    )
    .expect("material fixture");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);
    let material = engine_authoring::MaterialAsset {
        metallic: 0.75,
        alpha_mode: engine_authoring::MaterialAlphaMode::Mask,
        alpha_cutoff: 0.25,
        ..engine_authoring::MaterialAsset::default()
    };
    app.material_editor
        .open_material(PathBuf::from("surface.material.json"), material);

    app.save_active_material();

    let saved = engine_authoring::MaterialAsset::from_json(
        &std::fs::read_to_string(material_path).expect("saved material"),
    )
    .expect("saved material remains valid");
    assert_eq!(saved.metallic, 0.75);
    assert_eq!(saved.alpha_mode, engine_authoring::MaterialAlphaMode::Mask);
    assert_eq!(saved.alpha_cutoff, 0.25);
}

#[test]
fn continuous_material_edits_are_coalesced_before_disk_write() {
    let directory = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "MaterialDebounceTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project fixture");
    let material_path = root.assets_root().join("surface.material.json");
    let original = engine_authoring::MaterialAsset::default();
    std::fs::write(
        &material_path,
        original.to_json().expect("default material JSON"),
    )
    .expect("material fixture");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);
    let changed = engine_authoring::MaterialAsset {
        metallic: 0.81,
        ..original.clone()
    };
    app.material_editor
        .open_material(PathBuf::from("surface.material.json"), changed.clone());
    let context = egui::Context::default();

    app.queue_active_material_save(&context);
    let before_quiet_period = engine_authoring::MaterialAsset::from_json(
        &std::fs::read_to_string(&material_path).expect("queued material file"),
    )
    .expect("queued material remains valid");
    assert_eq!(before_quiet_period, original);

    for pending in app.pending_material_saves.values_mut() {
        pending.deadline = std::time::Instant::now();
    }
    app.flush_pending_material_saves(&context);

    let saved = engine_authoring::MaterialAsset::from_json(
        &std::fs::read_to_string(material_path).expect("flushed material file"),
    )
    .expect("flushed material remains valid");
    assert_eq!(saved, changed);
    assert!(app.pending_material_saves.is_empty());
    assert!(app.material_scene_preview_deadline.is_some());
}

#[test]
fn open_project_opens_configured_start_scene() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "ConfiguredSceneTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let scene_path = root.assets_root().join("scenes/configured.scene.json");
    let scene_json = engine_authoring::AuthoringScene::new()
        .to_canonical_json()
        .expect("empty scene must serialize");
    std::fs::write(&scene_path, scene_json).expect("scene fixture must be written");
    let settings = ProjectSettings {
        start_scene: Some("scenes/configured.scene.json".into()),
        ..ProjectSettings::default()
    };
    settings
        .save(root.path())
        .expect("project settings fixture must be written");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.do_open_project(root.path().to_path_buf());

    assert!(app.session.scene().is_some());
    assert_eq!(
        app.session.current_document_path(),
        Some(scene_path.as_path())
    );
}

#[test]
fn open_project_without_start_scene_opens_first_scene_asset() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "FallbackSceneTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let scene_path = root.assets_root().join("scenes/main.scene.json");
    let scene_json = engine_authoring::AuthoringScene::new()
        .to_canonical_json()
        .expect("empty scene must serialize");
    std::fs::write(&scene_path, scene_json).expect("scene fixture must be written");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.do_open_project(root.path().to_path_buf());

    assert!(app.session.scene().is_some());
    assert_eq!(
        app.session.current_document_path(),
        Some(scene_path.as_path())
    );
}

#[test]
fn new_project_creates_and_opens_starter_scene() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());

    app.new_project(dir.path().to_path_buf());

    let root = app
        .project_root
        .as_ref()
        .expect("new project must become the active project");
    let scene_path = root.assets_root().join("scenes/main.scene.json");
    assert_eq!(
        app.session.current_document_path(),
        Some(scene_path.as_path())
    );
    let scene = app
        .session
        .scene()
        .expect("starter Scene must open immediately");
    assert_eq!(scene.entity_count(), 4);
    let square = scene
        .entities()
        .find(|(_, entity)| entity.name == "square")
        .map(|(_, entity)| entity)
        .expect("starter Scene must contain the visible square");
    assert!(square.components.contains_key(&ComponentTypeId::new(
        engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT
    )));
    let settings = ProjectSettings::load(root.path()).expect("project settings must load");
    assert_eq!(
        settings.start_scene.as_deref(),
        Some("scenes/main.scene.json")
    );
}

#[test]
fn set_project_root_loads_asset_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "ManifestTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let asset_id = engine_authoring::id::AssetId::generate();
    let manifest_json = format!(
        r#"{{
  "schema_version": 2,
  "assets": {{
"{}": {{ "path": "meshes/cube.obj", "name": "cube" }}
  }}
}}"#,
        asset_id.as_str()
    );
    // Test fixture: plain write is fine here; production manifest saves use replace_file_contents.
    std::fs::write(root.path().join("asset_manifest.json"), manifest_json)
        .expect("manifest write must succeed");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);

    assert_eq!(app.asset_manifest.len(), 1);
    let entry = app
        .asset_manifest
        .get(&asset_id)
        .expect("manifest entry must be loaded");
    assert_eq!(entry.path, "meshes/cube.obj");
    assert_eq!(entry.name.as_deref(), Some("cube"));
}

#[test]
fn set_project_root_discovers_game_component_in_unicode_project_path() {
    let dir = tempfile::tempdir().unwrap();
    let project_path = dir.path().join("\u{30d0}\u{30b9}\u{30bf}\u{30fc}\u{30ba}!");
    std::fs::create_dir(&project_path).expect("Unicode project directory must be created");
    let root = ProjectRoot::create(
        &project_path,
        engine_authoring::ProjectConfig {
            name: "UnicodeGameCodeTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    engine_authoring::initialize_game_project(&root).expect("Rust game project must initialize");
    engine_authoring::create_rust_script(
        &root,
        RustScriptKind::Component,
        "YokaiComponent",
        RustScriptSchedule::Update,
    )
    .expect("component source must be created");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);

    let component = app
        .asset_browser
        .entries()
        .iter()
        .find(|entry| entry.kind == AssetKind::RustComponent)
        .expect("game component must appear in the project browser");
    assert_eq!(component.display_name, "yokai_component");
    assert_eq!(
        component.relative_path,
        PathBuf::from("scripts/rust/components").join("yokai_component.rs")
    );
}

#[test]
fn set_project_root_treats_missing_asset_manifest_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "MissingManifestTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);

    assert!(app.asset_manifest.is_empty());
    assert!(
        !app.session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.asset_manifest_load_failed"),
        "missing manifest should not be reported as an error"
    );
}

#[test]
fn create_animation_graph_registers_and_opens_a_gui_editable_asset() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "AnimationGraphCreateTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let assets_root = root.assets_root();
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);

    app.create_animation_graph_document();

    assert!(app.session.is_animation_graph());
    assert_eq!(app.session.graph().kind.as_str(), "anim.graph");
    assert_eq!(
        app.session
            .graph()
            .nodes
            .values()
            .filter(|node| node.node_type.as_str() == "anim.entry")
            .count(),
        1
    );
    let (_, entry) = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".anim.graph.json"))
        .expect("new graph must be registered in the asset manifest");
    assert!(assets_root.join(&entry.path).is_file());
    assert!(
        crate::document::derive_view_path(&assets_root.join(&entry.path))
            .is_some_and(|path| path.is_file())
    );
}

#[test]
fn create_animation_graph_in_selected_asset_folder_registers_and_opens_it() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "AnimationGraphFolderCreateTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let destination = root.assets_root().join("characters");
    std::fs::create_dir_all(&destination).expect("asset folder must exist");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);

    app.create_animation_graph_document_in_folder(std::path::Path::new("characters"));

    let (_, entry) = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| {
            entry.path.starts_with("characters/") && entry.path.ends_with(".anim.graph.json")
        })
        .expect("folder-created graph must be registered");
    assert!(app
        .project_root
        .as_ref()
        .expect("project root")
        .assets_root()
        .join(&entry.path)
        .is_file());
    assert!(app.session.is_animation_graph());
}

#[test]
fn create_animation_graph_for_controller_assigns_before_opening_graph_tab() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "ControllerGraphCreateTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let scene_path = root.scenes_dir().join("main.scene.json");
    std::fs::write(&scene_path, r#"{"schema_version":1,"entities":[]}"#)
        .expect("scene fixture must be written");

    let mut session = crate::session::EditorSession::empty_behavior_tree();
    session
        .open_scene(scene_path)
        .expect("scene fixture must open");
    let entity = session
        .create_scene_entity("animated")
        .expect("entity creation must succeed");
    let component_type = ComponentTypeId::new(engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT);
    let controller = engine::builtin_registry()
        .get(&component_type)
        .expect("Animation Controller schema must exist")
        .schema
        .default_value();
    session
        .add_scene_component(entity.clone(), component_type.clone(), controller)
        .expect("Animation Controller must be added");

    let mut app = EditorApp::new(session);
    app.set_project_root(root);
    let scene_tab = app
        .session
        .summaries()
        .into_iter()
        .find(|tab| tab.kind == WorkspaceDocumentKind::Scene)
        .expect("scene tab must exist")
        .id;

    app.create_animation_graph_for_controller(entity.clone());

    assert!(app.session.is_animation_graph());
    let graph_id = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".anim.graph.json"))
        .map(|(id, _)| id.clone())
        .expect("created Graph must be registered");

    assert!(app.session.activate(scene_tab));
    let assigned = app
        .session
        .scene_entity(&entity)
        .and_then(|entity| entity.components.get(&component_type))
        .and_then(|value| match value {
            Value::Object(fields) => fields.get("graph"),
            _ => None,
        });
    assert_eq!(assigned, Some(&Value::AssetRef(graph_id)));
}

#[test]
fn create_animation_set_for_graph_registers_typed_document() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "AnimationSetCreateTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let assets_root = root.assets_root();
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);
    app.create_animation_graph_document_in_folder(std::path::Path::new("characters"));
    let graph_id = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".anim.graph.json"))
        .map(|(id, _)| id.clone())
        .expect("new graph must be registered");

    app.create_animation_set_document_in_folder(
        Some(graph_id.clone()),
        std::path::Path::new("characters"),
    );

    let (_, entry) = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".animset.json"))
        .expect("new Animation Set must be registered");
    let json = std::fs::read_to_string(assets_root.join(&entry.path))
        .expect("Animation Set document must exist");
    let set = engine_authoring::AnimationSet::from_json(&json)
        .expect("created Animation Set must be valid");
    assert_eq!(set.graph, Some(graph_id));
    assert!(set.bindings.is_empty());
}

#[test]
fn animation_graph_reverse_lookup_lists_only_sets_targeting_the_open_graph() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "AnimationSetReverseLookupTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);
    app.create_animation_graph_document_in_folder(std::path::Path::new("characters"));
    let graph_id = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".anim.graph.json"))
        .map(|(id, _)| id.clone())
        .expect("new graph must be registered");

    app.create_animation_set_document_in_folder(
        Some(graph_id.clone()),
        std::path::Path::new("characters"),
    );
    let matching_set = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".animset.json"))
        .map(|(id, _)| id.clone())
        .expect("graph-bound Set must be registered");
    app.create_animation_set_document_in_folder(None, std::path::Path::new("characters"));

    assert_eq!(app.current_animation_graph_asset(), Some(graph_id.clone()));
    let sets = app.animation_sets_for_graph(&graph_id);
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].id, matching_set);
}

#[test]
fn create_animation_set_for_controller_assigns_graph_bound_set_and_opens_editor() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "ControllerSetCreateTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let scene_path = root.scenes_dir().join("main.scene.json");
    std::fs::write(&scene_path, r#"{"schema_version":1,"entities":[]}"#)
        .expect("scene fixture must be written");

    let mut session = crate::session::EditorSession::empty_behavior_tree();
    session
        .open_scene(scene_path)
        .expect("scene fixture must open");
    let entity = session
        .create_scene_entity("animated")
        .expect("entity creation must succeed");
    let component_type = ComponentTypeId::new(engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT);
    let controller = engine::builtin_registry()
        .get(&component_type)
        .expect("Animation Controller schema must exist")
        .schema
        .default_value();
    session
        .add_scene_component(entity.clone(), component_type.clone(), controller)
        .expect("Animation Controller must be added");

    let mut app = EditorApp::new(session);
    app.set_project_root(root);
    let scene_tab = app
        .session
        .summaries()
        .into_iter()
        .find(|tab| tab.kind == WorkspaceDocumentKind::Scene)
        .expect("scene tab must exist")
        .id;
    app.create_animation_graph_for_controller(entity.clone());
    let graph_id = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".anim.graph.json"))
        .map(|(id, _)| id.clone())
        .expect("created Graph must be registered");
    assert!(app.session.activate(scene_tab));

    app.create_animation_set_for_controller(entity.clone(), graph_id.clone());

    let set_id = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path.ends_with(".animset.json"))
        .map(|(id, _)| id.clone())
        .expect("created Set must be registered");
    let assigned = app
        .session
        .scene_entity(&entity)
        .and_then(|entity| entity.components.get(&component_type))
        .and_then(|value| match value {
            Value::Object(fields) => fields.get("animation_set"),
            _ => None,
        });
    assert_eq!(assigned, Some(&Value::AssetRef(set_id)));
    let editor = app
        .animation_set_editor
        .as_ref()
        .expect("created Set must open in its dedicated editor");
    assert_eq!(editor.document.graph, Some(graph_id));
}

#[test]
fn create_empty_animation_set_registers_and_opens_graphless_document() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "EmptyAnimationSetCreateTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);

    app.create_animation_set_document_in_folder(None, std::path::Path::new(""));

    let editor = app
        .animation_set_editor
        .as_ref()
        .expect("created set must open in its dedicated editor");
    assert!(editor.document.graph.is_none());
    assert!(editor.document.bindings.is_empty());
    assert!(editor.absolute_path.is_file());
}

/// Reproduces the Animation Set window jumping back to its default position
/// whenever the document was saved or its target Graph was changed.
///
/// `egui::Window` keys its stored position on the title, and this title carries
/// a dirty marker, so every clean/dirty transition renamed the window and egui
/// laid it out from scratch. An explicit window ID keeps one identity.
#[test]
fn animation_set_window_keeps_its_position_across_dirty_transitions() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "AnimationSetWindowIdentityTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);
    app.create_animation_set_document_in_folder(None, std::path::Path::new(""));

    let context = egui::Context::default();
    let run_frame = |app: &mut EditorApp, context: &egui::Context| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 900.0),
            )),
            ..egui::RawInput::default()
        };
        let _ = context.run_ui(input, |ui| {
            app.show_animation_set_editor_window(ui.ctx());
        });
        egui::AreaState::load(context, animation_set_window_id())
            .map(|state| state.left_top_pos())
            .expect("the window must lay out under its stable id, not a title-derived one")
    };

    let clean_position = run_frame(&mut app, &context);
    assert!(
        !app.animation_set_editor
            .as_ref()
            .expect("the set stays open")
            .is_dirty(),
        "a freshly created Set must start clean"
    );

    // Changing the target Graph is what put the dirty marker in the title.
    app.animation_set_editor
        .as_mut()
        .expect("the set stays open")
        .clear_graph(false);
    let dirty_position = run_frame(&mut app, &context);

    assert_eq!(
        clean_position, dirty_position,
        "the Animation Set window moved when the document became dirty"
    );
}

#[test]
fn set_project_root_reports_invalid_asset_manifest_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "BadManifestTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    // Test fixture: plain write is fine here; production manifest saves use replace_file_contents.
    std::fs::write(root.path().join("asset_manifest.json"), "{ not json")
        .expect("manifest write must succeed");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);

    assert!(app.asset_manifest.is_empty());
    assert!(
        app.session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "editor.asset_manifest_load_failed"),
        "invalid manifest should be surfaced as a diagnostic"
    );
}

#[test]
fn register_asset_from_browser_adds_obj_to_manifest_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "RegisterAssetTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    let mesh_path = root.meshes_dir().join("My Cube.obj");
    std::fs::write(
        &mesh_path,
        "v 0.0 0.0 0.0\nv 1.0 0.0 0.0\nv 0.0 1.0 0.0\nf 1 2 3\n",
    )
    .expect("mesh write must succeed");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    let index = app
        .asset_browser
        .entries()
        .iter()
        .position(|entry| entry.relative_path.ends_with("My Cube.obj"))
        .expect("mesh entry must be visible");

    app.register_asset_from_browser(index);

    assert_eq!(app.asset_manifest.len(), 1);
    let manifest_json = std::fs::read_to_string(root.path().join("asset_manifest.json"))
        .expect("manifest must be saved");
    let saved_manifest =
        engine::AssetManifest::from_json(&manifest_json).expect("manifest must parse");
    let (_, entry) = saved_manifest
        .iter()
        .next()
        .expect("manifest entry must exist");
    assert_eq!(entry.path, "meshes/My Cube.obj");
    assert_eq!(entry.name.as_deref(), Some("my_cube"));
    assert!(app
        .notifications
        .iter()
        .any(|notification| notification.message == "Registered 1 asset: My Cube.obj"));
}

#[test]
fn panel_problems_mirror_into_console_once_per_appearance() {
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    let warning = engine_authoring::Diagnostic::warning(
        "scene.component_reference_unassigned",
        "`engine.skinned_model.skeleton` is not assigned",
    );
    let info = engine_authoring::Diagnostic::info("scene.some_info", "informational");
    let problems = vec![warning, info];

    app.mirror_new_problems_to_console(&problems);
    app.mirror_new_problems_to_console(&problems);

    let mirrored = app
        .session
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == "scene.component_reference_unassigned")
        .count();
    assert_eq!(mirrored, 1, "a persisting problem must be logged only once");
    assert!(
        app.session
            .diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.code != "scene.some_info"),
        "info problems stay panel-only"
    );

    // A problem that disappears and comes back is a new appearance.
    app.mirror_new_problems_to_console(&[]);
    app.mirror_new_problems_to_console(&problems);
    let mirrored = app
        .session
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == "scene.component_reference_unassigned")
        .count();
    assert_eq!(mirrored, 2, "a reappearing problem must be logged again");
}

#[test]
fn scene_view_problem_appears_in_problems_and_console_once_until_resolved() {
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    let problem = engine_authoring::Diagnostic::error(
        "editor.scene_view.conversion_failed",
        "Scene View conversion failed: missing game component",
    );

    app.scene_view_problem = Some(problem.clone());
    app.refresh_scene_problems();
    app.refresh_scene_problems();

    assert!(app.problems_panel.problems.iter().any(|diagnostic| {
        diagnostic.code == problem.code && diagnostic.message == problem.message
    }));

    let console_count = app
        .session
        .diagnostics()
        .iter()
        .filter(|diagnostic| {
            diagnostic.code == problem.code && diagnostic.message == problem.message
        })
        .count();
    assert_eq!(console_count, 1);

    app.scene_view_problem = None;
    app.refresh_scene_problems();

    assert!(app
        .problems_panel
        .problems
        .iter()
        .all(|diagnostic| diagnostic.code != problem.code));
}

#[test]
fn registration_notification_contains_count_and_file_names() {
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());

    app.notify_registered_assets(&[
        PathBuf::from("meshes/player.obj"),
        PathBuf::from("audio/hit.wav"),
        PathBuf::from("textures/icon.png"),
    ]);

    assert_eq!(app.notifications.len(), 1);
    assert_eq!(
        app.notifications[0].message,
        "Registered 3 assets: player.obj, hit.wav, icon.png"
    );
}

#[test]
fn external_drop_refreshes_browser_and_notifies_after_manifest_commit() {
    let directory = tempfile::tempdir().unwrap();
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "ExternalDropUiTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .unwrap();
    std::fs::create_dir_all(root.assets_root().join("textures")).unwrap();
    let external = tempfile::tempdir().unwrap();
    let source = external.path().join("icon.png");
    std::fs::write(&source, b"png").unwrap();
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    assert!(app.asset_browser.set_selected_folder("textures"));

    app.import_external_asset_files(vec![source]);

    assert!(app
        .asset_browser
        .entries()
        .iter()
        .any(|entry| entry.relative_path == Path::new("textures/icon.png")));
    assert_eq!(app.asset_manifest.len(), 1);
    assert!(app
        .notifications
        .iter()
        .any(|notification| notification.message == "Registered 1 asset: icon.png"));
}

#[test]
fn external_manifest_save_failure_shows_no_success_notification() {
    let directory = tempfile::tempdir().unwrap();
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "ExternalDropSaveFailureTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .unwrap();
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    let manifest_path = root.path().join("asset_manifest.json");
    if manifest_path.is_file() {
        std::fs::remove_file(&manifest_path).unwrap();
    }
    std::fs::create_dir(&manifest_path).unwrap();
    let external = tempfile::tempdir().unwrap();
    let source = external.path().join("icon.png");
    std::fs::write(&source, b"png").unwrap();

    app.import_external_asset_files(vec![source]);

    assert!(app
        .notifications
        .iter()
        .all(|notification| !notification.message.starts_with("Registered")));
    assert!(app
        .notifications
        .iter()
        .any(|notification| matches!(notification.level, EditorNotificationLevel::Error)));
    assert!(app.asset_manifest.is_empty());
    assert!(!root.assets_root().join("icon.png").exists());
}

#[test]
fn register_gltf_finishes_background_catalog_in_manifest() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "GltfCatalogTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");
    let source_path = root.assets_root().join("hero.gltf");
    let mut positions = Vec::new();
    for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        positions.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(root.assets_root().join("hero.bin"), positions).expect("buffer fixture");
    std::fs::write(
        &source_path,
        r#"{
            "asset":{"version":"2.0"},
            "buffers":[{"uri":"hero.bin","byteLength":36}],
            "bufferViews":[{"buffer":0,"byteLength":36}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
            "meshes":[{"name":"Body","primitives":[{"attributes":{"POSITION":0}}]}]
        }"#,
    )
    .expect("source fixture");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    let index = app
        .asset_browser
        .entries()
        .iter()
        .position(|entry| entry.relative_path == Path::new("hero.gltf"))
        .expect("glTF browser entry");

    app.register_asset_from_browser(index);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let result = loop {
        if let Some(result) = app.asset_import.poll() {
            break result;
        }
        assert!(std::time::Instant::now() < deadline, "import timed out");
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    app.handle_asset_import_result(result);

    let (_, entry) = app.asset_manifest.iter().next().expect("source entry");
    assert!(entry.import_settings.source_fingerprint.is_some());
    assert_eq!(
        entry.import_settings.source_dependencies,
        vec!["hero.bin".to_owned()]
    );
    assert_eq!(entry.import_settings.sub_assets.len(), 1);
    assert_eq!(
        entry.import_settings.sub_assets[0].kind,
        engine::ImportedSubAssetKind::Mesh
    );
    let saved =
        std::fs::read_to_string(root.path().join("asset_manifest.json")).expect("saved manifest");
    let saved = engine::AssetManifest::from_json(&saved).expect("saved manifest parses");
    assert_eq!(
        saved.iter().next().expect("saved source").1.import_settings,
        entry.import_settings
    );
}

/// Writes a minimal single-mesh glTF plus its buffer into `assets/`.
fn write_model_fixture(root: &ProjectRoot, stem: &str) {
    let mut positions = Vec::new();
    for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        positions.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(root.assets_root().join(format!("{stem}.bin")), positions)
        .expect("buffer fixture");
    std::fs::write(
        root.assets_root().join(format!("{stem}.gltf")),
        format!(
            r#"{{
            "asset":{{"version":"2.0"}},
            "buffers":[{{"uri":"{stem}.bin","byteLength":36}}],
            "bufferViews":[{{"buffer":0,"byteLength":36}}],
            "accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}}],
            "meshes":[{{"name":"Body","primitives":[{{"attributes":{{"POSITION":0}}}}]}}]
        }}"#
        ),
    )
    .expect("source fixture");
}

/// Runs the queued imports to completion the way the editor frame loop does.
fn drain_model_imports(app: &mut EditorApp) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.asset_import.is_running() || !app.pending_model_imports.is_empty() {
        if let Some(result) = app.asset_import.poll() {
            app.handle_asset_import_result(result);
        }
        app.start_next_model_import();
        assert!(std::time::Instant::now() < deadline, "import timed out");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn a_model_copied_into_the_project_imports_without_being_registered_by_hand() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "AutoImportTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    // The file appears after the project is open, as a file-manager copy or
    // a branch switch would produce.
    write_model_fixture(&root, "hero");
    app.asset_browser.refresh(&root.assets_root());

    app.auto_import_model_source(Path::new("hero.gltf"));
    drain_model_imports(&mut app);

    let (_, entry) = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path == "hero.gltf")
        .expect("the model must have registered itself");
    assert!(entry.import_settings.source_fingerprint.is_some());
    assert_eq!(entry.import_settings.sub_assets.len(), 1);
    let generated = entry
        .import_settings
        .generated_prefab
        .as_ref()
        .expect("import must generate a placeable prefab");
    assert!(
        Path::new(generated).starts_with(".engine"),
        "the artifact must stay out of the asset tree, got {generated}"
    );
    assert!(root.path().join(generated).is_file());
    assert!(
        !root.assets_root().join("hero.prefab.json").exists(),
        "no artifact may appear beside the source"
    );
}

#[test]
fn a_model_already_in_the_project_imports_when_it_is_opened() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "OpenImportTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");
    // Added while the editor was closed, so the watcher reports nothing.
    write_model_fixture(&root, "hero");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    drain_model_imports(&mut app);

    let (_, entry) = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path == "hero.gltf")
        .expect("opening the project must catalog the model");
    assert!(entry.import_settings.generated_prefab.is_some());
}

#[test]
fn queued_model_imports_are_deduplicated_and_run_one_at_a_time() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "ImportQueueTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");
    write_model_fixture(&root, "first");
    write_model_fixture(&root, "second");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    drain_model_imports(&mut app);

    // A burst of repeated events for the same sources must not stack up.
    for _ in 0..3 {
        app.auto_import_model_source(Path::new("first.gltf"));
        app.auto_import_model_source(Path::new("second.gltf"));
    }

    assert!(
        app.pending_model_imports.len() <= 1,
        "one source runs while at most the other waits, got {}",
        app.pending_model_imports.len()
    );
    drain_model_imports(&mut app);
    assert_eq!(
        app.asset_manifest
            .iter()
            .filter(|(_, entry)| entry.path.ends_with(".gltf"))
            .count(),
        2,
        "repeated events must not register a source twice"
    );
}

/// Writes a placeholder `.pmx` and registers it, returning its asset ID.
///
/// The bytes are never parsed by these tests: they cover the editor's
/// routing and pairing decisions, while the actual PMX/VMD bake is covered
/// by `engine::vmd_import`'s own tests against real fixture bytes.
fn register_placeholder_pmx(app: &mut EditorApp, root: &ProjectRoot, stem: &str) -> AssetId {
    std::fs::write(root.assets_root().join(format!("{stem}.pmx")), b"PMX ")
        .expect("model placeholder");
    let id = AssetId::generate();
    app.asset_manifest.insert(
        id.clone(),
        engine::ManifestEntry {
            path: format!("{stem}.pmx"),
            name: Some(stem.to_owned()),
            import_settings: engine::ImportSettings::default(),
        },
    );
    id
}

/// Builds the smallest model-domain VMD needed by registration-routing tests.
fn minimal_model_vmd() -> Vec<u8> {
    let mut bytes = vec![0_u8; 30];
    let signature = b"Vocaloid Motion Data 0002";
    bytes[..signature.len()].copy_from_slice(signature);
    bytes.extend_from_slice(&[0_u8; 20]);
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    let mut bone_name = [0_u8; 15];
    bone_name[..6].copy_from_slice(b"center");
    bytes.extend_from_slice(&bone_name);
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    for value in [0.0_f32, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&[0_u8; 64]);
    // Empty morph, camera, light, self-shadow, and property sections retain
    // a complete modern VMD tail while the bone key classifies it as Model.
    for _ in 0..5 {
        bytes.extend_from_slice(&0_u32.to_le_bytes());
    }
    bytes
}

#[test]
fn a_motion_registered_beside_one_model_pairs_with_it_automatically() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "MotionPairTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    let model_id = register_placeholder_pmx(&mut app, &root, "character");

    std::fs::write(root.assets_root().join("dance.vmd"), minimal_model_vmd())
        .expect("model motion fixture");
    app.asset_browser.refresh(&root.assets_root());
    app.auto_import_model_source(Path::new("dance.vmd"));

    let (_, entry) = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path == "dance.vmd")
        .expect("the motion must have registered itself");
    assert_eq!(
        entry.import_settings.motion_model_sources,
        vec![model_id.as_str().to_owned()],
        "the project's only PMX must be paired without asking"
    );
}

#[test]
fn a_motion_registered_beside_several_models_is_left_unpaired() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "MotionAmbiguousTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    register_placeholder_pmx(&mut app, &root, "first");
    register_placeholder_pmx(&mut app, &root, "second");

    std::fs::write(root.assets_root().join("dance.vmd"), minimal_model_vmd())
        .expect("model motion fixture");
    app.asset_browser.refresh(&root.assets_root());
    app.auto_import_model_source(Path::new("dance.vmd"));

    let (_, entry) = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path == "dance.vmd")
        .expect("the motion must still register");
    assert!(
        entry.import_settings.motion_model_sources.is_empty(),
        "an ambiguous project must not guess which rig to bake against"
    );
    // The author has to act, so the queue must say so rather than fail
    // silently in the background.
    assert!(
        app.session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "asset.motion_source_unpaired"),
        "an unpaired motion must be reported"
    );
}

#[test]
fn sole_pmx_model_source_ignores_other_model_formats() {
    let mut manifest = engine::AssetManifest::default();
    let pmx = AssetId::generate();
    manifest.insert(
        pmx.clone(),
        engine::ManifestEntry {
            path: "character.pmx".into(),
            name: None,
            import_settings: engine::ImportSettings::default(),
        },
    );
    // An FBX or glTF carries no MMD rig, so it must never be offered as a
    // motion's pairing target even when it is the only other model.
    manifest.insert(
        AssetId::generate(),
        engine::ManifestEntry {
            path: "hero.fbx".into(),
            name: None,
            import_settings: engine::ImportSettings::default(),
        },
    );
    assert_eq!(
        super::assets::sole_pmx_model_source(&manifest),
        Some(pmx),
        "exactly one PMX must still resolve unambiguously"
    );
}

#[test]
fn motion_sources_are_importable_but_not_model_sources() {
    assert!(super::assets::is_importable_source_path(Path::new(
        "dance.vmd"
    )));
    assert!(super::assets::is_importable_source_path(Path::new(
        "character.pmx"
    )));
    // Guards the routing split: anything the model importer would be handed
    // must match `GltfSource`, and a `.vmd` must not.
    assert!(!engine::asset_path_matches_kind(
        engine::AssetKind::GltfSource,
        Path::new("dance.vmd")
    ));
}

/// VMD motion sources repeat the skeleton ledgers of their PMX bake targets,
/// but only model sources may appear as RetargetMap endpoints.
#[test]
fn retarget_map_model_source_choices_exclude_vmd_skeleton_ledgers() {
    let default_model_id = engine_authoring::AssetId::generate();
    let nt_model_id = engine_authoring::AssetId::generate();
    let motion_id = engine_authoring::AssetId::generate();
    let default_skeleton_id = engine_authoring::AssetId::generate();
    let nt_skeleton_id = engine_authoring::AssetId::generate();

    let default_skeleton = engine::SkeletonRecord {
        id: default_skeleton_id.as_str().to_owned(),
        identity: 1,
        next_bone_id: 1,
        bones: vec![engine::SkeletonBoneRecord {
            bone_id: 0,
            name: "root".to_owned(),
        }],
    };
    let nt_skeleton = engine::SkeletonRecord {
        id: nt_skeleton_id.as_str().to_owned(),
        identity: 2,
        next_bone_id: 1,
        bones: vec![engine::SkeletonBoneRecord {
            bone_id: 0,
            name: "root".to_owned(),
        }],
    };

    let mut manifest = engine::AssetManifest::default();
    manifest.insert(
        default_model_id.clone(),
        engine::ManifestEntry {
            path: "miku_default.pmx".to_owned(),
            name: Some("miku_default".to_owned()),
            import_settings: engine::ImportSettings {
                skeleton_records: vec![default_skeleton.clone()],
                ..engine::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        nt_model_id.clone(),
        engine::ManifestEntry {
            path: "miku_nt.pmx".to_owned(),
            name: Some("miku_nt".to_owned()),
            import_settings: engine::ImportSettings {
                skeleton_records: vec![nt_skeleton.clone()],
                ..engine::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        motion_id,
        engine::ManifestEntry {
            path: "girl.vmd".to_owned(),
            name: Some("girl".to_owned()),
            import_settings: engine::ImportSettings {
                // Mirrors a multi-target VMD import: these records describe
                // its PMX bake targets rather than a rig owned by the motion.
                skeleton_records: vec![default_skeleton, nt_skeleton],
                ..engine::ImportSettings::default()
            },
        },
    );

    let choices = retarget_map_model_source_choices(&manifest);
    let choice_ids = choices
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        choice_ids,
        BTreeSet::from([default_model_id, nt_model_id]),
        "only registered model sources may be RetargetMap endpoints"
    );
}

/// AP-5 creation flow for `anim.retarget_map_missing`: right-clicking a
/// registered glTF source with a recorded skeleton offers "Create Retarget
/// Map" against another such source; confirming it must write
/// `<source-stem>_to_<target-stem>.retarget.json` under `assets/` and
/// register it in the manifest, matching how other created assets (prefabs,
/// materials) are persisted.
#[test]
fn create_retarget_map_writes_file_and_registers_manifest_entry() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "RetargetCreateTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    // Written after `set_project_root` so its one-time
    // `import_models_missing_catalogs` sweep (which would otherwise
    // auto-register these sources with their own fresh IDs and empty
    // `ImportSettings`, racing the manifest built below) never sees them.
    write_model_fixture(&root, "hero");
    write_model_fixture(&root, "villain");

    let hero_id = engine_authoring::id::AssetId::generate();
    let villain_id = engine_authoring::id::AssetId::generate();
    let hero_skeleton_id = engine_authoring::id::AssetId::generate();
    let villain_skeleton_id = engine_authoring::id::AssetId::generate();
    let mut manifest = app.asset_manifest.clone();
    manifest.insert(
        hero_id.clone(),
        engine::ManifestEntry {
            path: "hero.gltf".to_owned(),
            name: Some("hero".to_owned()),
            import_settings: engine::ImportSettings {
                skeleton_records: vec![engine::SkeletonRecord {
                    id: hero_skeleton_id.as_str().to_owned(),
                    identity: 1,
                    next_bone_id: 1,
                    bones: vec![engine::SkeletonBoneRecord {
                        bone_id: 0,
                        name: "root".to_owned(),
                    }],
                }],
                ..engine::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        villain_id.clone(),
        engine::ManifestEntry {
            path: "villain.gltf".to_owned(),
            name: Some("villain".to_owned()),
            import_settings: engine::ImportSettings {
                skeleton_records: vec![engine::SkeletonRecord {
                    id: villain_skeleton_id.as_str().to_owned(),
                    identity: 2,
                    next_bone_id: 1,
                    bones: vec![engine::SkeletonBoneRecord {
                        bone_id: 0,
                        name: "root".to_owned(),
                    }],
                }],
                ..engine::ImportSettings::default()
            },
        },
    );
    app.asset_manifest = manifest;
    app.asset_browser.refresh(&root.assets_root());

    let source_index = app
        .asset_browser
        .entries()
        .iter()
        .position(|entry| entry.relative_path == Path::new("hero.gltf"))
        .expect("hero row must be visible");

    app.create_retarget_map_from_browser(source_index, villain_id);

    let expected_relative = "hero_to_villain.retarget.json";
    assert!(
        root.assets_root().join(expected_relative).is_file(),
        "the map must be written under assets/ with the source_to_target name; diagnostics: {:?}",
        app.session.diagnostics()
    );
    let (_, entry) = app
        .asset_manifest
        .iter()
        .find(|(_, entry)| entry.path == expected_relative)
        .expect("the created map must be registered in the manifest");
    assert!(matches!(
        entry.import_settings,
        engine::ImportSettings { .. }
    ));
    let json = std::fs::read_to_string(root.assets_root().join(expected_relative))
        .expect("written file must be readable");
    let map = engine::RetargetMap::from_json(&json).expect("written file must parse as a map");
    assert_eq!(
        map.bone_pairs.len(),
        1,
        "the root bones must match by name via the heuristic pre-fill"
    );
}

/// AP-7: toggling the RetargetMap inspector's "Always package" checkbox must
/// write `always_package` back to the open `*.retarget.json` file, the same
/// write path `rerun_retarget_map_name_matching` uses for its own action.
#[test]
fn set_retarget_map_always_package_writes_the_flag_to_the_file() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "RetargetAlwaysPackageTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());

    let relative_path = PathBuf::from("hero_to_villain.retarget.json");
    let map = engine::RetargetMap {
        schema_version: engine::RETARGET_MAP_SCHEMA_VERSION,
        source_skeleton: engine_authoring::id::AssetId::generate(),
        source_identity: 1,
        target_skeleton: engine_authoring::id::AssetId::generate(),
        target_identity: 2,
        bone_pairs: Vec::new(),
        chain_pairs: Vec::new(),
        translation: engine::TranslationPolicy::default(),
        always_package: false,
    };
    std::fs::write(
        root.assets_root().join(&relative_path),
        map.to_json().expect("map fixture must serialize"),
    )
    .expect("map fixture must write");
    app.retarget_map_editor = Some(RetargetMapEditorState {
        relative_path: relative_path.clone(),
        map: map.clone(),
    });

    app.set_retarget_map_always_package(true);

    assert!(
        app.retarget_map_editor
            .as_ref()
            .expect("editor state must remain open")
            .map
            .always_package,
        "in-memory editor state must reflect the toggled flag"
    );
    let saved = std::fs::read_to_string(root.assets_root().join(&relative_path))
        .expect("written file must be readable");
    let reloaded = engine::RetargetMap::from_json(&saved).expect("written file must parse");
    assert!(
        reloaded.always_package,
        "the file on disk must record always_package: true"
    );
}

/// AP-6 scope (b): when the source side records more than one skeleton
/// (multiple skins in one imported file), `create_retarget_map_from_browser`
/// must not guess via `skeleton_records.first()` — it opens the picker
/// instead, and no map file is written until a pair is confirmed.
#[test]
fn create_retarget_map_from_browser_opens_picker_when_a_side_has_multiple_skeletons() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "RetargetPickerTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    write_model_fixture(&root, "hero");
    write_model_fixture(&root, "villain");

    let hero_id = engine_authoring::id::AssetId::generate();
    let villain_id = engine_authoring::id::AssetId::generate();
    let hero_upper_id = engine_authoring::id::AssetId::generate();
    let hero_lower_id = engine_authoring::id::AssetId::generate();
    let villain_skeleton_id = engine_authoring::id::AssetId::generate();
    let mut manifest = app.asset_manifest.clone();
    manifest.insert(
        hero_id.clone(),
        engine::ManifestEntry {
            path: "hero.gltf".to_owned(),
            name: Some("hero".to_owned()),
            import_settings: engine::ImportSettings {
                sub_assets: vec![
                    engine::ImportedSubAsset {
                        id: hero_upper_id.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Skeleton,
                        name: "Upper Body".to_owned(),
                        index: 0,
                        target_model_source: None,
                    },
                    engine::ImportedSubAsset {
                        id: hero_lower_id.as_str().to_owned(),
                        kind: engine::ImportedSubAssetKind::Skeleton,
                        name: "Lower Body".to_owned(),
                        index: 1,
                        target_model_source: None,
                    },
                ],
                skeleton_records: vec![
                    engine::SkeletonRecord {
                        id: hero_upper_id.as_str().to_owned(),
                        identity: 1,
                        next_bone_id: 1,
                        bones: vec![engine::SkeletonBoneRecord {
                            bone_id: 0,
                            name: "root".to_owned(),
                        }],
                    },
                    engine::SkeletonRecord {
                        id: hero_lower_id.as_str().to_owned(),
                        identity: 2,
                        next_bone_id: 2,
                        bones: vec![
                            engine::SkeletonBoneRecord {
                                bone_id: 0,
                                name: "root".to_owned(),
                            },
                            engine::SkeletonBoneRecord {
                                bone_id: 1,
                                name: "tail".to_owned(),
                            },
                        ],
                    },
                ],
                ..engine::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        villain_id.clone(),
        engine::ManifestEntry {
            path: "villain.gltf".to_owned(),
            name: Some("villain".to_owned()),
            import_settings: engine::ImportSettings {
                skeleton_records: vec![engine::SkeletonRecord {
                    id: villain_skeleton_id.as_str().to_owned(),
                    identity: 3,
                    next_bone_id: 1,
                    bones: vec![engine::SkeletonBoneRecord {
                        bone_id: 0,
                        name: "root".to_owned(),
                    }],
                }],
                ..engine::ImportSettings::default()
            },
        },
    );
    app.asset_manifest = manifest;
    app.asset_browser.refresh(&root.assets_root());

    let source_index = app
        .asset_browser
        .entries()
        .iter()
        .position(|entry| entry.relative_path == Path::new("hero.gltf"))
        .expect("hero row must be visible");

    app.create_retarget_map_from_browser(source_index, villain_id.clone());

    assert!(
        !root
            .assets_root()
            .join("hero_to_villain.retarget.json")
            .exists(),
        "the ambiguous pair must not write a map before a selection is confirmed"
    );
    let picker = app
        .retarget_map_creation_picker
        .as_ref()
        .expect("a multi-skin source must open the picker instead of guessing");
    assert_eq!(picker.source_records.len(), 2);
    assert_eq!(picker.target_records.len(), 1);
    assert_eq!(picker.target_source_id, villain_id);

    let model = crate::anim_ux::build_retarget_map_creation_picker_model(
        &app.asset_manifest,
        &picker.source_records,
        &picker.target_records,
    );
    assert_eq!(model.source_rows[0].label, "Upper Body (1 bones)");
    assert_eq!(model.source_rows[1].label, "Lower Body (2 bones)");
}

/// AP-6 scope (b): confirming a non-default selection in the multi-skin
/// picker must write the map for the *chosen* pair, following the same
/// registration path as the single-record case (mirrors
/// `create_retarget_map_writes_file_and_registers_manifest_entry`).
#[test]
fn retarget_map_creation_picker_confirm_writes_map_for_the_selected_pair() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = ProjectRoot::create(
        directory.path(),
        engine_authoring::ProjectConfig {
            name: "RetargetPickerConfirmTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root.clone());
    write_model_fixture(&root, "hero");
    write_model_fixture(&root, "villain");

    let hero_id = engine_authoring::id::AssetId::generate();
    let villain_id = engine_authoring::id::AssetId::generate();
    let hero_upper_id = engine_authoring::id::AssetId::generate();
    let hero_lower_id = engine_authoring::id::AssetId::generate();
    let villain_skeleton_id = engine_authoring::id::AssetId::generate();
    let mut manifest = app.asset_manifest.clone();
    manifest.insert(
        hero_id.clone(),
        engine::ManifestEntry {
            path: "hero.gltf".to_owned(),
            name: Some("hero".to_owned()),
            import_settings: engine::ImportSettings {
                skeleton_records: vec![
                    engine::SkeletonRecord {
                        id: hero_upper_id.as_str().to_owned(),
                        identity: 1,
                        next_bone_id: 1,
                        bones: vec![engine::SkeletonBoneRecord {
                            bone_id: 0,
                            name: "root".to_owned(),
                        }],
                    },
                    engine::SkeletonRecord {
                        id: hero_lower_id.as_str().to_owned(),
                        identity: 2,
                        next_bone_id: 1,
                        bones: vec![engine::SkeletonBoneRecord {
                            bone_id: 0,
                            name: "root".to_owned(),
                        }],
                    },
                ],
                ..engine::ImportSettings::default()
            },
        },
    );
    manifest.insert(
        villain_id.clone(),
        engine::ManifestEntry {
            path: "villain.gltf".to_owned(),
            name: Some("villain".to_owned()),
            import_settings: engine::ImportSettings {
                skeleton_records: vec![engine::SkeletonRecord {
                    id: villain_skeleton_id.as_str().to_owned(),
                    identity: 3,
                    next_bone_id: 1,
                    bones: vec![engine::SkeletonBoneRecord {
                        bone_id: 0,
                        name: "root".to_owned(),
                    }],
                }],
                ..engine::ImportSettings::default()
            },
        },
    );
    app.asset_manifest = manifest;
    app.asset_browser.refresh(&root.assets_root());

    let source_index = app
        .asset_browser
        .entries()
        .iter()
        .position(|entry| entry.relative_path == Path::new("hero.gltf"))
        .expect("hero row must be visible");

    app.create_retarget_map_from_browser(source_index, villain_id);
    let mut state = app
        .retarget_map_creation_picker
        .take()
        .expect("a multi-skin source must open the picker");
    // Confirm the *second* recorded skeleton rather than the default first
    // one, mirroring what `show_retarget_map_creation_picker_window`'s
    // confirm branch does with the user's selection.
    state.selected_source = 1;
    let selected_source_record = state.source_records[state.selected_source].clone();
    let selected_target_record = state.target_records[state.selected_target].clone();
    app.write_retarget_map_for_pair(
        &root,
        &state.source_relative_path,
        &selected_source_record,
        &state.target_source_id,
        &selected_target_record,
    );

    let expected_relative = "hero_to_villain.retarget.json";
    let json = std::fs::read_to_string(root.assets_root().join(expected_relative))
        .expect("the map for the confirmed pair must be written");
    let map = engine::RetargetMap::from_json(&json).expect("written file must parse as a map");
    assert_eq!(
        map.source_skeleton.as_str(),
        hero_lower_id.as_str(),
        "the confirmed (second) source skeleton must be the one written, not skeleton_records[0]"
    );
}

#[test]
fn register_asset_from_browser_does_not_duplicate_existing_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "DuplicateAssetTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    std::fs::write(
        root.meshes_dir().join("cube.obj"),
        "v 0.0 0.0 0.0\nv 1.0 0.0 0.0\nv 0.0 1.0 0.0\nf 1 2 3\n",
    )
    .expect("mesh write must succeed");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);
    let index = app
        .asset_browser
        .entries()
        .iter()
        .position(|entry| entry.relative_path.ends_with("cube.obj"))
        .expect("mesh entry must be visible");

    app.register_asset_from_browser(index);
    app.register_asset_from_browser(index);

    assert_eq!(app.asset_manifest.len(), 1);
    assert!(
        app.session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "asset.already_registered"),
        "duplicate registration should produce a warning"
    );
}

#[test]
fn register_asset_from_browser_does_not_duplicate_different_case_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = ProjectRoot::create(
        dir.path(),
        engine_authoring::ProjectConfig {
            name: "CaseDupTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");
    // File on disk is lowercase; manifest records the same path with uppercase first letter.
    std::fs::write(
        root.meshes_dir().join("cube.obj"),
        "v 0.0 0.0 0.0\nv 1.0 0.0 0.0\nv 0.0 1.0 0.0\nf 1 2 3\n",
    )
    .expect("mesh write must succeed");
    let asset_id = engine_authoring::AssetId::generate();
    // Test fixture: plain write is fine here; production manifest saves use replace_file_contents.
    std::fs::write(
        root.path().join("asset_manifest.json"),
        format!(
            r#"{{"schema_version":2,"assets":{{"{}":{{"path":"meshes/Cube.obj","name":"cube"}}}}}}"#,
            asset_id.as_str()
        ),
    )
    .expect("manifest write must succeed");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.set_project_root(root);
    let index = app
        .asset_browser
        .entries()
        .iter()
        .position(|entry| entry.relative_path.ends_with("cube.obj"))
        .expect("mesh entry must be visible in browser");

    app.register_asset_from_browser(index);

    assert_eq!(app.asset_manifest.len(), 1, "manifest must not grow");
    assert!(
        app.session
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "asset.already_registered"),
        "case-variant path must be detected as duplicate"
    );
}

#[test]
fn static_mesh_renderer_added_from_schema_default_converts_to_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.scene.json");
    std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
    let mut session = crate::session::EditorSession::empty_behavior_tree();
    session.open_scene(path).expect("open_scene must succeed");
    let entity = session
        .create_scene_entity("meshed")
        .expect("entity create must succeed");

    let registry = engine::builtin_registry();
    for component in [
        "engine.transform",
        engine::scene_bridge::CAMERA_COMPONENT,
        engine::scene_bridge::DIRECTIONAL_LIGHT_COMPONENT,
        engine::scene_bridge::AMBIENT_LIGHT_COMPONENT,
        engine::scene_bridge::PLAYER_CONTROLLER_COMPONENT,
        engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
    ] {
        let definition = registry
            .get(&ComponentTypeId::new(component))
            .expect("schema must be registered");
        let schema = &definition.schema;
        session
            .add_scene_component(
                entity.clone(),
                schema.type_id.clone(),
                schema.default_value(),
            )
            .unwrap_or_else(|error| panic!("{component} add must succeed: {error}"));
    }

    RuntimePlayState::start(session.scene().expect("scene must be open"), None)
        .expect("schema defaults must convert to a runtime world");
}

#[test]
fn numeric_component_drag_commits_one_undo_entry_on_release() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.scene.json");
    std::fs::write(&path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
    let mut session = crate::session::EditorSession::empty_behavior_tree();
    session.open_scene(path).expect("open_scene must succeed");
    let entity = session
        .create_scene_entity("dragged")
        .expect("entity create must succeed");
    let component_type = ComponentTypeId::new("engine.transform");
    let component = Value::Object(std::collections::BTreeMap::from([
        ("x".into(), Value::F64(0.0)),
        ("y".into(), Value::F64(0.0)),
        ("z".into(), Value::F64(0.0)),
    ]));
    session
        .add_scene_component(entity.clone(), component_type.clone(), component.clone())
        .expect("component add must succeed");

    let mut app = EditorApp::new(session);
    let path = vec![PropertyPathSegment::Field { name: "x".into() }];
    app.apply_component_edit(
        entity.clone(),
        component_type.clone(),
        &component,
        ComponentEdit::DraftProperty {
            path: path.clone(),
            value: Value::F64(1.0),
        },
    );
    app.apply_component_edit(
        entity.clone(),
        component_type.clone(),
        &component,
        ComponentEdit::DraftProperty {
            path: path.clone(),
            value: Value::F64(2.0),
        },
    );

    assert_eq!(
        property_value(
            &app.session
                .scene()
                .unwrap()
                .entity(&entity)
                .unwrap()
                .components[&component_type],
            &path
        ),
        Some(&Value::F64(0.0)),
        "draft values must not commit while the drag is active"
    );
    let scene_preview = app
        .pending_component_drag
        .as_ref()
        .and_then(|pending| pending.scene_preview(&component))
        .expect("active numeric drag must produce a Scene View preview");
    assert_eq!(
        property_value(&scene_preview.value, &path),
        Some(&Value::F64(2.0)),
        "Scene View must receive the latest draft before pointer release"
    );

    app.apply_component_edit(
        entity.clone(),
        component_type.clone(),
        &component,
        ComponentEdit::CommitDraft { path: path.clone() },
    );

    assert_eq!(
        property_value(
            &app.session
                .scene()
                .unwrap()
                .entity(&entity)
                .unwrap()
                .components[&component_type],
            &path
        ),
        Some(&Value::F64(2.0))
    );
    assert!(
        app.session.undo(),
        "release commit must create one undo entry"
    );
    assert_eq!(
        property_value(
            &app.session
                .scene()
                .unwrap()
                .entity(&entity)
                .unwrap()
                .components[&component_type],
            &path
        ),
        Some(&Value::F64(0.0))
    );
}

#[test]
fn editing_schema_default_materializes_missing_transform_field_and_undoes() {
    let dir = tempfile::tempdir().unwrap();
    let scene_path = dir.path().join("main.scene.json");
    std::fs::write(&scene_path, r#"{"schema_version":1,"entities":[]}"#).unwrap();
    let mut session = crate::session::EditorSession::empty_behavior_tree();
    session
        .open_scene(scene_path)
        .expect("open_scene must succeed");
    let entity = session
        .create_scene_entity("legacy_transform")
        .expect("entity create must succeed");
    let component_type = ComponentTypeId::new("engine.transform");
    let legacy_component = Value::Object(std::collections::BTreeMap::from([
        ("x".into(), Value::F64(4.0)),
        ("y".into(), Value::F64(5.0)),
        ("z".into(), Value::F64(6.0)),
    ]));
    session
        .add_scene_component(
            entity.clone(),
            component_type.clone(),
            legacy_component.clone(),
        )
        .expect("legacy transform add must succeed");

    let mut app = EditorApp::new(session);
    let scale_path = vec![PropertyPathSegment::Field {
        name: "scale_x".into(),
    }];
    app.apply_component_edit(
        entity.clone(),
        component_type.clone(),
        &legacy_component,
        ComponentEdit::Property {
            path: scale_path.clone(),
            value: Value::F64(1.5),
        },
    );

    let edited = &app
        .session
        .scene()
        .unwrap()
        .entity(&entity)
        .unwrap()
        .components[&component_type];
    assert_eq!(property_value(edited, &scale_path), Some(&Value::F64(1.5)));
    assert_eq!(
        property_value(edited, &[PropertyPathSegment::Field { name: "x".into() }]),
        Some(&Value::F64(4.0)),
        "materializing a v2 field must preserve legacy transform data"
    );
    assert!(
        app.session.undo(),
        "materializing the field must create one undo entry"
    );
    let undone = &app
        .session
        .scene()
        .unwrap()
        .entity(&entity)
        .unwrap()
        .components[&component_type];
    assert_eq!(
        property_value(undone, &scale_path),
        None,
        "undo must restore the exact v1 serialized shape"
    );
}

/// The Add Component list must keep its height at the end of a long Inspector
/// column instead of shrinking into the few pixels left below the button.
///
/// The list is the last thing in a scrolling column, so the space remaining
/// under it is normally near zero; egui sizes a nested scroll area from that
/// remainder unless the area declares a floor.
#[test]
fn add_component_list_keeps_its_height_at_the_end_of_the_inspector_column() {
    let builtins = engine::builtin_registry();
    let schemas = builtins
        .definitions()
        .map(|definition| definition.schema.clone())
        .collect::<Vec<_>>();
    let available = schemas
        .iter()
        .map(|schema| (schema, "Engine"))
        .collect::<Vec<_>>();
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());

    // A headless egui context is enough: this concerns layout arithmetic, not
    // native window integration.
    let context = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(320.0, 300.0),
        )),
        ..egui::RawInput::default()
    };
    let mut section_height = 0.0_f32;

    let _ = context.run_ui(input, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            // Stand in for the component cards the picker normally follows.
            ui.allocate_space(egui::vec2(ui.available_width(), 1_000.0));
            section_height = ui
                .scope(|ui| app.show_add_component_choices(ui, "", &available))
                .response
                .rect
                .height();
        });
    });

    assert!(
        section_height >= super::inspector::ADD_COMPONENT_LIST_HEIGHT,
        "the choice list collapsed to {section_height} px below the button"
    );
}

/// Choosing a component must add it and dismiss the Add Component list.
///
/// The list previously stayed on screen after a choice because it lived in an
/// egui popup that the selection path failed to close.
#[test]
fn choosing_a_component_adds_it_and_closes_the_add_component_list() {
    let dir = tempfile::tempdir().expect("temporary project root");
    let scene_path = dir.path().join("main.scene.json");
    std::fs::write(&scene_path, r#"{"schema_version":1,"entities":[]}"#)
        .expect("scene fixture must be written");

    let mut session = crate::session::EditorSession::empty_behavior_tree();
    session
        .open_scene(scene_path)
        .expect("scene fixture must open");
    let entity = session
        .create_scene_entity("subject")
        .expect("entity creation must succeed");

    let component_type = ComponentTypeId::new(engine::scene_bridge::ANIMATION_CONTROLLER_COMPONENT);
    let schema = engine::builtin_registry()
        .get(&component_type)
        .expect("Animation Controller schema must exist")
        .schema
        .clone();

    let mut app = EditorApp::new(session);
    app.selected_entity = Some(entity.clone());
    app.add_component_picker_open = true;
    app.component_search = "anim".into();

    app.apply_add_component_choice(&schema);

    assert!(
        app.session
            .scene_entity(&entity)
            .is_some_and(|item| item.components.contains_key(&component_type)),
        "the chosen component must be added to the selected entity"
    );
    assert!(
        !app.add_component_picker_open,
        "one choice completes the interaction, so the list must close"
    );
    assert!(
        app.component_search.is_empty(),
        "the next Add Component must start from the complete list"
    );
}

#[test]
fn encode_frame_png_round_trips_rgba8_pixels() {
    let capture = FrameCapture {
        width: 2,
        height: 2,
        rgba8: vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128,
        ],
    };

    let encoded = encode_frame_png(&capture).expect("frame capture should encode as PNG");

    let decoder = png::Decoder::new(Cursor::new(encoded));
    let mut reader = decoder.read_info().expect("PNG header should decode");
    let mut pixels = vec![
        0;
        reader
            .output_buffer_size()
            .expect("known 2x2 RGBA PNG should have a bounded output size")
    ];
    let info = reader
        .next_frame(&mut pixels)
        .expect("PNG frame should decode");

    assert_eq!(info.width, capture.width);
    assert_eq!(info.height, capture.height);
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    assert_eq!(&pixels[..info.buffer_size()], capture.rgba8.as_slice());
}

#[test]
fn texture_preview_decodes_registered_source_dimensions() {
    let directory = tempfile::tempdir().expect("temporary texture directory");
    let path = directory.path().join("preview.png");
    let capture = FrameCapture {
        width: 2,
        height: 1,
        rgba8: vec![255, 0, 255, 255, 0, 255, 255, 255],
    };
    std::fs::write(&path, encode_frame_png(&capture).expect("PNG fixture"))
        .expect("preview fixture");
    let context = egui::Context::default();

    let preview = load_texture_preview(&context, &path, PathBuf::from("textures/preview.png"))
        .expect("texture preview");

    assert_eq!(preview.dimensions, [2, 1]);
    assert_eq!(preview.relative_path, PathBuf::from("textures/preview.png"));
}

/// Builds a project holding one Scene and one UI document for tab-restore tests.
fn workspace_restore_fixture(dir: &std::path::Path) -> (ProjectRoot, PathBuf, PathBuf) {
    let root = ProjectRoot::create(
        dir,
        engine_authoring::ProjectConfig {
            name: "WorkspaceRestoreTest".into(),
            schema_version: engine_authoring::PROJECT_SCHEMA_VERSION,
        },
    )
    .expect("project create must succeed");

    let scene_path = root.assets_root().join("scenes/main.scene.json");
    std::fs::write(
        &scene_path,
        engine_authoring::AuthoringScene::new()
            .to_canonical_json()
            .expect("empty scene must serialize"),
    )
    .expect("scene fixture must be written");

    let ui_path = root.assets_root().join("ui/hud.ui.json");
    std::fs::create_dir_all(ui_path.parent().expect("UI fixture has a parent"))
        .expect("UI fixture directory must be created");
    std::fs::write(
        &ui_path,
        engine_authoring::UiDocument::default()
            .to_json_string()
            .expect("default UI document must serialize"),
    )
    .expect("UI fixture must be written");

    (root, scene_path, ui_path)
}

#[test]
fn open_project_restores_every_open_document_tab_and_the_active_one() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let (root, scene_path, ui_path) = workspace_restore_fixture(dir.path());

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.preferences.open_documents = vec![scene_path.clone(), ui_path.clone()];
    // The active tab is deliberately not the last opened one, which is the
    // case a single `last_document` could not express.
    app.preferences.last_document = Some(scene_path.clone());
    app.do_open_project(root.path().to_path_buf());

    let labels: Vec<String> = app
        .session
        .summaries()
        .into_iter()
        .map(|tab| tab.label)
        .collect();
    assert_eq!(labels, vec!["main.scene.json", "hud.ui.json"]);
    assert_eq!(
        app.session.current_document_path(),
        Some(scene_path.as_path())
    );
    assert_eq!(app.preferences.open_documents, vec![scene_path, ui_path]);
}

#[test]
fn open_project_skips_workspace_documents_that_no_longer_exist() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let (root, scene_path, ui_path) = workspace_restore_fixture(dir.path());
    std::fs::remove_file(&ui_path).expect("UI fixture must be removable");

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.preferences.open_documents = vec![scene_path.clone(), ui_path];
    app.preferences.last_document = Some(scene_path.clone());
    app.do_open_project(root.path().to_path_buf());

    assert_eq!(app.session.summaries().len(), 1);
    assert_eq!(app.preferences.open_documents, vec![scene_path]);
}

#[test]
fn open_project_ignores_workspace_documents_from_another_project() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let other_dir = tempfile::tempdir().expect("second temp dir must be created");
    let (root, scene_path, _) = workspace_restore_fixture(dir.path());
    let (_, foreign_scene, _) = workspace_restore_fixture(other_dir.path());

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.preferences.open_documents = vec![foreign_scene, scene_path.clone()];
    app.do_open_project(root.path().to_path_buf());

    assert_eq!(app.preferences.open_documents, vec![scene_path]);
}

#[test]
fn legacy_preferences_restore_the_last_document_as_the_only_tab() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let (root, _, ui_path) = workspace_restore_fixture(dir.path());

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    // Preferences written before tab restore existed carry no tab list, so the
    // UI document must open instead of the project's start Scene.
    app.preferences.last_document = Some(ui_path.clone());
    app.do_open_project(root.path().to_path_buf());

    assert_eq!(app.session.summaries().len(), 1);
    assert_eq!(app.session.current_document_path(), Some(ui_path.as_path()));
}

#[test]
fn switching_and_closing_tabs_records_the_workspace_immediately() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let (root, scene_path, ui_path) = workspace_restore_fixture(dir.path());

    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.preferences.open_documents = vec![scene_path.clone(), ui_path.clone()];
    app.preferences.last_document = Some(ui_path.clone());
    app.do_open_project(root.path().to_path_buf());
    assert_eq!(app.preferences.last_document, Some(ui_path.clone()));

    let ui_tab = app.session.active_tab_id();
    let scene_tab = app
        .session
        .tab_for_path(&scene_path)
        .expect("the Scene tab must be restored");
    assert!(app.session.activate(scene_tab));
    app.on_active_document_changed(Some(ui_tab));
    assert_eq!(app.preferences.last_document, Some(scene_path.clone()));

    assert!(app.session.close_if_clean(ui_tab));
    app.on_document_closed(ui_tab, false);
    assert_eq!(app.preferences.open_documents, vec![scene_path]);
}

/// Opens a project with a Scene and a UI tab and selects one Scene entity.
///
/// Returns the app, the Scene tab, the UI tab, and the selected entity.
fn selected_entity_workspace_fixture(
    root: &ProjectRoot,
    scene_path: &Path,
    ui_path: &Path,
) -> (EditorApp, WorkspaceTabId, WorkspaceTabId, EntityId) {
    let mut app = EditorApp::new(crate::session::EditorSession::empty_behavior_tree());
    app.preferences.open_documents = vec![scene_path.to_path_buf(), ui_path.to_path_buf()];
    app.preferences.last_document = Some(scene_path.to_path_buf());
    app.do_open_project(root.path().to_path_buf());

    let scene_tab = app.session.active_tab_id();
    let ui_tab = app
        .session
        .tab_for_path(ui_path)
        .expect("the UI tab must be restored");
    let entity = app
        .session
        .create_scene_entity("selected")
        .expect("entity creation must succeed");
    app.selected_entity = Some(entity.clone());
    app.selected_entities = BTreeSet::from([entity.clone()]);
    (app, scene_tab, ui_tab, entity)
}

#[test]
fn returning_to_a_scene_tab_restores_the_selection_it_had() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let (root, scene_path, ui_path) = workspace_restore_fixture(dir.path());
    let (mut app, scene_tab, ui_tab, entity) =
        selected_entity_workspace_fixture(&root, &scene_path, &ui_path);

    assert!(app.session.activate(ui_tab));
    app.on_active_document_changed(Some(scene_tab));
    // The UI document has no Scene selection of its own to show.
    assert_eq!(app.selected_entity, None);
    assert!(app.selected_entities.is_empty());

    assert!(app.session.activate(scene_tab));
    app.on_active_document_changed(Some(ui_tab));

    assert_eq!(app.selected_entity, Some(entity.clone()));
    assert!(app.selected_entities.contains(&entity));
}

#[test]
fn closing_a_background_tab_keeps_the_drawn_document_selection() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let (root, scene_path, ui_path) = workspace_restore_fixture(dir.path());
    let (mut app, _, ui_tab, entity) =
        selected_entity_workspace_fixture(&root, &scene_path, &ui_path);

    assert!(app.session.close_if_clean(ui_tab));
    app.on_document_closed(ui_tab, false);

    assert_eq!(app.selected_entity, Some(entity.clone()));
    assert!(app.selected_entities.contains(&entity));
}

#[test]
fn reopening_the_document_already_in_front_keeps_its_selection() {
    let dir = tempfile::tempdir().expect("temp dir must be created");
    let (root, scene_path, ui_path) = workspace_restore_fixture(dir.path());
    let (mut app, scene_tab, _, entity) =
        selected_entity_workspace_fixture(&root, &scene_path, &ui_path);

    app.request_open(PendingOpen::Scene(scene_path));

    assert_eq!(app.session.active_tab_id(), scene_tab);
    assert_eq!(app.selected_entity, Some(entity.clone()));
    assert!(app.selected_entities.contains(&entity));
}
