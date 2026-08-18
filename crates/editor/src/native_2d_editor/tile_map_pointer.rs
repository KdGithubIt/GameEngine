use super::{LoadedTileMap, TileTool2d};
use eframe::egui;

pub(super) fn handle(
    ui: &egui::Ui,
    response: &egui::Response,
    loaded: &mut LoadedTileMap,
    layer: &engine_authoring::TileLayerId,
    pointer_cell: Option<engine_authoring::TileCell>,
    tool: TileTool2d,
    visible_bounds: engine_authoring::TileRect,
    work_budget: usize,
) -> Result<(), String> {
    if ui.input(|input| input.key_pressed(egui::Key::Escape)) && loaded.gesture_active {
        loaded.cancel_gesture()?;
        return Ok(());
    }

    let primary = egui::PointerButton::Primary;
    match tool {
        TileTool2d::Paint | TileTool2d::Erase => {
            handle_brush(response, loaded, layer, pointer_cell, tool)?;
        }
        TileTool2d::Rectangle | TileTool2d::Line => {
            handle_shape(response, loaded, layer, pointer_cell, tool, work_budget)?;
        }
        TileTool2d::Fill => {
            if response.clicked_by(primary) {
                let Some(cell) = pointer_cell else {
                    return Ok(());
                };
                loaded.service.begin_gesture().map_err(|error| error.to_string())?;
                loaded.gesture_active = true;
                loaded.gesture_start = Some(cell);
                if let Err(error) = loaded.service.fill_bounded(
                    layer,
                    cell,
                    visible_bounds,
                    loaded.selected_tile.clone(),
                    work_budget,
                ) {
                    loaded.cancel_gesture()?;
                    return Err(error.to_string());
                }
                loaded.commit_gesture()?;
            }
        }
        TileTool2d::Eyedropper => {
            if response.clicked_by(primary) {
                if let Some(cell) = pointer_cell {
                    loaded.selected_tile = loaded.service.eyedropper(layer, cell);
                }
            }
        }
        TileTool2d::SelectStamp => {
            handle_select_stamp(response, loaded, layer, pointer_cell, work_budget)?;
        }
    }

    Ok(())
}

fn handle_brush(
    response: &egui::Response,
    loaded: &mut LoadedTileMap,
    layer: &engine_authoring::TileLayerId,
    pointer_cell: Option<engine_authoring::TileCell>,
    tool: TileTool2d,
) -> Result<(), String> {
    let primary = egui::PointerButton::Primary;
    if response.drag_started_by(primary) {
        begin(loaded, pointer_cell)?;
        paint_brush_cell(loaded, layer, pointer_cell, tool)?;
    }
    if loaded.gesture_active && response.dragged_by(primary) {
        paint_brush_cell(loaded, layer, pointer_cell, tool)?;
    }
    if loaded.gesture_active && response.drag_stopped_by(primary) {
        loaded.commit_gesture()?;
    } else if response.clicked_by(primary) && !loaded.gesture_active {
        begin(loaded, pointer_cell)?;
        paint_brush_cell(loaded, layer, pointer_cell, tool)?;
        loaded.commit_gesture()?;
    }
    Ok(())
}

fn paint_brush_cell(
    loaded: &mut LoadedTileMap,
    layer: &engine_authoring::TileLayerId,
    cell: Option<engine_authoring::TileCell>,
    tool: TileTool2d,
) -> Result<(), String> {
    let Some(cell) = cell else {
        return Ok(());
    };
    let tile = if matches!(tool, TileTool2d::Paint) {
        loaded.selected_tile.clone()
    } else {
        None
    };
    loaded.service.paint(layer, cell, tile).map_err(|error| error.to_string())?;
    Ok(())
}

fn handle_shape(
    response: &egui::Response,
    loaded: &mut LoadedTileMap,
    layer: &engine_authoring::TileLayerId,
    pointer_cell: Option<engine_authoring::TileCell>,
    tool: TileTool2d,
    work_budget: usize,
) -> Result<(), String> {
    let primary = egui::PointerButton::Primary;
    if response.drag_started_by(primary) {
        begin(loaded, pointer_cell)?;
    }
    if loaded.gesture_active && response.drag_stopped_by(primary) {
        let start = loaded.gesture_start;
        match (start, pointer_cell) {
            (Some(start), Some(end)) => {
                apply_shape(loaded, layer, start, end, tool, work_budget)?;
                loaded.commit_gesture()?;
            }
            _ => loaded.cancel_gesture()?,
        }
    } else if response.clicked_by(primary) && !loaded.gesture_active {
        let Some(cell) = pointer_cell else {
            return Ok(());
        };
        begin(loaded, Some(cell))?;
        apply_shape(loaded, layer, cell, cell, tool, work_budget)?;
        loaded.commit_gesture()?;
    }
    Ok(())
}

fn apply_shape(
    loaded: &mut LoadedTileMap,
    layer: &engine_authoring::TileLayerId,
    start: engine_authoring::TileCell,
    end: engine_authoring::TileCell,
    tool: TileTool2d,
    work_budget: usize,
) -> Result<(), String> {
    let result = if matches!(tool, TileTool2d::Rectangle) {
        loaded.service.rectangle(
            layer,
            engine_authoring::TileRect::from_corners(start, end),
            loaded.selected_tile.clone(),
            work_budget,
        )
    } else {
        loaded.service.line(
            layer,
            start,
            end,
            loaded.selected_tile.clone(),
            work_budget,
        )
    };
    if let Err(error) = result {
        loaded.cancel_gesture()?;
        return Err(error.to_string());
    }
    Ok(())
}

fn handle_select_stamp(
    response: &egui::Response,
    loaded: &mut LoadedTileMap,
    layer: &engine_authoring::TileLayerId,
    pointer_cell: Option<engine_authoring::TileCell>,
    work_budget: usize,
) -> Result<(), String> {
    let primary = egui::PointerButton::Primary;
    if loaded.stamp.cells.is_empty() {
        if response.drag_started_by(primary) {
            loaded.gesture_start = pointer_cell;
        }
        if response.drag_stopped_by(primary) {
            if let (Some(start), Some(end)) = (loaded.gesture_start.take(), pointer_cell) {
                loaded.stamp = loaded.service.copy_stamp(
                    layer,
                    engine_authoring::TileRect::from_corners(start, end),
                );
            }
        } else if response.clicked_by(primary) {
            if let Some(cell) = pointer_cell {
                loaded.stamp = loaded.service.copy_stamp(
                    layer,
                    engine_authoring::TileRect::from_corners(cell, cell),
                );
            }
        }
        return Ok(());
    }

    if response.clicked_by(primary) {
        let Some(origin) = pointer_cell else {
            return Ok(());
        };
        let stamp = loaded.stamp.clone();
        begin(loaded, Some(origin))?;
        if let Err(error) = loaded.service.paste_stamp(layer, origin, &stamp, work_budget) {
            loaded.cancel_gesture()?;
            return Err(error.to_string());
        }
        loaded.commit_gesture()?;
    }
    Ok(())
}

fn begin(
    loaded: &mut LoadedTileMap,
    start: Option<engine_authoring::TileCell>,
) -> Result<(), String> {
    loaded.service.begin_gesture().map_err(|error| error.to_string())?;
    loaded.gesture_active = true;
    loaded.gesture_start = start;
    Ok(())
}
