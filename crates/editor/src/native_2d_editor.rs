//! Dedicated Native 2D authoring workspace presentation (ADR 0127).

use eframe::egui;

/// Transient 2D Scene View/tool state. Persisted edits are delegated to authoring services.
#[derive(Debug, Clone)]
pub struct Native2dEditorState {
    /// Whether the 2D pixel/grid overlay is visible.
    pub show_pixel_grid: bool,
    /// Whether logical sorting information is visible.
    pub show_sorting: bool,
    /// Whether 2D collider gizmos are visible.
    pub show_colliders: bool,
    /// Whether sparse Tile Map chunk boundaries are visible.
    pub show_chunks: bool,
    /// Transient editor-only view zoom.
    pub zoom: f32,
    /// Active Tile Map authoring tool.
    pub selected_tool: TileTool2d,
}

impl Default for Native2dEditorState {
    fn default() -> Self {
        Self {
            show_pixel_grid: true,
            show_sorting: true,
            show_colliders: true,
            show_chunks: true,
            zoom: 1.0,
            selected_tool: TileTool2d::Paint,
        }
    }
}

/// Tile editing gesture exposed by the dedicated workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileTool2d {
    /// Paint the selected tile into touched cells.
    Paint,
    /// Clear touched cells.
    Erase,
    /// Paint a rectangular region.
    Rectangle,
    /// Paint a rasterized line.
    Line,
    /// Flood-fill a contiguous region.
    Fill,
    /// Pick the tile under the pointer.
    Eyedropper,
    /// Select a region and stamp it elsewhere.
    SelectStamp,
}

impl Native2dEditorState {
    /// Draws a visual-validation fixture containing Scene View, Game View, Sprite, Tile Map, collider, pivot, and Camera2D cues.
    pub fn show(&mut self, ui:&mut egui::Ui) {
        ui.heading("Native 2D");
        ui.horizontal(|ui| { ui.selectable_value(&mut self.selected_tool,TileTool2d::Paint,"Paint");ui.selectable_value(&mut self.selected_tool,TileTool2d::Erase,"Erase");ui.selectable_value(&mut self.selected_tool,TileTool2d::Rectangle,"Rectangle");ui.selectable_value(&mut self.selected_tool,TileTool2d::Line,"Line");ui.selectable_value(&mut self.selected_tool,TileTool2d::Fill,"Fill");ui.selectable_value(&mut self.selected_tool,TileTool2d::Eyedropper,"Eyedropper");ui.selectable_value(&mut self.selected_tool,TileTool2d::SelectStamp,"Select/Stamp"); });
        ui.horizontal(|ui| { ui.checkbox(&mut self.show_pixel_grid,"Pixel grid");ui.checkbox(&mut self.show_sorting,"Sorting");ui.checkbox(&mut self.show_colliders,"Collider gizmos");ui.checkbox(&mut self.show_chunks,"Chunk overlay");ui.add(egui::Slider::new(&mut self.zoom,0.25..=8.0).text("Zoom")); });
        ui.separator();
        egui::Grid::new("native_2d_status").num_columns(2).show(ui,|ui|{ui.label("Scene View");ui.label("2D / XY orthographic");ui.end_row();ui.label("Game View");ui.label("Camera2D · Pixel Perfect 320×180");ui.end_row();ui.label("Sorting");ui.label("Background < World < Foreground");ui.end_row();ui.label("Tile Map");ui.label("32×32 chunks · sparse cells");ui.end_row();ui.label("Animation");ui.label("Idle · frame 2/4 · 60 ticks/s");ui.end_row();});
        ui.separator();
        let desired=egui::vec2(ui.available_width().max(480.0),420.0); let(rect,_)=ui.allocate_exact_size(desired,egui::Sense::hover()); let p=ui.painter_at(rect);p.rect_filled(rect,0.0,egui::Color32::from_gray(32));
        let center=rect.center(); let step=32.0*self.zoom.clamp(0.25,2.0);
        if self.show_pixel_grid { let mut x=center.x;while x<rect.right(){p.line_segment([egui::pos2(x,rect.top()),egui::pos2(x,rect.bottom())],egui::Stroke::new(1.0,egui::Color32::from_gray(48)));x+=step;}let mut x=center.x-step;while x>rect.left(){p.line_segment([egui::pos2(x,rect.top()),egui::pos2(x,rect.bottom())],egui::Stroke::new(1.0,egui::Color32::from_gray(48)));x-=step;}let mut y=center.y;while y<rect.bottom(){p.line_segment([egui::pos2(rect.left(),y),egui::pos2(rect.right(),y)],egui::Stroke::new(1.0,egui::Color32::from_gray(48)));y+=step;}let mut y=center.y-step;while y>rect.top(){p.line_segment([egui::pos2(rect.left(),y),egui::pos2(rect.right(),y)],egui::Stroke::new(1.0,egui::Color32::from_gray(48)));y-=step;} }
        p.line_segment([egui::pos2(rect.left(),center.y),egui::pos2(rect.right(),center.y)],egui::Stroke::new(1.5,egui::Color32::from_rgb(180,70,70)));p.line_segment([egui::pos2(center.x,rect.top()),egui::pos2(center.x,rect.bottom())],egui::Stroke::new(1.5,egui::Color32::from_rgb(70,180,90)));
        for row in 0..4 { for col in 0..9 { if (row+col)%3!=0 { let r=egui::Rect::from_min_size(egui::pos2(center.x-210.0+col as f32*46.0,center.y+65.0-row as f32*34.0),egui::vec2(44.0,32.0));p.rect_filled(r,1.0,egui::Color32::from_rgb(72,102,78)); } } }
        let sprite=egui::Rect::from_center_size(egui::pos2(center.x,center.y-28.0),egui::vec2(72.0,92.0));p.rect_filled(sprite,4.0,egui::Color32::from_rgb(105,155,225));p.circle_filled(sprite.center(),4.0,egui::Color32::YELLOW);p.text(egui::pos2(sprite.left(),sprite.top()-18.0),egui::Align2::LEFT_BOTTOM,"Sprite · pivot 0.5,0.5 · World +12",egui::FontId::proportional(13.0),egui::Color32::WHITE);
        if self.show_colliders { let c=sprite.shrink(-5.0);p.rect_stroke(c,2.0,egui::Stroke::new(2.0,egui::Color32::from_rgb(80,230,230)),egui::StrokeKind::Outside);p.line_segment([egui::pos2(c.left()-24.0,c.bottom()),egui::pos2(c.right()+24.0,c.bottom())],egui::Stroke::new(3.0,egui::Color32::from_rgb(245,180,75)));p.text(egui::pos2(c.right()+28.0,c.bottom()),egui::Align2::LEFT_CENTER,"one-way",egui::FontId::proportional(12.0),egui::Color32::from_rgb(245,200,100)); }
        let camera=egui::Rect::from_center_size(center,egui::vec2(430.0,242.0));p.rect_stroke(camera,0.0,egui::Stroke::new(2.0,egui::Color32::from_rgb(220,90,220)),egui::StrokeKind::Inside);p.text(camera.left_top()+egui::vec2(6.0,6.0),egui::Align2::LEFT_TOP,"Camera2D 16:9",egui::FontId::proportional(13.0),egui::Color32::from_rgb(235,150,235));
        if self.show_chunks { for x in [center.x-192.0,center.x,center.x+192.0] { p.line_segment([egui::pos2(x,rect.top()),egui::pos2(x,rect.bottom())],egui::Stroke::new(1.5,egui::Color32::from_rgb(180,130,60))); } }
        ui.separator();ui.label("Inspector · Transform (XY / Z Rotation / XY Scale) · SpriteRenderer2D · Rigidbody2D · Collider2D · CharacterController2D");ui.label("Gesture transaction: one tile stroke = one undo entry; Cancel restores the exact pre-stroke cells.");
    }
}
