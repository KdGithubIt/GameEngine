//! Offscreen Scene View panel with editor camera, grid, and entity picking.

use crate::gizmo::{apply_rotate_delta, apply_scale_delta, transform_component_type, GizmoAxis};
use crate::view_aspect::ViewAspect;
use crate::view_resolution::render_target_size_in_pixels;
use eframe::{egui, egui_wgpu, wgpu};
use engine::glam::{EulerRot, Mat4, Quat, Vec3, Vec4};
use engine::{
    Camera3D, DebugLines, GlobalTransform, Transform, ViewportSize, PREVIEW_COLOR_FORMAT,
    PREVIEW_DEPTH_FORMAT, PREVIEW_MSAA_SAMPLE_COUNT, PREVIEW_RENDER_FORMAT,
};
use engine_authoring::{
    AssetId, AuthoringCommand, AuthoringScene, ComponentTypeId, Diagnostic, EntityId, ProjectRoot,
    Transaction, UiDocument, Value,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

// ---------------------------------------------------------------------------
// EditorViewCamera
// ---------------------------------------------------------------------------

/// Orbit camera state for the Scene View editor camera.
pub struct EditorViewCamera {
    /// Point the camera orbits around.
    pub target: Vec3,
    /// Distance from target to camera eye.
    pub distance: f32,
    /// Horizontal orbit angle in radians.
    pub yaw: f32,
    /// Vertical orbit angle in radians (clamped away from poles).
    pub pitch: f32,
}

impl Default for EditorViewCamera {
    fn default() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 10.0,
            yaw: 0.5,
            pitch: 0.4,
        }
    }
}

impl EditorViewCamera {
    /// Restores the predictable default Scene View framing.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Frames one world-space point while preserving the current view angle.
    pub fn focus_on(&mut self, target: Vec3) {
        self.target = target;
        self.distance = 6.0;
    }

    /// Points this orbit camera so it matches an eye position and forward
    /// direction (used by "Align View to Camera").
    pub fn align_to(&mut self, eye: Vec3, forward: Vec3) {
        let forward = forward.normalize_or_zero();
        if forward == Vec3::ZERO {
            return;
        }
        let distance = self.distance.clamp(0.5, 500.0);
        self.target = eye + forward * distance;
        let offset = eye - self.target;
        self.pitch = (offset.y / distance).clamp(-1.0, 1.0).asin();
        self.yaw = offset.x.atan2(offset.z);
    }

    /// Returns the world-space eye position.
    pub fn eye(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        self.target
            + Vec3::new(
                self.distance * cp * sy,
                self.distance * sp,
                self.distance * cp * cy,
            )
    }

    /// Returns a `Transform` for the camera entity (translation + look-at rotation).
    pub fn to_transform(&self) -> Transform {
        let eye = self.eye();
        let target = self.target;
        if (eye - target).length_squared() < 1e-6 {
            return Transform::from_translation(eye);
        }
        Transform::looking_at(eye, target, Vec3::Y)
    }

    /// Handles right-drag orbit, middle-drag pan, scroll zoom, and
    /// right-button WASD/QE fly-through.
    pub fn handle_input(&mut self, response: &egui::Response) {
        let delta = response.ctx.input(|input| input.pointer.delta());
        if response.dragged_by(egui::PointerButton::Secondary) {
            // Pointer delta is per-frame. `Response::drag_delta` is cumulative
            // from drag start and caused accelerating camera motion.
            self.orbit_by_pointer_delta(delta);
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            self.pan_by_pointer_delta(delta);
        }
        let scroll = if response.hovered() {
            response.ctx.input(|i| i.smooth_scroll_delta.y)
        } else {
            0.0
        };
        if scroll != 0.0 {
            self.zoom_by_scroll(scroll);
        }
        let secondary_down = response.ctx.input(|input| input.pointer.secondary_down());
        if secondary_down && (response.hovered() || response.dragged()) {
            self.fly_by_keys(response);
        }
    }

    /// Unity-style fly-through: while the right button is held, W/S move
    /// along the view direction, A/D strafe, Q/E move down/up, and Shift
    /// boosts the speed. The orbit target moves so orbiting keeps working
    /// from the new position.
    fn fly_by_keys(&mut self, response: &egui::Response) {
        let (dt, boost, forward_key, back, left, right_key, down, up) =
            response.ctx.input(|input| {
                (
                    input.stable_dt.min(0.1),
                    input.modifiers.shift,
                    input.key_down(egui::Key::W),
                    input.key_down(egui::Key::S),
                    input.key_down(egui::Key::A),
                    input.key_down(egui::Key::D),
                    input.key_down(egui::Key::Q),
                    input.key_down(egui::Key::E),
                )
            });
        let mut motion = Vec3::ZERO;
        let view_forward = (self.target - self.eye()).normalize_or_zero();
        let view_right = view_forward.cross(Vec3::Y).normalize_or_zero();
        if forward_key {
            motion += view_forward;
        }
        if back {
            motion -= view_forward;
        }
        if right_key {
            motion += view_right;
        }
        if left {
            motion -= view_right;
        }
        if up {
            motion += Vec3::Y;
        }
        if down {
            motion -= Vec3::Y;
        }
        if motion == Vec3::ZERO {
            return;
        }
        let speed = self.distance.max(1.0) * if boost { 3.0 } else { 1.0 };
        self.target += motion.normalize_or_zero() * speed * dt;
        response.ctx.request_repaint();
    }

    fn orbit_by_pointer_delta(&mut self, delta: egui::Vec2) {
        // This follows Unity's grab-the-view direction: dragging right moves
        // the camera eye left around the pivot, while the viewed scene follows
        // the pointer.
        self.yaw -= delta.x * 0.01;
        self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.45, 1.45);
    }

    fn pan_by_pointer_delta(&mut self, delta: egui::Vec2) {
        let (sy, cy) = self.yaw.sin_cos();
        let right = Vec3::new(cy, 0.0, -sy);
        self.target -= right * delta.x * self.distance * 0.002;
        self.target += Vec3::Y * delta.y * self.distance * 0.002;
    }

    fn zoom_by_scroll(&mut self, scroll: f32) {
        self.distance = (self.distance * (1.0 - scroll * 0.002)).clamp(0.5, 500.0);
    }

    /// Returns the combined view-projection matrix.
    pub fn view_projection(&self, aspect: f32) -> Mat4 {
        let eye = self.eye();
        let view = Mat4::look_at_rh(eye, self.target, Vec3::Y);
        let proj = Mat4::perspective_rh(60_f32.to_radians(), aspect, 0.1, 1000.0);
        proj * view
    }
}

// ---------------------------------------------------------------------------
// Gizmo mode
// ---------------------------------------------------------------------------

/// The transform operation shown on gizmo handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoMode {
    /// Move the entity along world axes.
    Translate,
    /// Rotate the entity around world axes.
    Rotate,
    /// Scale the entity along local axes.
    Scale,
}

impl GizmoMode {
    /// Returns the toolbar label for this mode.
    pub fn label(self) -> &'static str {
        match self {
            Self::Translate => "Move",
            Self::Rotate => "Rotate",
            Self::Scale => "Scale",
        }
    }
}

/// Coordinate space for translate handles.
///
/// Rotation and scale fields are stored per local axis already, so the
/// toggle only changes how translate handles are oriented and applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GizmoSpace {
    /// Handles follow the world axes.
    #[default]
    Global,
    /// Handles follow the selected entity's rotated axes.
    Local,
}

impl GizmoSpace {
    /// Returns the toolbar label for this space.
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Local => "Local",
        }
    }
}

// ---------------------------------------------------------------------------
// Scene texture (offscreen color + depth)
// ---------------------------------------------------------------------------

struct SceneTexture {
    _color_texture: wgpu::Texture,
    render_view: wgpu::TextureView,
    _depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    texture_id: egui::TextureId,
    size: [u32; 2],
}

impl SceneTexture {
    fn new(render_state: &egui_wgpu::RenderState, size: [u32; 2]) -> Option<Self> {
        let device = &render_state.device;
        let color_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene_view_color"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PREVIEW_COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[PREVIEW_RENDER_FORMAT],
        });
        let render_view = color_texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(PREVIEW_RENDER_FORMAT),
            ..Default::default()
        });
        let sample_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene_view_depth"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: PREVIEW_MSAA_SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: PREVIEW_DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let texture_id = render_state.renderer.write().register_native_texture(
            device,
            &sample_view,
            wgpu::FilterMode::Linear,
        );

        Some(Self {
            _color_texture: color_texture,
            render_view,
            _depth_texture: depth_texture,
            depth_view,
            texture_id,
            size,
        })
    }

    fn release(&self, render_state: &egui_wgpu::RenderState) {
        render_state.renderer.write().free_texture(&self.texture_id);
    }
}

/// Renders game-owned egui UI into the Scene View color target.
///
/// A separate context prevents game panels from entering the editor's
/// global layer stack while the returned node reports preserve authoring
/// selection and navigation.
struct SceneUiTextureRenderer {
    context: egui::Context,
    renderer: egui_wgpu::Renderer,
}

impl SceneUiTextureRenderer {
    fn new(render_state: &egui_wgpu::RenderState) -> Self {
        let context = egui::Context::default();
        crate::install_editor_fonts(&context);
        let renderer = egui_wgpu::Renderer::new(
            &render_state.device,
            PREVIEW_RENDER_FORMAT,
            egui_wgpu::RendererOptions::default(),
        );
        Self { context, renderer }
    }

    fn render(
        &mut self,
        app: &mut engine::App,
        render_state: &egui_wgpu::RenderState,
        target: &wgpu::TextureView,
        size: [u32; 2],
        viewport: engine::UiViewport,
    ) -> Vec<engine::UiDocumentInstanceDrawReport> {
        app.install_ui_fonts(&self.context);
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(size[0] as f32, size[1] as f32),
            )),
            ..egui::RawInput::default()
        };
        let mut reports = Vec::new();
        let egui::FullOutput {
            platform_output: _,
            textures_delta,
            shapes,
            pixels_per_point,
            viewport_output: _,
        } = self.context.run_ui(raw_input, |ui| {
            reports = app.run_ui_systems_with_options(
                ui.ctx(),
                viewport,
                engine::UiDocumentDrawOptions::editor_preview(),
            );
        });
        let paint_jobs = self.context.tessellate(shapes, pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point,
        };
        let mut encoder =
            render_state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("scene_view_ui_encoder"),
                });

        for (id, image_delta) in &textures_delta.set {
            self.renderer.update_texture(
                &render_state.device,
                &render_state.queue,
                *id,
                image_delta,
            );
        }
        let callback_commands = self.renderer.update_buffers(
            &render_state.device,
            &render_state.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene_view_ui_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer.render(
                &mut render_pass.forget_lifetime(),
                &paint_jobs,
                &screen_descriptor,
            );
        }
        render_state
            .queue
            .submit(callback_commands.into_iter().chain([encoder.finish()]));
        for id in &textures_delta.free {
            self.renderer.free_texture(id);
        }
        reports
    }
}

// ---------------------------------------------------------------------------
// SceneView
// ---------------------------------------------------------------------------

/// Output returned from [`SceneView::show`].
pub struct SceneViewOutput {
    /// The egui response for the image area.
    pub response: egui::Response,
    /// Structured diagnostic represented by the current Scene View notice.
    pub preview_diagnostic: Option<Diagnostic>,
    /// Entity clicked in the Scene View, if any (Phase 28 picking).
    pub picked_entity: Option<EntityId>,
    /// Front-most declarative UI node clicked in UI selection mode.
    pub picked_ui_node: Option<SceneUiNodeSelection>,
    /// One completed transform gizmo edit. A complete pointer drag becomes one undo step.
    pub gizmo_edit: Option<GizmoEdit>,
    /// One completed AudioEmitter distance-handle edit.
    pub audio_distance_edit: Option<AudioDistanceGizmoEdit>,
    /// World-space intersection of a primary click with the authoring Z=0 plane.
    pub placement_position: Option<[f64; 3]>,
    /// Entities inside a completed marquee (box-select) drag.
    pub box_selected: Option<Vec<EntityId>>,
}

/// Interaction result from the read-only Play Mode Scene View.
pub struct PlaySceneViewOutput {
    /// Authoring entity resolved from a runtime-space pick.
    pub picked_entity: Option<EntityId>,
    /// Rendering failure reported by the live runtime viewport.
    pub render_error: Option<String>,
}

/// Editor-only selection identity for one scene-placed declarative UI node.
///
/// This value is transient presentation state. It is never serialized into
/// the Scene or UI Document and therefore cannot change game visibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneUiNodeSelection {
    /// Authoring entity that owns the `engine.ui_document` component.
    pub owner_entity: EntityId,
    /// UI Document asset referenced by the owner entity.
    pub document_asset: AssetId,
    /// Document-local node identifier selected through the runtime layout.
    pub node_id: String,
}

#[derive(Debug, Clone)]
struct SceneUiDrawRegion {
    selection: SceneUiNodeSelection,
    rect: egui::Rect,
    document_order: u64,
    node_draw_order: u64,
}

fn frontmost_ui_region(
    regions: &[SceneUiDrawRegion],
    position: egui::Pos2,
) -> Option<&SceneUiDrawRegion> {
    regions
        .iter()
        .filter(|region| region.rect.contains(position))
        .max_by_key(|region| (region.document_order, region.node_draw_order))
}
fn texture_rect_to_editor(
    texture_rect: egui::Rect,
    editor_viewport: egui::Rect,
    texture_size: [u32; 2],
) -> egui::Rect {
    let scale = egui::vec2(
        editor_viewport.width() / texture_size[0].max(1) as f32,
        editor_viewport.height() / texture_size[1].max(1) as f32,
    );
    let map = |position: egui::Pos2| {
        egui::pos2(
            editor_viewport.left() + position.x * scale.x,
            editor_viewport.top() + position.y * scale.y,
        )
    };
    egui::Rect::from_min_max(map(texture_rect.min), map(texture_rect.max))
}

#[cfg(test)]
mod scene_ui_coordinate_tests {
    use super::*;

    #[test]
    fn texture_rect_maps_to_the_presented_viewport() {
        let mapped = texture_rect_to_editor(
            egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(200.0, 100.0)),
            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 400.0)),
            [400, 200],
        );

        assert_eq!(mapped.min, egui::pos2(210.0, 120.0));
        assert_eq!(mapped.max, egui::pos2(410.0, 220.0));
    }
}

/// Shades the part of the Scene View outside the game frame and outlines it.
///
/// The frame marks which part of the free editor viewpoint the shipped screen
/// actually covers, so authored UI placement and camera framing can be read
/// without switching to Play mode.
fn draw_game_frame_guide(painter: &egui::Painter, viewport: egui::Rect, frame: egui::Rect) {
    let shade = egui::Color32::from_black_alpha(72);
    let outside = [
        egui::Rect::from_min_max(
            viewport.left_top(),
            egui::pos2(viewport.right(), frame.top()),
        ),
        egui::Rect::from_min_max(
            egui::pos2(viewport.left(), frame.bottom()),
            viewport.right_bottom(),
        ),
        egui::Rect::from_min_max(
            egui::pos2(viewport.left(), frame.top()),
            egui::pos2(frame.left(), frame.bottom()),
        ),
        egui::Rect::from_min_max(
            egui::pos2(frame.right(), frame.top()),
            egui::pos2(viewport.right(), frame.bottom()),
        ),
    ];
    for band in outside {
        if band.is_positive() {
            painter.rect_filled(band, 0.0, shade);
        }
    }
    painter.rect_stroke(
        frame,
        0.0,
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(96, 108, 124)),
        egui::StrokeKind::Inside,
    );
}

fn ui_selection_click_position(ctx: &egui::Context, viewport: egui::Rect) -> Option<egui::Pos2> {
    ctx.input(|input| {
        input
            .pointer
            .button_clicked(egui::PointerButton::Primary)
            .then(|| input.pointer.interact_pos())
            .flatten()
            .filter(|position| viewport.contains(*position))
    })
}

/// A transform change accumulated over one complete gizmo drag.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GizmoEdit {
    /// Operation active when the drag began.
    pub mode: GizmoMode,
    /// Axis handle selected by the pointer.
    pub axis: GizmoAxis,
    /// Translation units, Euler degrees, or scale multiplier delta.
    pub delta: f32,
    /// World-space direction of the dragged handle. Differs from the world
    /// axis when local-space translate handles are active.
    pub axis_direction: [f32; 3],
}

/// AudioEmitter distance field manipulated by a Scene View handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioDistanceField {
    /// Distance where positional attenuation begins.
    Min,
    /// Distance where positional attenuation reaches its floor.
    Max,
}

impl AudioDistanceField {
    /// Serialized AudioEmitter field name updated by the authoring command.
    pub(crate) const fn field_name(self) -> &'static str {
        match self {
            Self::Min => "min_distance",
            Self::Max => "max_distance",
        }
    }
}

/// One completed Scene View edit of an AudioEmitter distance handle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioDistanceGizmoEdit {
    /// Authored distance field changed by the drag.
    pub field: AudioDistanceField,
    /// Final non-negative distance in world units.
    pub distance: f32,
}

/// One transient component value rendered by the Scene View before commit.
///
/// Inspector drags use this value to update the preview world every frame
/// while retaining one authoring transaction and one undo entry on release.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneComponentPreview {
    /// Stable entity whose preview component is replaced.
    pub entity: EntityId,
    /// Stable component type replaced only in the transient preview scene.
    pub component_type: ComponentTypeId,
    /// Complete component value shown by the transient preview world.
    pub value: Value,
}

struct GizmoDragState {
    axis: GizmoAxis,
    /// World-space direction of the grabbed handle (differs from the world
    /// axis when local-space handles are active).
    axis_dir: Vec3,
    mode: GizmoMode,
    accumulated_delta: f32,
    /// Delta actually applied; equals `accumulated_delta` unless Ctrl
    /// snapping quantized it.
    effective_delta: f32,
    origin_center: Vec3,
    base_transform: Value,
}

struct AudioDistanceDragState {
    entity: EntityId,
    field: AudioDistanceField,
    direction: Vec3,
    base_distance: f32,
    min_distance: f32,
    max_distance: f32,
    accumulated_delta: f32,
    effective_distance: f32,
    center: Vec3,
}

struct EntityPickInfo {
    id: EntityId,
    center: Vec3,
    /// World-space half extents of the picking volume.
    half: Vec3,
    /// Wire icon drawn for entities with no visible mesh.
    icon: Option<EntityIcon>,
    /// Parent-resolved world matrix used for icon orientation.
    world: Mat4,
}

/// Scene View wire icon for entities that render nothing themselves.
///
/// Without an icon these entities are invisible yet still pickable, which
/// makes them impossible to find or aim at in the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntityIcon {
    Camera,
    Light,
    Audio,
    Particle,
}

/// Animation sampling mode used by a dedicated editor preview surface.
#[derive(Debug, Clone, PartialEq)]
pub enum AnimationPreviewMode {
    /// Samples one named clip without evaluating graph transitions.
    Clip {
        /// Imported clip name resolved by the target controller.
        clip: String,
    },
    /// Replays one explicit source-to-target crossfade at a chosen time.
    Transition {
        /// Clip sampled before the transition starts.
        from_clip: String,
        /// Clip sampled after the transition starts.
        to_clip: String,
        /// Seconds spent in the source clip before starting the crossfade.
        trigger_seconds: f32,
        /// Crossfade duration passed to the runtime animator.
        fade_duration: f32,
    },
    /// Runs the authored Animation Graph with transient parameter overrides.
    Graph {
        /// Boolean parameter values used only by the preview runtime.
        parameters: BTreeMap<String, bool>,
    },
}

/// Target and playback mode for a dedicated animation preview.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimationPreviewRequest {
    /// Authoring entity whose runtime Animation Controller is sampled.
    pub target: EntityId,
    /// Clip, transition, or full-graph sampling behavior.
    pub mode: AnimationPreviewMode,
}

/// Read-only runtime telemetry shown by the Animation Preview window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnimationPreviewStatus {
    /// Current clip or graph-state label.
    pub active_clip: Option<String>,
    /// Current target-clip playback position in seconds.
    pub clip_time: f32,
    /// Normalized crossfade progress while a transition is active.
    pub crossfade_progress: Option<f32>,
    /// Latest graph transition accepted by the runtime evaluator.
    pub last_transition: Option<String>,
    /// Why the selected controller cannot currently advance, when known.
    ///
    /// Dedicated previews surface this directly because their private scene
    /// conversion diagnostics are otherwise not mirrored into Problems.
    pub runtime_issue: Option<String>,
}

/// Offscreen Scene View panel: renders the authoring scene in Edit Mode.
pub struct SceneView {
    /// Editor-only orbit camera (not serialized to the scene).
    pub camera: EditorViewCamera,
    renderer: Option<engine::PreviewRenderer>,
    texture: Option<SceneTexture>,
    ui_texture_renderer: Option<SceneUiTextureRenderer>,
    entity_pick_info: Vec<EntityPickInfo>,
    gizmo_drag: Option<GizmoDragState>,
    /// Active transient drag of one AudioEmitter distance handle.
    audio_distance_drag: Option<AudioDistanceDragState>,
    /// Screen position where a marquee (box-select) drag started.
    box_select_start: Option<egui::Pos2>,
    /// Logical viewport rect and physical render-target size of the most recent
    /// frame, kept for drop targets that need to pick outside `show`.
    last_view: Option<(egui::Rect, [u32; 2])>,
    /// Draws the procedural gradient sky behind the preview scene.
    pub show_sky: bool,
    /// Transient Scene View overlay; never persisted into scene semantics.
    pub show_lod_debug: bool,
    /// Shows scene-authored declarative UI only inside the Scene View.
    pub show_ui_overlay: bool,
    /// Screen shape the UI overlay is laid out against.
    ///
    /// The Scene View panel can have any shape, but a UI document anchors and
    /// scales against the shipped screen. Laying the overlay out inside a
    /// frame with this aspect ratio keeps the preview proportional to the game
    /// instead of to the dock.
    pub game_frame_aspect: ViewAspect,
    /// Lets primary clicks select UI nodes before falling back to 3D picking.
    pub ui_selection_enabled: bool,
    selected_ui_node: Option<SceneUiNodeSelection>,
    /// Enables deterministic particle simulation in Edit Mode.
    pub particle_preview_enabled: bool,
    /// Shows live-pool bounds and emission telemetry for the selection.
    pub show_particle_debug: bool,
    particle_preview_elapsed: f32,
    /// Enables transient fixed-step animation sampling in Edit Mode.
    pub animation_preview_enabled: bool,
    /// Simulates isolated MMD secondary motion during animation preview.
    ///
    /// Gameplay rigid bodies and collision systems remain disabled. This
    /// toggle only controls imported per-character rigid-body rigs, whose
    /// Rapier worlds are isolated from gameplay collision by ADR 0096.
    pub animation_secondary_physics_enabled: bool,
    /// Playback multiplier applied only to the transient Scene View preview.
    pub animation_preview_speed: f32,
    animation_preview_elapsed: f32,
    animation_preview_request: Option<AnimationPreviewRequest>,
    animation_preview_status: Option<AnimationPreviewStatus>,
    last_particle_frame: Instant,
    preview_notice: Option<PreviewNotice>,
    /// Start of a continuous preview failure. One-frame rebuild failures are
    /// intentionally not painted so scene/document switches cannot flash red.
    preview_failure_since: Option<Instant>,
    /// Cross-frame glTF parse/decode cache (ADR 0071). Consulted whenever the
    /// preview world is rebuilt so a referenced glTF/GLB source is parsed and
    /// its images decoded at most once per edit rather than once per frame.
    gltf_cache: engine::scene_bridge::SharedGltfImportCache,
    /// Device-local mesh uploads retained across preview-world rebuilds.
    gpu_mesh_cache: engine::SharedGpuMeshCache,
    /// Manifest hash recomputed only when the manifest's revision changes.
    manifest_hash_cache: Option<(u64, u64)>,
    /// Persistent preview world reused across frames (ADR 0072). Rebuilt only
    /// when [`PreviewKey`] changes; an idle frame reuses it wholesale, doing
    /// no scene conversion, mesh copy, or GPU re-upload.
    preview: Option<PreviewWorld>,
}

/// The persistent Scene View preview world and the inputs it was built from
/// (ADR 0072).
struct PreviewWorld {
    app: engine::App,
    key: PreviewKey,
    /// Authoring-to-runtime mapping from the build, used by the transform
    /// fast path to move a dragged entity without a full rebuild. `None` when
    /// the last conversion failed outright.
    bridge: Option<engine::scene_bridge::AuthoringToRuntimeMap>,
    /// Authoring entities whose runtime [`Transform`] the fast path overrode
    /// on the previous frame. They are rewritten from the render scene each
    /// frame so a cancelled gesture restores the committed pose without a
    /// rebuild.
    transform_overrides: Vec<EntityId>,
    /// Whether the animation and pose-composition preview pipeline has already
    /// been installed in this world's fixed schedule. Installing it more than
    /// once would advance animation and secondary physics multiple times.
    animation_system_installed: bool,
    /// Whether graph evaluation was installed before animation sampling.
    animation_graph_system_installed: bool,
    /// Absolute preview time represented by the incremental graph runtime.
    animation_sampled_elapsed: f32,
    /// Render-frame time not yet consumed by a complete fixed preview step.
    ///
    /// Retaining this remainder prevents Rapier from receiving variable or
    /// extremely short final steps at the end of every rendered frame.
    animation_fixed_step_remainder: f32,
    /// Whether an explicit Transition preview has entered its target clip.
    ///
    /// The flag prevents the crossfade from being restarted on every rendered
    /// frame after the configured trigger time.
    animation_transition_started: bool,
}

/// The inputs that determine whether the persistent preview world is still
/// valid (ADR 0072).
///
/// Any change here forces one rebuild. The scene is represented by the
/// session's document revision (a scene edit bumps it); the manifest by a
/// cheap content hash (asset registration or reimport changes it). Transient
/// gesture previews are handled outside this key: transform drags use the
/// fast path, and non-transform component previews force a per-frame rebuild.
#[derive(Debug, PartialEq, Eq)]
struct PreviewKey {
    scene_revision: u64,
    manifest_hash: u64,
    game_module: Option<usize>,
    project_root: Option<std::path::PathBuf>,
    sky_enabled: bool,
    animation_preview_enabled: bool,
    animation_secondary_physics_enabled: bool,
    particle_preview_enabled: bool,
}

/// Overlay message describing a Scene View conversion or rendering problem.
enum PreviewNotice {
    /// The preview world could not be built or drawn at all.
    Failure(Diagnostic),
    /// The preview is visible but some invalid components were skipped
    /// (best-effort conversion, ADR 0068).
    SkippedComponents(Diagnostic),
}

impl PreviewNotice {
    /// Creates an error notice whose structured diagnostic is shared with the
    /// Problems and Console panels.
    fn failure(code: &'static str, message: String) -> Self {
        Self::Failure(Diagnostic::error(code, message))
    }

    /// Creates a warning notice for best-effort component skips.
    ///
    /// The first skipped component target is preserved when available so a
    /// Problems-panel click can navigate to the affected authoring component.
    fn skipped_components(
        message: String,
        target: Option<engine_authoring::DiagnosticTarget>,
    ) -> Self {
        let mut diagnostic = Diagnostic::warning("editor.scene_view.components_skipped", message);
        if let Some(target) = target {
            diagnostic = diagnostic.with_target(target);
        }
        Self::SkippedComponents(diagnostic)
    }

    /// Returns the exact diagnostic represented by this visible notice.
    fn diagnostic(&self) -> &Diagnostic {
        match self {
            Self::Failure(diagnostic) | Self::SkippedComponents(diagnostic) => diagnostic,
        }
    }
}

impl Default for SceneView {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneView {
    /// Creates a new scene view with default camera settings.
    pub fn new() -> Self {
        Self {
            camera: EditorViewCamera::default(),
            renderer: None,
            texture: None,
            ui_texture_renderer: None,
            entity_pick_info: Vec::new(),
            gizmo_drag: None,
            audio_distance_drag: None,
            box_select_start: None,
            last_view: None,
            show_sky: true,
            show_lod_debug: true,
            show_ui_overlay: true,
            game_frame_aspect: ViewAspect::Wide16x9,
            ui_selection_enabled: false,
            selected_ui_node: None,
            particle_preview_enabled: true,
            show_particle_debug: true,
            particle_preview_elapsed: 0.0,
            animation_preview_enabled: false,
            animation_secondary_physics_enabled: true,
            animation_preview_speed: 1.0,
            animation_preview_elapsed: 0.0,
            animation_preview_request: None,
            animation_preview_status: None,
            last_particle_frame: Instant::now(),
            preview_notice: None,
            preview_failure_since: None,
            gltf_cache: engine::scene_bridge::SharedGltfImportCache::default(),
            gpu_mesh_cache: engine::SharedGpuMeshCache::default(),
            manifest_hash_cache: None,
            preview: None,
        }
    }

    /// Restores the Scene View camera independently from authored cameras.
    pub fn reset_camera(&mut self) {
        self.camera.reset();
    }

    /// Resolves the current authoring world-space pose for one AudioEmitter.
    pub(crate) fn authoring_audio_emitter_pose(
        scene: &AuthoringScene,
        entity: &EntityId,
    ) -> Option<engine::audio::AudioEmitterPose> {
        scene.entity(entity)?;
        let tx_type = ComponentTypeId::new(engine::scene_bridge::TRANSFORM_COMPONENT);
        let mut memo = std::collections::BTreeMap::new();
        let world = resolve_world_matrix(entity, scene, &tx_type, &mut memo, 0);
        Some(engine::audio::AudioEmitterPose {
            position: world.transform_point3(Vec3::ZERO).to_array(),
        })
    }

    /// Resolves the current authoring world-space pose for one AudioListener.
    pub(crate) fn authoring_audio_listener_pose(
        scene: &AuthoringScene,
        entity: &EntityId,
    ) -> Option<engine::audio::AudioListenerPose> {
        scene.entity(entity)?;
        let tx_type = ComponentTypeId::new(engine::scene_bridge::TRANSFORM_COMPONENT);
        let mut memo = std::collections::BTreeMap::new();
        let world = resolve_world_matrix(entity, scene, &tx_type, &mut memo, 0);
        let right = world.x_axis.truncate().normalize_or_zero();
        Some(engine::audio::AudioListenerPose {
            position: world.transform_point3(Vec3::ZERO).to_array(),
            right: if right == Vec3::ZERO { Vec3::X } else { right }.to_array(),
        })
    }

    /// Returns the transient Scene View camera pose used only by explicit audition.
    pub(crate) fn editor_audio_listener_pose(&self) -> engine::audio::AudioListenerPose {
        let transform = self.camera.to_transform();
        let right = (transform.rotation * Vec3::X).normalize_or_zero();
        engine::audio::AudioListenerPose {
            position: transform.translation.to_array(),
            right: if right == Vec3::ZERO { Vec3::X } else { right }.to_array(),
        }
    }

    /// Synchronizes the Scene View highlight with UI Builder selection.
    ///
    /// Synchronization only applies when the Builder document is the same
    /// asset originally selected from the Scene View. Opening an unrelated UI
    /// asset does not manufacture a scene owner for it.
    pub fn sync_ui_builder_selection(&mut self, asset: &AssetId, node_id: Option<&str>) {
        let Some(selection) = &mut self.selected_ui_node else {
            return;
        };
        if &selection.document_asset != asset {
            return;
        }
        if let Some(node_id) = node_id {
            selection.node_id = node_id.to_owned();
        } else {
            self.selected_ui_node = None;
        }
    }

    /// Frames the selected entity and returns whether it existed in the scene.
    pub fn focus_entity(&mut self, scene: &AuthoringScene, entity: &EntityId) -> bool {
        let Some(center) = collect_entity_positions(scene)
            .into_iter()
            .find(|info| &info.id == entity)
            .map(|info| info.center)
        else {
            return false;
        };
        self.camera.focus_on(center);
        true
    }

    /// Restarts every Edit Mode emitter from its authored deterministic seed.
    pub fn restart_particle_preview(&mut self) {
        self.particle_preview_elapsed = 0.0;
        self.last_particle_frame = Instant::now();
    }

    /// Restarts transient Edit Mode animation sampling from each clip's first pose.
    pub fn restart_animation_preview(&mut self) {
        self.animation_preview_elapsed = 0.0;
        self.animation_preview_status = None;
        // Graph state is runtime-only. Rebuilding this transient world is the
        // deterministic way to restore Entry without mutating authoring data.
        self.preview = None;
        self.last_particle_frame = Instant::now();
    }

    /// Selects a dedicated clip, transition, or graph preview request.
    ///
    /// Changing the request rebuilds only this transient preview world; no
    /// authoring document or Play state is modified.
    pub fn set_animation_preview_request(&mut self, request: Option<AnimationPreviewRequest>) {
        if self.animation_preview_request == request {
            return;
        }
        let keeps_graph_runtime = matches!(
            (&self.animation_preview_request, &request),
            (
                Some(AnimationPreviewRequest {
                    target: previous_target,
                    mode: AnimationPreviewMode::Graph { .. },
                }),
                Some(AnimationPreviewRequest {
                    target: next_target,
                    mode: AnimationPreviewMode::Graph { .. },
                })
            ) if previous_target == next_target
        );
        self.animation_preview_request = request;
        if !keeps_graph_runtime {
            self.restart_animation_preview();
        }
    }

    /// Returns telemetry captured after the latest animation preview sample.
    pub fn animation_preview_status(&self) -> Option<&AnimationPreviewStatus> {
        self.animation_preview_status.as_ref()
    }

    /// Returns the current editor-only animation preview time in seconds.
    pub fn animation_preview_time(&self) -> f32 {
        self.animation_preview_elapsed
    }

    /// Seeks the transient animation preview without modifying runtime Play.
    pub fn seek_animation_preview(&mut self, seconds: f32) {
        let seconds = seconds.max(0.0);
        if seconds < self.animation_preview_elapsed {
            // Graph previews evaluate incrementally, so a backward seek must
            // replay from Entry in a fresh preview world.
            self.preview = None;
        }
        self.animation_preview_elapsed = seconds;
        self.animation_preview_status = None;
        self.last_particle_frame = Instant::now();
    }

    /// Releases the GPU texture registered with egui.
    pub fn release(&mut self, render_state: &egui_wgpu::RenderState) {
        if let Some(texture) = self.texture.take() {
            texture.release(render_state);
        }
        self.ui_texture_renderer = None;
    }

    /// Releases preview worlds and resident imports owned by the old project.
    pub fn clear_project_caches(&mut self) {
        self.preview = None;
        self.gltf_cache = engine::scene_bridge::SharedGltfImportCache::default();
        self.gpu_mesh_cache.clear();
        self.manifest_hash_cache = None;
    }

    /// Rebuilds the persistent preview world after an asset value or override
    /// changes without discarding reusable model and GPU caches.
    pub fn invalidate_asset_preview(&mut self) {
        self.preview = None;
        self.manifest_hash_cache = None;
    }
    /// Draws the Scene View, renders the authoring scene, and returns interaction results.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        scene: &AuthoringScene,
        scene_revision: u64,
        project_root: Option<&ProjectRoot>,
        manifest: &engine::AssetManifest,
        game_module: Option<&Arc<engine::game_module::GameModule>>,
        component_preview: Option<&SceneComponentPreview>,
        selected_entity: Option<&EntityId>,
        gizmo_mode: GizmoMode,
        gizmo_space: GizmoSpace,
        open_ui_document: Option<(&AssetId, &UiDocument)>,
        render_state: &egui_wgpu::RenderState,
    ) -> SceneViewOutput {
        if !self.show_ui_overlay {
            self.ui_selection_enabled = false;
        }
        let available = egui::vec2(
            ui.available_width().max(0.0),
            ui.available_height().max(0.0),
        );
        if available.x < 64.0 || available.y < 64.0 {
            let (rect, response) = ui.allocate_exact_size(available, egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_rgb(31, 34, 39));
            if rect.width() >= 32.0 && rect.height() >= 24.0 {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Expand Scene View",
                    egui::TextStyle::Small.resolve(ui.style()),
                    egui::Color32::GRAY,
                );
            }
            return SceneViewOutput {
                response,
                preview_diagnostic: self
                    .preview_notice
                    .as_ref()
                    .map(|notice| notice.diagnostic().clone()),
                picked_entity: None,
                picked_ui_node: None,
                gizmo_edit: None,
                audio_distance_edit: None,
                placement_position: None,
                box_selected: None,
            };
        }
        let size = render_target_size_in_pixels(
            available,
            ui.ctx().pixels_per_point(),
            render_state.device.limits().max_texture_dimension_2d,
        );

        let (rect, response) = ui.allocate_exact_size(available, egui::Sense::click_and_drag());
        self.last_view = Some((rect, size));

        self.camera.handle_input(&response);
        let now = Instant::now();
        let particle_delta = now
            .saturating_duration_since(self.last_particle_frame)
            .as_secs_f32()
            .min(0.1);
        self.last_particle_frame = now;
        if self.particle_preview_enabled {
            self.particle_preview_elapsed =
                (self.particle_preview_elapsed + particle_delta).min(5.0);
        }
        if self.animation_preview_enabled {
            self.animation_preview_elapsed = (self.animation_preview_elapsed
                + particle_delta * self.animation_preview_speed.max(0.0))
            .min(3600.0);
        }

        // Inspector drags are command-shaped but intentionally remain outside
        // the persisted authoring session until release. Applying the draft to
        // a cloned scene makes every runtime-backed component visible during
        // the gesture without generating one undo entry per frame.
        let inspector_preview_scene =
            component_preview.and_then(|preview| apply_component_preview(scene, preview));
        let interaction_scene = inspector_preview_scene.as_ref().unwrap_or(scene);

        // Collect current preview positions before hit testing so a transform
        // dragged in the Inspector moves both the entity and its gizmo handle.
        self.entity_pick_info = collect_entity_positions(interaction_scene);

        let aspect = size[0] as f32 / size[1] as f32;
        let vp = self.camera.view_projection(aspect);
        let mut placement_position = response
            .clicked_by(egui::PointerButton::Primary)
            .then(|| response.interact_pointer_pos())
            .flatten()
            .and_then(|position| pointer_plane_intersection(position, rect, vp));
        let selected_center = selected_entity
            .and_then(|sel_id| self.entity_pick_info.iter().find(|e| &e.id == sel_id))
            .map(|info| info.center);
        // Local space orients translate/scale handles along the selected
        // entity's rotated axes; rotate rings stay world-aligned because the
        // stored Euler fields are already local.
        let axis_dirs = gizmo_axis_directions(
            gizmo_space,
            gizmo_mode,
            selected_entity,
            &self.entity_pick_info,
        );

        // Audio distance handles share the Scene View pointer gesture with the
        // transform gizmo. Audio handles win only when their visible tip is hit.
        if response.drag_started() {
            let primary_down = ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary));
            if primary_down {
                self.audio_distance_drag = None;
                self.gizmo_drag = None;
                if let (Some(pos), Some(entity), Some(center), Some((min_distance, max_distance))) = (
                    response.interact_pointer_pos(),
                    selected_entity,
                    selected_center,
                    selected_entity.and_then(|entity| audio_emitter_distances(interaction_scene, entity)),
                ) {
                    let direction = audio_distance_handle_direction(&self.camera);
                    if let Some(field) = hit_test_audio_distance_handle(
                        pos, center, min_distance, max_distance, direction, vp, rect,
                    ) {
                        let base_distance = match field {
                            AudioDistanceField::Min => min_distance,
                            AudioDistanceField::Max => max_distance,
                        };
                        self.audio_distance_drag = Some(AudioDistanceDragState {
                            entity: (*entity).clone(),
                            field,
                            direction,
                            base_distance,
                            min_distance,
                            max_distance,
                            accumulated_delta: 0.0,
                            effective_distance: base_distance,
                            center,
                        });
                    }
                }

                if self.audio_distance_drag.is_none() {
                    let base_transform = selected_entity.and_then(|entity| {
                        interaction_scene
                            .entity(entity)
                            .and_then(|item| item.components.get(&transform_component_type()))
                            .cloned()
                    });
                    if let (Some(pos), Some(center), Some(base_transform)) = (
                        response.interact_pointer_pos(),
                        selected_center,
                        base_transform,
                    ) {
                        let len = gizmo_axis_length(center, vp, rect);
                        self.gizmo_drag =
                            hit_test_gizmo_axis(pos, center, vp, rect, gizmo_mode, len, &axis_dirs)
                                .map(|axis| GizmoDragState {
                                    axis,
                                    axis_dir: axis_direction_of(&axis_dirs, axis),
                                    mode: gizmo_mode,
                                    accumulated_delta: 0.0,
                                    effective_delta: 0.0,
                                    origin_center: center,
                                    base_transform,
                                });
                    }
                }
            }
        }

        // Accumulate per-frame pointer movement. Committing only at pointer-up
        // produces one authoring transaction and therefore one undo item.
        let mut audio_distance_edit = None;
        if let Some(drag) = &mut self.audio_distance_drag
            && response.dragged_by(egui::PointerButton::Primary)
        {
            let screen_delta = response.ctx.input(|input| input.pointer.delta());
            let delta = screen_delta_to_world(
                screen_delta, drag.direction, drag.center, vp, rect,
            );
            if delta.is_finite() {
                drag.accumulated_delta += delta;
            }
            drag.effective_distance = clamp_audio_distance(
                drag.field,
                drag.base_distance + drag.accumulated_delta,
                drag.min_distance,
                drag.max_distance,
            );
        }

        let mut gizmo_edit = None;
        if let Some(drag) = &mut self.gizmo_drag
            && response.dragged_by(egui::PointerButton::Primary) {
                let screen_delta = response.ctx.input(|input| input.pointer.delta());
                let delta = match drag.mode {
                    GizmoMode::Translate => screen_delta_to_world(
                        screen_delta,
                        drag.axis_dir,
                        drag.origin_center,
                        vp,
                        rect,
                    ),
                    GizmoMode::Rotate => {
                        screen_delta_along_axis(
                            screen_delta,
                            drag.axis_dir,
                            drag.origin_center,
                            vp,
                            rect,
                        ) * 0.5
                    }
                    GizmoMode::Scale => {
                        screen_delta_along_axis(
                            screen_delta,
                            drag.axis_dir,
                            drag.origin_center,
                            vp,
                            rect,
                        ) * 0.01
                    }
                };
                if delta.is_finite() {
                    drag.accumulated_delta += delta;
                }
                let modifiers = response.ctx.input(|input| input.modifiers);
                drag.effective_delta = if modifiers.ctrl || modifiers.command {
                    crate::gizmo::snap_delta(
                        drag.accumulated_delta,
                        snap_increment(drag.mode, modifiers.shift),
                    )
                } else {
                    drag.accumulated_delta
                };
            }

        let primary_down =
            ui.input(|input| input.pointer.button_down(egui::PointerButton::Primary));
        if !primary_down
            && let Some(drag) = self.audio_distance_drag.take()
            && (drag.effective_distance - drag.base_distance).abs() > f32::EPSILON
        {
            audio_distance_edit = Some(AudioDistanceGizmoEdit {
                field: drag.field,
                distance: drag.effective_distance,
            });
        }
        let mut completed_gizmo_preview = None;
        if !primary_down
            && let Some(drag) = self.gizmo_drag.take()
                && drag.effective_delta.abs() > f32::EPSILON {
                    completed_gizmo_preview =
                        selected_entity.and_then(|entity| gizmo_component_preview(entity, &drag));
                    gizmo_edit = Some(GizmoEdit {
                        mode: drag.mode,
                        axis: drag.axis,
                        delta: drag.effective_delta,
                        axis_direction: drag.axis_dir.to_array(),
                    });
                }

        // Marquee selection: a primary drag that starts on empty space (no
        // gizmo handle) selects every entity whose projected center falls in
        // the dragged rectangle.
        let mut box_selected = None;
        if response.drag_started_by(egui::PointerButton::Primary)
            && self.gizmo_drag.is_none()
            && self.audio_distance_drag.is_none()
            && !self.ui_selection_enabled
        {
            self.box_select_start = response.interact_pointer_pos();
        }
        if let Some(start) = self.box_select_start {
            let current = response
                .ctx
                .input(|input| input.pointer.latest_pos())
                .unwrap_or(start);
            let marquee = egui::Rect::from_two_pos(start, current);
            if primary_down {
                if marquee.width() > 4.0 || marquee.height() > 4.0 {
                    ui.painter().rect_stroke(
                        marquee,
                        0.0,
                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(90, 180, 255)),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().rect_filled(
                        marquee,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(90, 180, 255, 20),
                    );
                }
            } else {
                self.box_select_start = None;
                if marquee.width() > 6.0 && marquee.height() > 6.0 {
                    let ids: Vec<EntityId> = self
                        .entity_pick_info
                        .iter()
                        .filter(|info| marquee.contains(world_to_screen(info.center, vp, rect)))
                        .map(|info| info.id.clone())
                        .collect();
                    if !ids.is_empty() {
                        box_selected = Some(ids);
                    }
                }
            }
        }

        // Keep the final preview value for the pointer-up frame as well. The
        // caller commits immediately after `show` returns, so this avoids a
        // one-frame snap back to the authored value at gesture completion.
        let active_gizmo_preview = completed_gizmo_preview.or_else(|| {
            self.gizmo_drag.as_ref().and_then(|drag| {
                selected_entity.and_then(|entity| gizmo_component_preview(entity, drag))
            })
        });
        let gizmo_preview_scene = active_gizmo_preview
            .as_ref()
            .and_then(|preview| apply_component_preview(interaction_scene, preview));
        let render_scene = gizmo_preview_scene.as_ref().unwrap_or(interaction_scene);

        // Picking, selection bounds, and the rendered gizmo use the same
        // transient scene as the runtime preview, keeping every overlay locked
        // to the object while the pointer moves.
        self.entity_pick_info = collect_entity_positions(render_scene);

        let needs_new_texture = self.texture.as_ref().is_none_or(|t| t.size != size);
        if needs_new_texture {
            if let Some(old) = self.texture.take() {
                old.release(render_state);
            }
            self.texture = SceneTexture::new(render_state, size);
        }

        if self.renderer.is_none() {
            match pollster::block_on(engine::PreviewRenderer::new(
                &render_state.device,
                &render_state.queue,
                PREVIEW_RENDER_FORMAT,
            )) {
                Ok(renderer) => self.renderer = Some(renderer),
                Err(error) => {
                    self.preview_notice = Some(PreviewNotice::failure(
                        "editor.scene_view.renderer_failed",
                        format!("Scene View renderer failed: {error}"),
                    ));
                }
            }
        }

        let mut picked_entity = None;
        let mut picked_ui_node = None;
        let mut ui_draw_regions = Vec::new();
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos() {
                picked_entity = self.pick(pos, rect, size);
            }

        let manifest_hash = match self.manifest_hash_cache {
            Some((revision, hash)) if revision == manifest.revision() => hash,
            _ => {
                let hash = manifest_content_hash(manifest);
                self.manifest_hash_cache = Some((manifest.revision(), hash));
                hash
            }
        };
        if let (Some(texture), Some(renderer)) = (&self.texture, &mut self.renderer) {
            let key = PreviewKey {
                scene_revision,
                manifest_hash,
                game_module: game_module.map(|module| Arc::as_ptr(module) as usize),
                project_root: project_root.map(|root| root.assets_root()),
                sky_enabled: self.show_sky,
                animation_preview_enabled: self.animation_preview_enabled,
                animation_secondary_physics_enabled: self
                    .animation_secondary_physics_enabled,
                particle_preview_enabled: self.particle_preview_enabled,
            };

            // A non-transform Inspector preview (for example a material color
            // drag) mutates state the transform fast path cannot express, so
            // it forces a full rebuild for the duration of the gesture.
            let non_transform_preview = component_preview
                .is_some_and(|preview| preview.component_type != transform_component_type());
            let reuse =
                !non_transform_preview && self.preview.as_ref().is_some_and(|p| p.key == key);

            if !reuse {
                let (app, spawn_notice, bridge) = build_preview_app_with_sky(
                    render_scene,
                    project_root,
                    manifest,
                    game_module,
                    &self.gltf_cache,
                    &self.gpu_mesh_cache,
                    size,
                    self.show_sky,
                );
                self.preview_notice = spawn_notice;
                self.preview = Some(PreviewWorld {
                    app,
                    key,
                    bridge,
                    transform_overrides: Vec::new(),
                    animation_system_installed: false,
                    animation_graph_system_installed: false,
                    animation_sampled_elapsed: -1.0,
                    animation_fixed_step_remainder: 0.0,
                    animation_transition_started: false,
                });
            }

            let animation_preview_request = self.animation_preview_request.clone();
            let preview = self
                .preview
                .as_mut()
                .expect("preview world is built above when it is not reused");
            let animation_preview_target = animation_preview_request
                .as_ref()
                .and_then(|request| preview.bridge.as_ref()?.get(&request.target));
            let app = &mut preview.app;

            // The editor camera is re-created every frame: on a rebuild this
            // removes the authored cameras once; on a reused world it removes
            // the previous frame's editor camera before spawning the current
            // one at the orbit position.
            despawn_camera_entities(app.world_mut());
            let cam_transform = self.camera.to_transform();
            update_editor_camera(app.world_mut(), cam_transform, aspect);

            // On a reused world, move any entity being dragged now (and restore
            // any moved last frame) directly, so a gizmo or Inspector transform
            // drag stays interactive without rebuilding the world (ADR 0072).
            // A rebuilt world already reflects the render scene.
            if reuse {
                let current = current_transform_override_ids(
                    component_preview,
                    self.gizmo_drag.is_some(),
                    selected_entity,
                );
                apply_transform_overrides(
                    app,
                    preview.bridge.as_ref(),
                    render_scene,
                    &mut preview.transform_overrides,
                    current,
                );
            } else {
                preview.transform_overrides.clear();
            }

            if self.particle_preview_enabled {
                simulate_particle_preview(app.world_mut(), self.particle_preview_elapsed);
            }
            if self.animation_preview_enabled {
                // Install the complete pose pipeline once per world. Animation
                // now writes RigPose layers, so preview output must be composed
                // and published just like runtime output. Optional MMD physics
                // sits between procedural modifiers and final publication.
                if !preview.animation_system_installed {
                    let graph_enabled = matches!(
                        animation_preview_request
                            .as_ref()
                            .map(|request| &request.mode),
                        Some(AnimationPreviewMode::Graph { .. })
                    );
                    install_animation_preview_systems(
                        app,
                        graph_enabled,
                        self.animation_secondary_physics_enabled,
                    );
                    preview.animation_graph_system_installed = graph_enabled;
                    preview.animation_system_installed = true;
                }
                let mut animation_status =
                    match (animation_preview_request.as_ref(), animation_preview_target) {
                        (Some(request), Some(target)) => Some(sample_requested_animation_preview(
                            app,
                            target,
                            &request.mode,
                            self.animation_preview_elapsed,
                            &mut preview.animation_sampled_elapsed,
                            &mut preview.animation_fixed_step_remainder,
                            &mut preview.animation_transition_started,
                            preview.animation_graph_system_installed,
                        )),
                        _ => {
                            sample_animation_preview(
                                app,
                                self.animation_preview_elapsed,
                                &mut preview.animation_sampled_elapsed,
                                &mut preview.animation_fixed_step_remainder,
                            );
                            None
                        }
                    };
                if let Some(status) = &mut animation_status
                    && status.runtime_issue.is_some()
                    && let Some(diagnostic) = preview.bridge.as_ref().and_then(|bridge| {
                        bridge.asset_diagnostics.iter().find(|diagnostic| {
                            diagnostic.severity == engine_authoring::diagnostic::Severity::Error
                        })
                    })
                {
                    status.runtime_issue = Some(format!(
                        "[{}] {}",
                        diagnostic.code, diagnostic.message
                    ));
                }
                self.animation_preview_status = animation_status;
            }
            let particle_debug = self
                .show_particle_debug
                .then(|| {
                    selected_entity.and_then(|id| selected_particle_debug(app.world_mut(), id))
                })
                .flatten();

            if let Some(dl) = app.world_mut().get_resource_mut::<DebugLines>() {
                // The world persists across frames, so last frame's overlay
                // segments must be discarded before redrawing.
                dl.lines.clear();
                draw_grid(dl);
                for info in &self.entity_pick_info {
                    if let Some(icon) = info.icon {
                        draw_entity_icon(dl, info, icon);
                        if icon == EntityIcon::Camera && selected_entity == Some(&info.id) {
                            draw_camera_frustum(dl, info, render_scene);
                        }
                    }
                    if has_audio_listener(render_scene, &info.id) {
                        draw_audio_listener_orientation(dl, info);
                    }
                }
                if let Some(sel_id) = selected_entity
                    && let Some(info) = self.entity_pick_info.iter().find(|e| &e.id == sel_id) {
                        dl.aabb(info.center, info.half, Vec3::new(1.0, 1.0, 0.0));
                        let len = gizmo_axis_length(info.center, vp, rect);
                        let hovered =
                            self.gizmo_drag.as_ref().map(|drag| drag.axis).or_else(|| {
                                response.hover_pos().and_then(|pos| {
                                    hit_test_gizmo_axis(
                                        pos,
                                        info.center,
                                        vp,
                                        rect,
                                        gizmo_mode,
                                        len,
                                        &axis_dirs,
                                    )
                                })
                            });
                        draw_gizmo(dl, info.center, gizmo_mode, len, hovered, &axis_dirs);
                        if self.show_lod_debug {
                            for distance in lod_distances(render_scene, sel_id) {
                                draw_distance_ring(dl, info.center, distance);
                            }
                        }
                        if let Some((min_distance, max_distance)) = audio_emitter_display_distances(
                            render_scene,
                            sel_id,
                            self.audio_distance_drag.as_ref(),
                        ) {
                            draw_audio_distance_shell(
                                dl, info.center, min_distance, Vec3::new(0.15, 0.85, 1.0),
                            );
                            draw_audio_distance_shell(
                                dl, info.center, max_distance, Vec3::new(0.2, 0.45, 1.0),
                            );
                        }
                    }
                if let Some(debug) = &particle_debug {
                    if let Some((min, max)) = debug.bounds {
                        dl.aabb(
                            (min + max) * 0.5,
                            ((max - min) * 0.5).max(Vec3::splat(0.02)),
                            Vec3::new(0.1, 0.9, 1.0),
                        );
                    }
                    dl.line(
                        debug.origin,
                        debug.origin + debug.direction.normalize_or_zero() * 2.0,
                        Vec3::new(1.0, 0.35, 0.1),
                    );
                }
            }

            if let Some(vp) = app.world_mut().get_resource_mut::<ViewportSize>() {
                vp.width = size[0];
                vp.height = size[1];
            }
            if let Err(error) = app.ecs_mut().update() {
                self.preview_notice = Some(PreviewNotice::failure(
                    "editor.scene_view.update_failed",
                    format!("Scene View update failed: {error}"),
                ));
            }

            if let Err(error) = renderer.render_to_view(
                app.world_mut(),
                &render_state.device,
                &render_state.queue,
                &texture.render_view,
                &texture.depth_view,
            ) {
                self.preview_notice = Some(PreviewNotice::failure(
                    "editor.scene_view.render_failed",
                    format!("Scene View render failed: {error}"),
                ));
            }

            // Game UI is rendered into the same offscreen color target as the
            // 3D scene. Only the final viewport texture enters the editor context;
            // node rectangles are mapped back for authoring selection.
            let game_frame = self.game_frame_aspect.fit_rect(rect);
            if self.show_ui_overlay {
                apply_live_ui_document(app.world_mut(), open_ui_document);
                let ui_screen_size = self
                    .game_frame_aspect
                    .target_resolution()
                    .unwrap_or(game_frame.size());
                let texture_viewport = egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(size[0] as f32, size[1] as f32),
                );
                let texture_game_frame = self.game_frame_aspect.fit_rect(texture_viewport);
                let ui_renderer = self
                    .ui_texture_renderer
                    .get_or_insert_with(|| SceneUiTextureRenderer::new(render_state));
                let reports = ui_renderer.render(
                    app,
                    render_state,
                    &texture.render_view,
                    size,
                    engine::UiViewport::scaled(texture_game_frame, ui_screen_size),
                );
                for report in reports {
                    let Some(identity) = app
                        .world()
                        .get_component::<engine::RuntimeEntityIdentity>(report.entity)
                    else {
                        continue;
                    };
                    for node in report.nodes {
                        ui_draw_regions.push(SceneUiDrawRegion {
                            selection: SceneUiNodeSelection {
                                owner_entity: identity.authoring_id.clone(),
                                document_asset: report.asset.clone(),
                                node_id: node.node_id,
                            },
                            rect: texture_rect_to_editor(node.rect, rect, size),
                            document_order: report.document_order,
                            node_draw_order: node.draw_order,
                        });
                    }
                }
            }

            ui.painter().image(
                texture.texture_id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );

            // A UI document anchors and scales against the shipped screen, not
            // against the dock. The Scene View image keeps filling the panel
            // because the editor camera may look anywhere, but everything that
            // represents the game screen is laid out inside this frame.
            if game_frame != rect {
                draw_game_frame_guide(&ui.painter().with_clip_rect(rect), rect, game_frame);
            }

            if let Some(sel_id) = selected_entity
                && let Some(info) = self.entity_pick_info.iter().find(|info| &info.id == sel_id)
                && let Some((min_distance, max_distance)) = audio_emitter_display_distances(
                    render_scene,
                    sel_id,
                    self.audio_distance_drag.as_ref(),
                )
            {
                draw_audio_distance_handles(
                    &ui.painter().with_clip_rect(rect),
                    info.center,
                    audio_distance_handle_direction(&self.camera),
                    min_distance,
                    max_distance,
                    vp,
                    rect,
                    self.audio_distance_drag.as_ref().map(|drag| drag.field),
                );
            }

            if self.show_ui_overlay {
                // UI interaction is metadata-driven: the baked texture never enters
                // the editor's widget layer or emits gameplay events.
                if self.ui_selection_enabled
                    && let Some(position) = ui_selection_click_position(ui.ctx(), game_frame)
                        && let Some(region) = frontmost_ui_region(&ui_draw_regions, position) {
                            let selection = region.selection.clone();
                            self.selected_ui_node = Some(selection.clone());
                            picked_ui_node = Some(selection);
                            picked_entity = None;
                            placement_position = None;
                        }

                // An empty UI-selection click deliberately falls through to
                // the previously computed 3D pick. Selecting 3D clears the UI
                // highlight so the two editing targets never appear active at
                // the same time.
                if response.clicked() && picked_ui_node.is_none() && picked_entity.is_some() {
                    self.selected_ui_node = None;
                }

                if let Some(region) = self.selected_ui_node.as_ref().and_then(|selection| {
                    ui_draw_regions
                        .iter()
                        .filter(|region| &region.selection == selection)
                        .max_by_key(|region| (region.document_order, region.node_draw_order))
                }) {
                    // Keep the authoring outline above the composed viewport image,
                    // but inside the Scene View's own editor layer. A global
                    // Foreground painter would cross independent editor windows.
                    ui.painter().with_clip_rect(game_frame).rect_stroke(
                        region.rect.expand(1.0),
                        0.0,
                        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(80, 205, 255)),
                        egui::StrokeKind::Outside,
                    );
                }
            } else if response.clicked() && picked_entity.is_some() {
                self.selected_ui_node = None;
            }

            if self.show_lod_debug
                && let Some(sel_id) = selected_entity
                    && let Some(info) = self.entity_pick_info.iter().find(|info| &info.id == sel_id)
                    {
                        let distances = lod_distances(render_scene, sel_id);
                        if !distances.is_empty() {
                            let camera_distance = self.camera.eye().distance(info.center);
                            let active = distances
                                .iter()
                                .position(|threshold| camera_distance < *threshold)
                                .unwrap_or(distances.len() - 1);
                            ui.painter().text(
                                rect.left_top() + egui::vec2(8.0, 8.0),
                                egui::Align2::LEFT_TOP,
                                format!("LOD {active}  |  camera {camera_distance:.1} m"),
                                egui::TextStyle::Monospace.resolve(ui.style()),
                                egui::Color32::YELLOW,
                            );
                        }
                    }
            if let Some(debug) = particle_debug {
                ui.painter().text(
                    rect.left_bottom() + egui::vec2(8.0, -8.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!(
                        "Particles {} / {}  |  {:.1}/s  |  preview {:.2}s",
                        debug.live, debug.maximum, debug.spawn_rate, self.particle_preview_elapsed
                    ),
                    egui::TextStyle::Monospace.resolve(ui.style()),
                    egui::Color32::from_rgb(80, 230, 255),
                );
            }

            let _ = aspect;
        }

        let preview_notice_is_failure = matches!(
            self.preview_notice.as_ref(),
            Some(PreviewNotice::Failure(_))
        );
        let preview_notice_ready = if preview_notice_is_failure {
            let started = self.preview_failure_since.get_or_insert_with(Instant::now);
            started.elapsed() >= std::time::Duration::from_millis(120)
        } else {
            self.preview_failure_since = None;
            self.preview_notice.is_some()
        };
        if preview_notice_ready {
            let notice = self
                .preview_notice
                .as_ref()
                .expect("a preview notice exists when it is ready to draw");
            let diagnostic = notice.diagnostic();
            let (background, foreground) = match notice {
                PreviewNotice::Failure(_) => (
                    egui::Color32::from_rgba_unmultiplied(80, 12, 12, 220),
                    egui::Color32::LIGHT_RED,
                ),
                PreviewNotice::SkippedComponents(_) => (
                    egui::Color32::from_rgba_unmultiplied(84, 62, 8, 220),
                    egui::Color32::LIGHT_YELLOW,
                ),
            };
            let text_padding = 12.0;
            let notice_width = (rect.width() - 16.0).max(1.0);
            let text_width = (notice_width - text_padding * 2.0).max(1.0);
            let galley = ui.painter().layout(
                diagnostic.message.clone(),
                egui::FontId::proportional(18.0),
                foreground,
                text_width,
            );
            let maximum_height = (rect.height() - 16.0).max(1.0);
            let notice_height = (galley.size().y + text_padding * 2.0)
                .max(56.0)
                .min(maximum_height);
            let notice_rect = egui::Rect::from_min_size(
                rect.left_top() + egui::vec2(8.0, 8.0),
                egui::vec2(notice_width, notice_height),
            );
            ui.painter().rect_filled(notice_rect, 4.0, background);
            ui.painter()
                .with_clip_rect(notice_rect.shrink(text_padding))
                .galley(
                    notice_rect.min + egui::vec2(text_padding, text_padding),
                    galley,
                    foreground,
                );
        }

        SceneViewOutput {
            response,
            preview_diagnostic: self
                .preview_notice
                .as_ref()
                .map(|notice| notice.diagnostic().clone()),
            picked_entity,
            picked_ui_node,
            gizmo_edit,
            audio_distance_edit,
            placement_position,
            box_selected,
        }
    }

    /// Draws the live Play world through the editor camera without editing it.
    pub(crate) fn show_play(
        &mut self,
        ui: &mut egui::Ui,
        runtime: &mut crate::runtime::RuntimePlayState,
        render_state: &egui_wgpu::RenderState,
    ) -> PlaySceneViewOutput {
        let available = egui::vec2(
            ui.available_width().max(1.0),
            ui.available_height().max(1.0),
        );
        let size = render_target_size_in_pixels(
            available,
            ui.ctx().pixels_per_point(),
            render_state.device.limits().max_texture_dimension_2d,
        );
        let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
        let (rect, response) = ui.allocate_exact_size(
            available,
            egui::Sense::click_and_drag(),
        );
        // Apply orbit/fly input before constructing both the render camera and
        // the picking ray so the image and interaction use one camera pose.
        self.camera.handle_input(&response);
        let camera = Camera3D::new(60.0, aspect, 0.1, 1000.0);
        let camera_transform = self.camera.to_transform();

        let texture = match runtime.render_scene_view(
            render_state,
            size,
            &camera,
            &camera_transform,
        ) {
            Ok(texture) => texture,
            Err(error) => {
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_rgb(31, 34, 39));
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("Play Scene View failed: {error}"),
                    egui::TextStyle::Body.resolve(ui.style()),
                    egui::Color32::LIGHT_RED,
                );
                return PlaySceneViewOutput {
                    picked_entity: None,
                    render_error: Some(error.to_string()),
                };
            }
        };
        ui.painter().image(
            texture,
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        let picked_entity = response
            .clicked_by(egui::PointerButton::Primary)
            .then(|| response.interact_pointer_pos())
            .flatten()
            .and_then(|position| {
                let ray = screen_ray(
                    position,
                    response.rect,
                    self.camera.view_projection(aspect),
                )?;
                pick_runtime_bounds(ray, runtime.scene_view_pick_info())
            });

        PlaySceneViewOutput {
            picked_entity,
            render_error: None,
        }
    }

    /// Picks the entity under a screen position using the viewport of the
    /// most recently shown frame.
    ///
    /// Drop targets receive their payload outside [`SceneView::show`], after
    /// the frame's rect and pixel size were already consumed; this replays
    /// the same ray cast against the stored view.
    pub fn pick_last_frame(&self, screen_pos: egui::Pos2) -> Option<EntityId> {
        let (rect, size) = self.last_view?;
        if !rect.contains(screen_pos) {
            return None;
        }
        self.pick(screen_pos, rect, size)
    }

    fn pick(
        &self,
        screen_pos: egui::Pos2,
        view_rect: egui::Rect,
        size: [u32; 2],
    ) -> Option<EntityId> {
        let aspect = size[0] as f32 / size[1] as f32;
        let vp = self.camera.view_projection(aspect);
        let (ray_origin, ray_dir) = screen_ray(screen_pos, view_rect, vp)?;

        let mut best: Option<(f32, EntityId)> = None;
        for info in &self.entity_pick_info {
            if let Some(t) = ray_aabb(
                ray_origin,
                ray_dir,
                info.center - info.half,
                info.center + info.half,
            )
                && best.as_ref().is_none_or(|(bt, _)| t < *bt) {
                    best = Some((t, info.id.clone()));
                }
        }
        best.map(|(_, id)| id)
    }
}

fn screen_ray(
    screen_pos: egui::Pos2,
    view_rect: egui::Rect,
    view_projection: Mat4,
) -> Option<(Vec3, Vec3)> {
    if view_rect.width() <= 0.0 || view_rect.height() <= 0.0 {
        return None;
    }
    let ndc_x = ((screen_pos.x - view_rect.min.x) / view_rect.width()) * 2.0 - 1.0;
    let ndc_y = 1.0 - ((screen_pos.y - view_rect.min.y) / view_rect.height()) * 2.0;
    let inverse = view_projection.inverse();
    let near = inverse * Vec4::new(ndc_x, ndc_y, -1.0, 1.0);
    let far = inverse * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
    if near.w.abs() < 1e-6 || far.w.abs() < 1e-6 {
        return None;
    }
    let origin = near.truncate() / near.w;
    let direction = (far.truncate() / far.w - origin).normalize_or_zero();
    (direction != Vec3::ZERO).then_some((origin, direction))
}

#[cfg(test)]
mod viewport_coordinate_tests {
    use super::*;

    fn assert_vec3_near(actual: Vec3, expected: Vec3) {
        assert!(
            (actual - expected).length() < 1e-6,
            "actual {actual:?}, expected {expected:?}"
        );
    }

    #[test]
    fn screen_ray_uses_the_logical_view_center() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 480.0));
        let (origin, direction) = screen_ray(rect.center(), rect, Mat4::IDENTITY)
            .expect("the center of a positive viewport must produce a ray");

        assert_vec3_near(origin, Vec3::new(0.0, 0.0, -1.0));
        assert_vec3_near(direction, Vec3::Z);
    }

    #[test]
    fn screen_ray_uses_the_logical_view_corners() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 480.0));

        let (top_left_origin, top_left_direction) =
            screen_ray(rect.left_top(), rect, Mat4::IDENTITY)
                .expect("the top-left corner must produce a ray");
        assert_vec3_near(top_left_origin, Vec3::new(-1.0, 1.0, -1.0));
        assert_vec3_near(top_left_direction, Vec3::Z);

        let (bottom_right_origin, bottom_right_direction) =
            screen_ray(rect.right_bottom(), rect, Mat4::IDENTITY)
                .expect("the bottom-right corner must produce a ray");
        assert_vec3_near(bottom_right_origin, Vec3::new(1.0, -1.0, -1.0));
        assert_vec3_near(bottom_right_direction, Vec3::Z);
    }
}

fn pick_runtime_bounds(
    ray: (Vec3, Vec3),
    bounds: Vec<(EntityId, Vec3, Vec3)>,
) -> Option<EntityId> {
    bounds
        .into_iter()
        .filter_map(|(entity, center, half)| {
            ray_aabb(ray.0, ray.1, center - half, center + half)
                .map(|distance| (distance, entity))
        })
        .min_by(|left, right| {
            left.0
                .partial_cmp(&right.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(_, entity)| entity)
}

/// Applies one transient component replacement through the shared command path.
///
/// The returned scene is detached from the editor's [`AuthoringSession`], so
/// rebuilding the runtime preview cannot mark the document dirty or append
/// undo history. Invalid drafts are ignored and the caller keeps rendering the
/// last committed scene.
fn apply_component_preview(
    scene: &AuthoringScene,
    preview: &SceneComponentPreview,
) -> Option<AuthoringScene> {
    let mut preview_scene = scene.clone();
    let mut transaction = Transaction::begin(scene);
    let result = transaction.apply(AuthoringCommand::SetComponentValue {
        entity: preview.entity.clone(),
        component_type: preview.component_type.clone(),
        value: preview.value.clone(),
    });
    if !result.is_ok() {
        return None;
    }
    transaction.commit(&mut preview_scene).ok()?;
    Some(preview_scene)
}

/// Converts the accumulated gizmo gesture into a complete transform preview.
///
/// The base value is captured when the pointer first grabs the handle. This
/// prevents the per-frame delta from feeding back through the already moved
/// transform, which otherwise makes the object oscillate or accelerate.
fn gizmo_component_preview(
    entity: &EntityId,
    drag: &GizmoDragState,
) -> Option<SceneComponentPreview> {
    if drag.mode == GizmoMode::Translate {
        // Local-space handles may move along an oblique world direction, so
        // the translation applies as a vector across x/y/z.
        let world_delta = drag.axis_dir * drag.effective_delta;
        let value = crate::gizmo::apply_translate_vector(
            &drag.base_transform,
            [
                world_delta.x as f64,
                world_delta.y as f64,
                world_delta.z as f64,
            ],
        )?;
        return Some(SceneComponentPreview {
            entity: entity.clone(),
            component_type: transform_component_type(),
            value,
        });
    }
    let (path, replacement) = match drag.mode {
        GizmoMode::Translate => unreachable!("translate handled above"),
        GizmoMode::Rotate => {
            apply_rotate_delta(&drag.base_transform, drag.axis, drag.effective_delta)
        }
        GizmoMode::Scale => {
            apply_scale_delta(&drag.base_transform, drag.axis, drag.effective_delta)
        }
    }?;
    let [engine_authoring::PropertyPathSegment::Field { name }] = path.as_slice() else {
        return None;
    };
    let mut value = drag.base_transform.clone();
    let Value::Object(fields) = &mut value else {
        return None;
    };
    fields.insert(name.clone(), replacement);
    Some(SceneComponentPreview {
        entity: entity.clone(),
        component_type: transform_component_type(),
        value,
    })
}

fn pointer_plane_intersection(
    position: egui::Pos2,
    viewport: egui::Rect,
    view_projection: Mat4,
) -> Option<[f64; 3]> {
    if viewport.width() <= 0.0 || viewport.height() <= 0.0 {
        return None;
    }
    let x = ((position.x - viewport.left()) / viewport.width()) * 2.0 - 1.0;
    let y = 1.0 - ((position.y - viewport.top()) / viewport.height()) * 2.0;
    let inverse = view_projection.inverse();
    let near = inverse.project_point3(Vec3::new(x, y, 0.0));
    let far = inverse.project_point3(Vec3::new(x, y, 1.0));
    let direction = far - near;
    if direction.z.abs() <= f32::EPSILON {
        return None;
    }
    let distance = -near.z / direction.z;
    if distance < 0.0 {
        return None;
    }
    let point = near + direction * distance;
    Some([point.x as f64, point.y as f64, 0.0])
}

// ---------------------------------------------------------------------------
// Preview world helpers
// ---------------------------------------------------------------------------

// Preview construction needs each independently revisioned input explicitly;
// bundling them would hide cache ownership and project-lifetime boundaries.
#[allow(clippy::too_many_arguments)]
fn build_preview_app_with_sky(
    scene: &AuthoringScene,
    project_root: Option<&ProjectRoot>,
    manifest: &engine::AssetManifest,
    game_module: Option<&Arc<engine::game_module::GameModule>>,
    gltf_cache: &engine::scene_bridge::SharedGltfImportCache,
    gpu_mesh_cache: &engine::SharedGpuMeshCache,
    size: [u32; 2],
    sky_enabled: bool,
) -> (
    engine::App,
    Option<PreviewNotice>,
    Option<engine::scene_bridge::AuthoringToRuntimeMap>,
) {
    let mut app = engine::App::new();
    app.insert_resource(engine::GpuMeshCache::with_shared(gpu_mesh_cache.clone()));

    if let Some(module) = game_module {
        // Project component schemas must be available during conversion even
        // though Edit Mode never registers or runs project gameplay systems.
        app.retain_game_module(Arc::clone(module));
    }

    if let Some(root) = project_root {
        app.insert_resource(engine::AssetServer::with_assets_root(root.assets_root()));
        app.insert_resource(manifest.clone());
        // Animation Set assignment invalidates the preview world. Reusing the
        // project-derived VMD bake keeps that rebuild from evaluating the
        // complete PMX/VMD motion again on the UI thread.
        app.insert_resource(engine::DerivedCache::new(root.path()));
    }

    // Reusing parsed glTF sources and decoded images across rebuilds keeps
    // large imports interactive (ADR 0071).
    app.insert_resource(gltf_cache.clone());

    app.insert_resource(engine::SkySettings {
        enabled: sky_enabled,
        ..engine::SkySettings::default()
    });
    app.world_mut().insert_resource(DebugLines::default());
    app.add_system(engine::collider_debug_draw_system);
    app.add_system(engine::nav_mesh_debug_draw_system);

    // Best-effort conversion (ADR 0068): an invalid component must not blank
    // the whole preview, so skips are surfaced as a notice instead.
    let (spawn_notice, bridge) = match engine::scene_bridge::spawn_from_authoring_scene_best_effort(
        app.world_mut(),
        scene,
    ) {
        Ok(map) => (skipped_component_notice(&map.asset_diagnostics), Some(map)),
        Err(error) => (
            Some(PreviewNotice::failure(
                "editor.scene_view.conversion_failed",
                format!("Scene View conversion failed: {error}"),
            )),
            None,
        ),
    };

    despawn_camera_entities(app.world_mut());

    if let Some(vp) = app.world_mut().get_resource_mut::<ViewportSize>() {
        vp.width = size[0];
        vp.height = size[1];
    }

    (app, spawn_notice, bridge)
}

/// Summarises best-effort conversion skips into one banner line.
fn skipped_component_notice(diagnostics: &[engine_authoring::Diagnostic]) -> Option<PreviewNotice> {
    let skipped: Vec<&engine_authoring::Diagnostic> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == engine::scene_bridge::COMPONENT_SKIPPED_DIAGNOSTIC)
        .collect();
    let first = skipped.first()?;
    let message = if skipped.len() == 1 {
        first.message.clone()
    } else {
        format!(
            "{} invalid components skipped; first: {}",
            skipped.len(),
            first.message
        )
    };
    Some(PreviewNotice::skipped_components(
        message,
        first.target.clone(),
    ))
}

fn simulate_particle_preview(world: &mut engine::ecs::World, elapsed_seconds: f32) {
    let mut emitters =
        engine::Query::<(&mut engine::ParticleEmitter, &GlobalTransform)>::new(world);
    for (_, (emitter, transform)) in emitters.iter_mut() {
        emitter.simulate_preview(elapsed_seconds, transform.matrix().col(3).truncate());
    }
}

/// Fixed interval used by animation-preview pose and secondary-physics updates.
///
/// This deliberately matches the runtime's 60 Hz interval. Rapier spring and
/// joint constraints must not receive render-frame-sized or fractional tail
/// steps because their correction changes with the integration delta.
const ANIMATION_PREVIEW_FIXED_STEP_SECONDS: f32 = 1.0 / 60.0;

/// Installs the editor-only subset of the runtime pose pipeline.
///
/// Gameplay gravity, velocity integration, collision, scripts, and character
/// motors are deliberately absent. The optional physics stage is exclusively
/// the isolated engine-native Secondary Motion system defined by ADR 0112.
fn install_animation_preview_systems(
    app: &mut engine::App,
    graph_enabled: bool,
    secondary_physics_enabled: bool,
) {
    ensure_animation_preview_resources(app);
    if secondary_physics_enabled
        && app
            .world()
            .get_resource::<engine::SecondaryMotionWorlds>()
            .is_none()
    {
        app.insert_resource(engine::SecondaryMotionWorlds::default());
    }

    app.add_fixed_system(engine::rig_pose_clear_transient_system);
    if graph_enabled {
        app.add_fixed_system(engine::anim_graph_system);
    }
    app.add_fixed_system(engine::animation_system);
    app.add_fixed_system(engine::transform_propagation_system);
    app.add_fixed_system(engine::foot_ik_system);
    if secondary_physics_enabled {
        app.add_fixed_system(engine::secondary_motion_system);
    }
    app.add_fixed_system(engine::publish_final_rig_pose_system);
    app.add_fixed_system(engine::transform_propagation_system);
}

/// Samples a dedicated clip, transition, or graph request for one entity.
// The caller-owned clock, fixed-step remainder, transition latch, and graph
// installation state are independently updated preview concerns. Bundling
// them solely to satisfy this lint would obscure which values are mutated.
#[allow(clippy::too_many_arguments)]
fn sample_requested_animation_preview(
    app: &mut engine::App,
    target: engine::ecs::Entity,
    mode: &AnimationPreviewMode,
    elapsed_seconds: f32,
    sampled_elapsed: &mut f32,
    fixed_step_remainder: &mut f32,
    transition_started: &mut bool,
    graph_system_installed: bool,
) -> AnimationPreviewStatus {
    ensure_animation_preview_resources(app);
    let elapsed_seconds = elapsed_seconds.max(0.0);

    match mode {
        AnimationPreviewMode::Clip { clip } => {
            let restarting = *sampled_elapsed < 0.0 || elapsed_seconds < *sampled_elapsed;
            if restarting {
                reset_preview_animator_to_clip(app.world_mut(), target, clip);
                *sampled_elapsed = 0.0;
                *fixed_step_remainder = 0.0;
                *transition_started = false;
                // A zero-time pass samples and publishes the first pose. It is
                // performed only on restart so paused previews cannot drift.
                run_animation_preview_step(app, 0.0);
            }
            run_animation_preview_interval(
                app,
                elapsed_seconds - *sampled_elapsed,
                fixed_step_remainder,
            );
            *sampled_elapsed = elapsed_seconds;
        }
        AnimationPreviewMode::Transition {
            from_clip,
            to_clip,
            trigger_seconds,
            fade_duration,
        } => {
            let trigger_seconds = trigger_seconds.max(0.0);
            let restarting = *sampled_elapsed < 0.0 || elapsed_seconds < *sampled_elapsed;
            if restarting {
                reset_preview_animator_to_clip(app.world_mut(), target, from_clip);
                *sampled_elapsed = 0.0;
                *fixed_step_remainder = 0.0;
                *transition_started = false;
                if trigger_seconds <= f32::EPSILON {
                    select_preview_clip(app.world_mut(), target, to_clip, fade_duration.max(0.0));
                    *transition_started = true;
                }
                run_animation_preview_step(app, 0.0);
            }

            let mut cursor = *sampled_elapsed;
            if cursor < trigger_seconds {
                let source_end = elapsed_seconds.min(trigger_seconds);
                run_animation_preview_interval(
                    app,
                    source_end - cursor,
                    fixed_step_remainder,
                );
                cursor = source_end;
            }
            if elapsed_seconds >= trigger_seconds {
                if !*transition_started {
                    select_preview_clip(
                        app.world_mut(),
                        target,
                        to_clip,
                        fade_duration.max(0.0),
                    );
                    *transition_started = true;
                }
                run_animation_preview_interval(
                    app,
                    elapsed_seconds - cursor,
                    fixed_step_remainder,
                );
            }
            if elapsed_seconds < trigger_seconds {
                *transition_started = false;
            }
            *sampled_elapsed = elapsed_seconds;
        }
        AnimationPreviewMode::Graph { parameters } if graph_system_installed => {
            let restarting = *sampled_elapsed < 0.0 || elapsed_seconds < *sampled_elapsed;
            if restarting {
                if let Some(player) = app
                    .world_mut()
                    .get_component_mut::<engine::AnimGraphPlayer>(target)
                {
                    player.restart();
                }
                if let Some(animator) = app
                    .world_mut()
                    .get_component_mut::<engine::Animator>(target)
                {
                    animator.stop();
                    animator.play();
                }
                *sampled_elapsed = 0.0;
                *fixed_step_remainder = 0.0;
            }
            if let Some(player) = app
                .world_mut()
                .get_component_mut::<engine::AnimGraphPlayer>(target)
            {
                for (name, value) in parameters {
                    // Preview overrides never redeclare a parameter's authored type,
                    // so a type mismatch leaves the runtime value untouched.
                    let _ = player.set_bool_parameter(name.clone(), *value);
                }
            }
            let delta = (elapsed_seconds - *sampled_elapsed).max(0.0);
            if restarting {
                run_animation_preview_step(app, 0.0);
            }
            if delta > f32::EPSILON {
                run_animation_preview_interval(app, delta, fixed_step_remainder);
            }
            *sampled_elapsed = elapsed_seconds;
            *transition_started = false;
        }
        AnimationPreviewMode::Graph { .. } => {}
    }

    collect_animation_preview_status(app.world(), target, mode, elapsed_seconds)
}

/// Restores one target Animator and selects a named clip without a blend.
fn reset_preview_animator_to_clip(
    world: &mut engine::ecs::World,
    target: engine::ecs::Entity,
    clip: &str,
) {
    if let Some(animator) = world.get_component_mut::<engine::Animator>(target) {
        animator.stop();
    }
    select_preview_clip(world, target, clip, 0.0);
    if let Some(animator) = world.get_component_mut::<engine::Animator>(target) {
        animator.play();
    }
}

/// Selects a clip from the target graph player's resolved clip table.
fn select_preview_clip(
    world: &mut engine::ecs::World,
    target: engine::ecs::Entity,
    clip: &str,
    fade_duration: f32,
) -> bool {
    let handle = world
        .get_component::<engine::AnimGraphPlayer>(target)
        .and_then(|player| player.clip_handle(clip));
    let Some(handle) = handle else {
        return false;
    };
    let Some(animator) = world.get_component_mut::<engine::Animator>(target) else {
        return false;
    };
    animator.crossfade_to(handle, fade_duration);
    true
}

/// Runs one fixed animation sample using a caller-supplied preview delta.
fn run_animation_preview_step(app: &mut engine::App, delta_seconds: f32) {
    if let Some(time) = app.world_mut().get_resource_mut::<engine::FixedTime>() {
        time.fixed_delta = delta_seconds.max(0.0);
        time.begin_step();
    }
    let _ = app.ecs_mut().run_fixed_update();
}

/// Accumulates render time and advances only complete runtime-sized steps.
fn run_animation_preview_interval(
    app: &mut engine::App,
    delta_seconds: f32,
    remainder: &mut f32,
) {
    *remainder += delta_seconds.max(0.0);
    while *remainder + f32::EPSILON >= ANIMATION_PREVIEW_FIXED_STEP_SECONDS {
        run_animation_preview_step(app, ANIMATION_PREVIEW_FIXED_STEP_SECONDS);
        *remainder -= ANIMATION_PREVIEW_FIXED_STEP_SECONDS;
    }
    if remainder.abs() <= f32::EPSILON {
        *remainder = 0.0;
    }
}

/// Captures the small runtime state needed by the preview controls.
fn collect_animation_preview_status(
    world: &engine::ecs::World,
    target: engine::ecs::Entity,
    mode: &AnimationPreviewMode,
    elapsed_seconds: f32,
) -> AnimationPreviewStatus {
    let animator = world.get_component::<engine::Animator>(target);
    let graph = world.get_component::<engine::AnimGraphPlayer>(target);
    let runtime_issue = if animator.is_none() {
        Some("The preview target has no runtime Animator.".to_owned())
    } else if matches!(mode, AnimationPreviewMode::Graph { .. }) && graph.is_none() {
        Some("The preview target has no runtime Animation Graph Player.".to_owned())
    } else if animator.is_some_and(|animator| {
        world
            .get_resource::<engine::Assets<engine::AnimationClip>>()
            .is_none_or(|clips| clips.get(&animator.clip).is_none())
    }) {
        Some("The preview target's active animation clip is not loaded.".to_owned())
    } else {
        None
    };
    let active_clip = match mode {
        AnimationPreviewMode::Clip { clip } => Some(clip.clone()),
        AnimationPreviewMode::Transition {
            from_clip,
            to_clip,
            trigger_seconds,
            ..
        } => Some(if elapsed_seconds < *trigger_seconds {
            from_clip.clone()
        } else {
            to_clip.clone()
        }),
        AnimationPreviewMode::Graph { .. } => graph
            .and_then(engine::AnimGraphPlayer::current_state_info)
            .and_then(|state| state.motion_key().map(str::to_owned)),
    };
    let last_transition = graph
        .and_then(engine::AnimGraphPlayer::last_transition)
        .map(|transition| {
            format!(
                "{} -> {}",
                transition.from_node.as_str(),
                transition.to_node.as_str()
            )
        });
    AnimationPreviewStatus {
        active_clip,
        clip_time: animator.map_or(0.0, |animator| animator.time),
        crossfade_progress: animator.and_then(engine::Animator::crossfade_progress),
        last_transition,
        runtime_issue,
    }
}

/// Ensures resources consumed by the animation fixed-step system exist.
fn ensure_animation_preview_resources(app: &mut engine::App) {
    if app
        .world()
        .get_resource::<engine::Assets<engine::AnimationClip>>()
        .is_none()
    {
        app.insert_resource(engine::Assets::<engine::AnimationClip>::default());
    }
    if app
        .world()
        .get_resource::<engine::AnimationEvents>()
        .is_none()
    {
        app.insert_resource(engine::AnimationEvents::default());
    }
}

/// Advances authored clips to one deterministic preview time.
///
/// A fresh preview world starts every animator once at time zero. Subsequent
/// frames advance only the unsampled interval through bounded substeps, which
/// preserves stateful secondary physics while remaining deterministic for a
/// given restart and playback sequence. Backward seeks, Restart, and preview
/// toggles rebuild the transient world before replaying from zero (ADR 0072).
///
/// The caller must install the preview pose pipeline exactly once for the
/// world; this function never registers systems because duplicate registration
/// would advance animation and physics by a multiple of the intended delta.
fn sample_animation_preview(
    app: &mut engine::App,
    elapsed_seconds: f32,
    sampled_elapsed: &mut f32,
    fixed_step_remainder: &mut f32,
) {
    let has_animator = {
        let query = engine::Query::<&engine::Animator>::new(app.world_mut());
        query.iter().next().is_some()
    };
    if !has_animator {
        return;
    }
    ensure_animation_preview_resources(app);
    let elapsed_seconds = elapsed_seconds.max(0.0);
    let restarting = *sampled_elapsed < 0.0 || elapsed_seconds < *sampled_elapsed;
    if restarting {
        let mut animators = engine::Query::<&mut engine::Animator>::new(app.world_mut());
        for (_, animator) in animators.iter_mut() {
            animator.stop();
            animator.play();
        }
        *sampled_elapsed = 0.0;
        *fixed_step_remainder = 0.0;
        run_animation_preview_step(app, 0.0);
    }
    run_animation_preview_interval(
        app,
        elapsed_seconds - *sampled_elapsed,
        fixed_step_remainder,
    );
    *sampled_elapsed = elapsed_seconds;
}

/// Refreshes scene-placed UI documents that match the document open in UI
/// Builder with its live in-memory version.
///
/// A placed `engine.ui_document` is loaded from disk when the preview world is
/// built, so an unsaved Builder edit would otherwise appear only after the world
/// is next rebuilt (for example when another entity is edited). Overwriting the
/// matching [`engine::UiDocumentRef`] each frame keeps the overlay in step with
/// authoring without a rebuild, mirroring the transform fast path (ADR 0072).
/// Only the open asset is refreshed; every other placed document keeps the value
/// it loaded. The document is cloned only when it actually differs, so idle
/// frames do no work.
fn apply_live_ui_document(world: &mut engine::ecs::World, open: Option<(&AssetId, &UiDocument)>) {
    let Some((asset, document)) = open else {
        return;
    };
    let entities: Vec<_> = world.entities().collect();
    for entity in entities {
        let stale = world
            .get_component::<engine::UiDocumentRef>(entity)
            .is_some_and(|placed| &placed.asset == asset && placed.document != *document);
        if stale
            && let Some(placed) = world.get_component_mut::<engine::UiDocumentRef>(entity) {
                placed.document = document.clone();
            }
    }
}

/// Hashes the manifest content that affects a built preview world (ADR 0072).
///
/// The Scene View compares this across frames to decide whether asset
/// registration or reimport invalidated its persistent world. Only fields the
/// preview resolves are hashed (path, display name, and the sub-asset identity
/// a reimport changes), so the cost is proportional to the number of entries,
/// not to asset data.
fn manifest_content_hash(manifest: &engine::AssetManifest) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (id, entry) in manifest.iter() {
        id.as_str().hash(&mut hasher);
        entry.path.hash(&mut hasher);
        entry.name.hash(&mut hasher);
        for dependency in &entry.import_settings.source_dependencies {
            dependency.hash(&mut hasher);
        }
        for sub_asset in &entry.import_settings.sub_assets {
            sub_asset.id.hash(&mut hasher);
            sub_asset.index.hash(&mut hasher);
        }
        entry.import_settings.material_remaps.hash(&mut hasher);
        entry.import_settings.texture_remaps.hash(&mut hasher);
    }
    hasher.finish()
}

/// Returns the authoring entities whose transform is being previewed this
/// frame: the gizmo target and any Inspector transform drag (ADR 0072).
fn current_transform_override_ids(
    component_preview: Option<&SceneComponentPreview>,
    gizmo_active: bool,
    selected_entity: Option<&EntityId>,
) -> Vec<EntityId> {
    let mut ids = Vec::new();
    if let Some(preview) = component_preview {
        // In the reuse path a present preview is always transform-typed; a
        // non-transform preview forces a rebuild instead.
        ids.push(preview.entity.clone());
    }
    if gizmo_active
        && let Some(id) = selected_entity
            && !ids.contains(id) {
                ids.push(id.clone());
            }
    ids
}

/// Writes the render-scene transform of every currently or previously
/// previewed entity into the reused preview world (ADR 0072).
///
/// Rewriting last frame's entities from `render_scene` restores a cancelled
/// gesture to the committed pose without a rebuild, because once the gesture
/// ends `render_scene` no longer carries the transient override. Local
/// transforms are written; global transforms are recomputed by the world's
/// propagation during update.
fn apply_transform_overrides(
    app: &mut engine::App,
    bridge: Option<&engine::scene_bridge::AuthoringToRuntimeMap>,
    render_scene: &AuthoringScene,
    previous: &mut Vec<EntityId>,
    current: Vec<EntityId>,
) {
    let Some(bridge) = bridge else {
        previous.clear();
        return;
    };
    let tx_type = engine_authoring::id::ComponentTypeId::new("engine.transform");
    for id in previous.iter().chain(current.iter()) {
        let Some(entity) = bridge.get(id) else {
            continue;
        };
        let Some(source) = render_scene.entity(id) else {
            continue;
        };
        let transform = authoring_local_transform(source, &tx_type);
        if let Some(existing) = app.world_mut().get_component_mut::<Transform>(entity) {
            *existing = transform;
        }
    }
    *previous = current;
}

/// Builds an engine [`Transform`] from an authoring entity's transform
/// component, mirroring [`local_transform_matrix`] but preserving the
/// translation/rotation/scale split the runtime component stores.
fn authoring_local_transform(
    entity: &engine_authoring::AuthoringEntity,
    tx_type: &engine_authoring::id::ComponentTypeId,
) -> Transform {
    use engine_authoring::Value;
    let Some(Value::Object(fields)) = entity.components.get(tx_type) else {
        return Transform::default();
    };
    let translation = Vec3::new(
        obj_f64(fields, "x") as f32,
        obj_f64(fields, "y") as f32,
        obj_f64(fields, "z") as f32,
    );
    let rotation = Quat::from_euler(
        EulerRot::XYZ,
        (obj_f64(fields, "rotation_x_degrees") as f32).to_radians(),
        (obj_f64(fields, "rotation_y_degrees") as f32).to_radians(),
        (obj_f64(fields, "rotation_z_degrees") as f32).to_radians(),
    );
    let scale = Vec3::new(
        obj_f64_or(fields, "scale_x", 1.0) as f32,
        obj_f64_or(fields, "scale_y", 1.0) as f32,
        obj_f64_or(fields, "scale_z", 1.0) as f32,
    );
    Transform {
        translation,
        rotation,
        scale,
    }
}

struct ParticleDebugSnapshot {
    live: usize,
    maximum: usize,
    spawn_rate: f32,
    bounds: Option<(Vec3, Vec3)>,
    origin: Vec3,
    direction: Vec3,
}

fn selected_particle_debug(
    world: &mut engine::ecs::World,
    selected: &EntityId,
) -> Option<ParticleDebugSnapshot> {
    let query = engine::Query::<(
        &engine::RuntimeEntityIdentity,
        &engine::ParticleEmitter,
        &GlobalTransform,
    )>::new(world);
    query
        .iter()
        .find_map(|(_, (identity, emitter, transform))| {
            (&identity.authoring_id == selected).then(|| ParticleDebugSnapshot {
                live: emitter.live_count(),
                maximum: emitter.max_particles,
                spawn_rate: emitter.spawn_rate,
                bounds: emitter.live_bounds(),
                origin: transform.matrix().col(3).truncate(),
                direction: emitter.direction,
            })
        })
}

fn despawn_camera_entities(world: &mut engine::ecs::World) {
    let camera_entities: Vec<engine::ecs::Entity> = {
        let q = engine::Query::<&Camera3D>::new(world);
        q.iter().map(|(e, _)| e).collect()
    };
    for e in camera_entities {
        let _ = world.despawn(e);
    }
}

fn update_editor_camera(world: &mut engine::ecs::World, cam_transform: Transform, aspect: f32) {
    if let Ok(e) = world.spawn() {
        let mat = cam_transform.to_matrix();
        let _ = world.add_component(e, cam_transform);
        let _ = world.add_component(e, GlobalTransform(mat));
        let _ = world.add_component(e, Camera3D::new(60.0, aspect, 0.1, 1000.0));
    }
}

fn collect_entity_positions(scene: &AuthoringScene) -> Vec<EntityPickInfo> {
    use engine_authoring::id::ComponentTypeId;
    let tx_type = ComponentTypeId::new("engine.transform");
    let mut world_memo = std::collections::BTreeMap::new();
    scene
        .entities()
        .map(|(id, entity)| {
            let world = resolve_world_matrix(id, scene, &tx_type, &mut world_memo, 0);
            let center = world.transform_point3(Vec3::ZERO);
            let world_scale = Vec3::new(
                world.x_axis.truncate().length(),
                world.y_axis.truncate().length(),
                world.z_axis.truncate().length(),
            );
            let has_mesh = [
                engine::scene_bridge::STATIC_MESH_RENDERER_COMPONENT,
                engine::scene_bridge::SKINNED_MESH_RENDERER_COMPONENT,
                engine::scene_bridge::LOD_GROUP_COMPONENT,
            ]
            .iter()
            .any(|name| entity.components.contains_key(&ComponentTypeId::new(*name)));
            // Meshes spawn as unit-sized primitives, so a scaled unit cube
            // tracks the visible size far better than the previous fixed
            // half-extent of 0.5 that made large floors unclickable at their
            // edges. Meshless entities keep a small icon-sized volume.
            let half = if has_mesh {
                (world_scale * 0.5).max(Vec3::splat(0.05))
            } else {
                Vec3::splat(0.35)
            };
            EntityPickInfo {
                id: id.clone(),
                center,
                half,
                icon: entity_icon(entity),
                world,
            }
        })
        .collect()
}

/// Resolves an entity's parent chain into one world matrix.
fn resolve_world_matrix(
    id: &EntityId,
    scene: &AuthoringScene,
    tx_type: &engine_authoring::id::ComponentTypeId,
    memo: &mut std::collections::BTreeMap<EntityId, Mat4>,
    depth: usize,
) -> Mat4 {
    if let Some(matrix) = memo.get(id) {
        return *matrix;
    }
    // Parent cycles are authoring errors surfaced elsewhere; the guard only
    // keeps the editor responsive if one slips through.
    if depth > 64 {
        return Mat4::IDENTITY;
    }
    let Some(entity) = scene.entity(id) else {
        return Mat4::IDENTITY;
    };
    let local = local_transform_matrix(entity, tx_type);
    let world = match &entity.parent {
        Some(parent) => resolve_world_matrix(parent, scene, tx_type, memo, depth + 1) * local,
        None => local,
    };
    memo.insert(id.clone(), world);
    world
}

fn local_transform_matrix(
    entity: &engine_authoring::AuthoringEntity,
    tx_type: &engine_authoring::id::ComponentTypeId,
) -> Mat4 {
    use engine_authoring::Value;
    let Some(Value::Object(fields)) = entity.components.get(tx_type) else {
        return Mat4::IDENTITY;
    };
    let translation = Vec3::new(
        obj_f64(fields, "x") as f32,
        obj_f64(fields, "y") as f32,
        obj_f64(fields, "z") as f32,
    );
    let rotation = Quat::from_euler(
        EulerRot::XYZ,
        (obj_f64(fields, "rotation_x_degrees") as f32).to_radians(),
        (obj_f64(fields, "rotation_y_degrees") as f32).to_radians(),
        (obj_f64(fields, "rotation_z_degrees") as f32).to_radians(),
    );
    let scale = Vec3::new(
        obj_f64_or(fields, "scale_x", 1.0) as f32,
        obj_f64_or(fields, "scale_y", 1.0) as f32,
        obj_f64_or(fields, "scale_z", 1.0) as f32,
    );
    Mat4::from_scale_rotation_translation(scale, rotation, translation)
}

fn entity_icon(entity: &engine_authoring::AuthoringEntity) -> Option<EntityIcon> {
    use engine_authoring::id::ComponentTypeId;
    let has = |name: &str| entity.components.contains_key(&ComponentTypeId::new(name));
    if has(engine::scene_bridge::CAMERA_COMPONENT) {
        Some(EntityIcon::Camera)
    } else if has(engine::scene_bridge::DIRECTIONAL_LIGHT_COMPONENT)
        || has(engine::scene_bridge::AMBIENT_LIGHT_COMPONENT)
    {
        Some(EntityIcon::Light)
    } else if has(engine::scene_bridge::AUDIO_EMITTER_COMPONENT)
        || has(engine::scene_bridge::AUDIO_LISTENER_COMPONENT)
    {
        Some(EntityIcon::Audio)
    } else if has(engine::scene_bridge::PARTICLE_EMITTER_COMPONENT) {
        Some(EntityIcon::Particle)
    } else {
        None
    }
}

fn obj_f64(fields: &std::collections::BTreeMap<String, engine_authoring::Value>, key: &str) -> f64 {
    obj_f64_or(fields, key, 0.0)
}

fn obj_f64_or(
    fields: &std::collections::BTreeMap<String, engine_authoring::Value>,
    key: &str,
    default: f64,
) -> f64 {
    use engine_authoring::Value;
    match fields.get(key) {
        Some(Value::F64(f)) => *f,
        Some(Value::I64(i)) => *i as f64,
        _ => default,
    }
}

fn lod_distances(scene: &AuthoringScene, entity: &EntityId) -> Vec<f32> {
    use engine_authoring::{ComponentTypeId, Value};
    scene
        .entity(entity)
        .and_then(|entity| {
            entity.components.get(&ComponentTypeId::new(
                engine::scene_bridge::LOD_GROUP_COMPONENT,
            ))
        })
        .and_then(|value| match value {
            Value::Object(fields) => fields.get("levels"),
            _ => None,
        })
        .and_then(|value| match value {
            Value::Array(levels) => Some(levels),
            _ => None,
        })
        .map(|levels| {
            levels
                .iter()
                .filter_map(|level| match level {
                    Value::Object(fields) => fields.get("distance"),
                    _ => None,
                })
                .filter_map(|distance| match distance {
                    Value::F64(distance) => Some(*distance as f32),
                    Value::I64(distance) => Some(*distance as f32),
                    Value::U64(distance) => Some(*distance as f32),
                    _ => None,
                })
                .filter(|distance| distance.is_finite() && *distance > 0.0)
                .collect()
        })
        .unwrap_or_default()
}

fn draw_distance_ring(lines: &mut DebugLines, center: Vec3, radius: f32) {
    const STEPS: usize = 64;
    for index in 0..STEPS {
        let a = index as f32 / STEPS as f32 * std::f32::consts::TAU;
        let b = (index + 1) as f32 / STEPS as f32 * std::f32::consts::TAU;
        lines.line(
            center + Vec3::new(a.cos() * radius, 0.0, a.sin() * radius),
            center + Vec3::new(b.cos() * radius, 0.0, b.sin() * radius),
            Vec3::new(1.0, 0.75, 0.1),
        );
    }
}

fn audio_emitter_distances(scene: &AuthoringScene, entity: &EntityId) -> Option<(f32, f32)> {
    let component = ComponentTypeId::new(engine::scene_bridge::AUDIO_EMITTER_COMPONENT);
    let Value::Object(fields) = scene.entity(entity)?.components.get(&component)? else {
        return None;
    };
    let min_distance = obj_f64_or(fields, "min_distance", 1.0) as f32;
    let max_distance = obj_f64_or(fields, "max_distance", 20.0) as f32;
    (min_distance.is_finite() && max_distance.is_finite())
        .then_some((min_distance.max(0.0), max_distance.max(min_distance).max(0.0)))
}

fn audio_emitter_display_distances(
    scene: &AuthoringScene,
    entity: &EntityId,
    drag: Option<&AudioDistanceDragState>,
) -> Option<(f32, f32)> {
    let (mut min_distance, mut max_distance) = audio_emitter_distances(scene, entity)?;
    if let Some(drag) = drag.filter(|drag| &drag.entity == entity) {
        match drag.field {
            AudioDistanceField::Min => min_distance = drag.effective_distance,
            AudioDistanceField::Max => max_distance = drag.effective_distance,
        }
    }
    Some((min_distance, max_distance))
}

fn has_audio_listener(scene: &AuthoringScene, entity: &EntityId) -> bool {
    scene.entity(entity).is_some_and(|item| {
        item.components.contains_key(&ComponentTypeId::new(
            engine::scene_bridge::AUDIO_LISTENER_COMPONENT,
        ))
    })
}

fn audio_distance_handle_direction(camera: &EditorViewCamera) -> Vec3 {
    let right = (camera.to_transform().rotation * Vec3::X).normalize_or_zero();
    if right == Vec3::ZERO { Vec3::X } else { right }
}

fn clamp_audio_distance(
    field: AudioDistanceField,
    candidate: f32,
    min_distance: f32,
    max_distance: f32,
) -> f32 {
    let candidate = if candidate.is_finite() { candidate.max(0.0) } else { 0.0 };
    match field {
        AudioDistanceField::Min => candidate.min(max_distance.max(0.0)),
        AudioDistanceField::Max => candidate.max(min_distance.max(0.0)),
    }
}

fn hit_test_audio_distance_handle(
    pointer: egui::Pos2,
    center: Vec3,
    min_distance: f32,
    max_distance: f32,
    direction: Vec3,
    vp: Mat4,
    rect: egui::Rect,
) -> Option<AudioDistanceField> {
    const HANDLE_RADIUS: f32 = 12.0;
    [(AudioDistanceField::Min, min_distance), (AudioDistanceField::Max, max_distance)]
        .into_iter()
        .map(|(field, distance)| {
            let tip = world_to_screen(center + direction * distance, vp, rect);
            ((pointer - tip).length(), field)
        })
        .filter(|(distance, _)| *distance <= HANDLE_RADIUS)
        .min_by(|(left, _), (right, _)| left.total_cmp(right))
        .map(|(_, field)| field)
}

fn draw_audio_distance_shell(lines: &mut DebugLines, center: Vec3, radius: f32, color: Vec3) {
    if !radius.is_finite() || radius <= 0.0 {
        return;
    }
    draw_rotation_ring(lines, center, [1.0, 0.0, 0.0], color, radius);
    draw_rotation_ring(lines, center, [0.0, 1.0, 0.0], color, radius);
    draw_rotation_ring(lines, center, [0.0, 0.0, 1.0], color, radius);
}

fn draw_audio_listener_orientation(lines: &mut DebugLines, info: &EntityPickInfo) {
    let right = info.world.x_axis.truncate().normalize_or_zero();
    let forward = -info.world.z_axis.truncate().normalize_or_zero();
    let right = if right == Vec3::ZERO { Vec3::X } else { right };
    let forward = if forward == Vec3::ZERO { Vec3::NEG_Z } else { forward };
    let color = Vec3::new(0.35, 0.9, 1.0);
    lines.line(info.center - right * 0.55, info.center + right * 0.55, color);
    let tip = info.center + forward * 0.9;
    lines.line(info.center, tip, color);
    draw_arrow_tip(lines, tip, forward, 0.18, color);
}

fn draw_audio_distance_handles(
    painter: &egui::Painter,
    center: Vec3,
    direction: Vec3,
    min_distance: f32,
    max_distance: f32,
    vp: Mat4,
    rect: egui::Rect,
    active: Option<AudioDistanceField>,
) {
    for (field, distance, label, color) in [
        (AudioDistanceField::Min, min_distance, "min", egui::Color32::from_rgb(90, 220, 255)),
        (AudioDistanceField::Max, max_distance, "max", egui::Color32::from_rgb(70, 145, 255)),
    ] {
        let tip = world_to_screen(center + direction * distance, vp, rect);
        let radius = if active == Some(field) { 7.0 } else { 5.0 };
        painter.circle_filled(tip, radius, color);
        painter.text(
            tip + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{label} {distance:.2} m"),
            egui::FontId::monospace(11.0),
            color,
        );
    }
}

// ---------------------------------------------------------------------------
// Debug line helpers
// ---------------------------------------------------------------------------

fn draw_grid(dl: &mut DebugLines) {
    let dim = 10i32;
    for i in -dim..=dim {
        let f = i as f32;
        let (cx, cz) = if i == 0 {
            (Vec3::new(0.8, 0.1, 0.1), Vec3::new(0.1, 0.1, 0.8))
        } else {
            (Vec3::splat(0.25), Vec3::splat(0.25))
        };
        dl.line(
            Vec3::new(f, 0.0, -(dim as f32)),
            Vec3::new(f, 0.0, dim as f32),
            cx,
        );
        dl.line(
            Vec3::new(-(dim as f32), 0.0, f),
            Vec3::new(dim as f32, 0.0, f),
            cz,
        );
    }
    dl.line(Vec3::ZERO, Vec3::X * 2.0, Vec3::new(1.0, 0.0, 0.0));
    dl.line(Vec3::ZERO, Vec3::Y * 2.0, Vec3::new(0.0, 1.0, 0.0));
    dl.line(Vec3::ZERO, Vec3::Z * 2.0, Vec3::new(0.0, 0.0, 1.0));
}

fn draw_gizmo(
    dl: &mut DebugLines,
    center: Vec3,
    mode: GizmoMode,
    len: f32,
    hovered: Option<GizmoAxis>,
    dirs: &[(GizmoAxis, Vec3); 3],
) {
    const HOVER_COLOR: Vec3 = Vec3::new(1.0, 0.9, 0.2);
    let axis_color = |axis: GizmoAxis, base: Vec3| {
        if hovered == Some(axis) {
            HOVER_COLOR
        } else {
            base
        }
    };
    let colors = [
        axis_color(GizmoAxis::X, Vec3::new(1.0, 0.15, 0.15)),
        axis_color(GizmoAxis::Y, Vec3::new(0.15, 1.0, 0.15)),
        axis_color(GizmoAxis::Z, Vec3::new(0.15, 0.15, 1.0)),
    ];
    match mode {
        GizmoMode::Translate | GizmoMode::Scale => {
            for (index, (_, dir)) in dirs.iter().enumerate() {
                let color = colors[index];
                let tip = center + *dir * len;
                dl.line(center, tip, color);
                if mode == GizmoMode::Translate {
                    draw_arrow_tip(dl, tip, *dir, len * 0.14, color);
                } else {
                    draw_cube_tip(dl, tip, len * 0.07, color);
                }
            }
        }
        GizmoMode::Rotate => {
            draw_rotation_ring(dl, center, [1.0, 0.0, 0.0], colors[0], len);
            draw_rotation_ring(dl, center, [0.0, 1.0, 0.0], colors[1], len);
            draw_rotation_ring(dl, center, [0.0, 0.0, 1.0], colors[2], len);
        }
    }
}

/// Wire icon for entities with no renderable mesh so they can be seen and
/// aimed at in the viewport.
fn draw_entity_icon(dl: &mut DebugLines, info: &EntityPickInfo, icon: EntityIcon) {
    let center = info.center;
    match icon {
        EntityIcon::Camera => {
            let size = 0.22;
            dl.aabb(center, Vec3::splat(size), Vec3::new(0.85, 0.85, 0.95));
            // Small lens frustum along the camera's forward (-Z) axis.
            let forward = -info.world.z_axis.truncate().normalize_or_zero();
            let up = info.world.y_axis.truncate().normalize_or_zero() * size;
            let right = info.world.x_axis.truncate().normalize_or_zero() * size;
            let front = center + forward * size;
            let lens = center + forward * (size * 3.0);
            for corner in [
                lens + up + right,
                lens + up - right,
                lens - up + right,
                lens - up - right,
            ] {
                dl.line(front, corner, Vec3::new(0.85, 0.85, 0.95));
            }
        }
        EntityIcon::Light => {
            let color = Vec3::new(1.0, 0.9, 0.3);
            let r = 0.35;
            for (a, b) in [
                (Vec3::X, Vec3::NEG_X),
                (Vec3::Y, Vec3::NEG_Y),
                (Vec3::Z, Vec3::NEG_Z),
                (Vec3::new(0.7, 0.7, 0.0), Vec3::new(-0.7, -0.7, 0.0)),
                (Vec3::new(0.0, 0.7, 0.7), Vec3::new(0.0, -0.7, -0.7)),
            ] {
                dl.line(center + a * r, center + b * r, color);
            }
        }
        EntityIcon::Audio => {
            let color = Vec3::new(0.4, 0.85, 1.0);
            draw_rotation_ring(dl, center, [0.0, 1.0, 0.0], color, 0.25);
            draw_rotation_ring(dl, center, [0.0, 1.0, 0.0], color, 0.4);
            dl.line(center, center + Vec3::Y * 0.35, color);
        }
        EntityIcon::Particle => {
            let color = Vec3::new(1.0, 0.55, 0.2);
            for direction in [
                Vec3::new(0.0, 1.0, 0.0),
                Vec3::new(0.5, 0.9, 0.0),
                Vec3::new(-0.5, 0.9, 0.0),
                Vec3::new(0.0, 0.9, 0.5),
                Vec3::new(0.0, 0.9, -0.5),
            ] {
                dl.line(center, center + direction.normalize_or_zero() * 0.4, color);
            }
        }
    }
}

/// View frustum for the selected camera entity so framing can be judged
/// without pressing Play.
fn draw_camera_frustum(dl: &mut DebugLines, info: &EntityPickInfo, scene: &AuthoringScene) {
    use engine_authoring::id::ComponentTypeId;
    use engine_authoring::Value;
    let camera_type = ComponentTypeId::new(engine::scene_bridge::CAMERA_COMPONENT);
    let fields = scene
        .entity(&info.id)
        .and_then(|entity| entity.components.get(&camera_type));
    let (fov_degrees, near, far) = match fields {
        Some(Value::Object(fields)) => (
            obj_f64_or(fields, "fov_y_degrees", 60.0) as f32,
            obj_f64_or(fields, "near", 0.1).max(0.01) as f32,
            obj_f64_or(fields, "far", 1000.0) as f32,
        ),
        _ => (60.0, 0.1, 1000.0),
    };
    // The far plane is clamped for display; a kilometer of frustum lines
    // would drown the scene.
    let far = far.min(20.0).max(near + 0.1);
    let aspect = 16.0 / 9.0;
    let tan_half = (fov_degrees.to_radians() * 0.5).tan();
    let color = Vec3::new(0.85, 0.85, 0.95);
    let corners = |distance: f32| -> [Vec3; 4] {
        let half_h = tan_half * distance;
        let half_w = half_h * aspect;
        let forward = -info.world.z_axis.truncate().normalize_or_zero();
        let up = info.world.y_axis.truncate().normalize_or_zero();
        let right = info.world.x_axis.truncate().normalize_or_zero();
        let base = info.center + forward * distance;
        [
            base + up * half_h + right * half_w,
            base + up * half_h - right * half_w,
            base - up * half_h - right * half_w,
            base - up * half_h + right * half_w,
        ]
    };
    let near_corners = corners(near);
    let far_corners = corners(far);
    for index in 0..4 {
        dl.line(near_corners[index], near_corners[(index + 1) % 4], color);
        dl.line(far_corners[index], far_corners[(index + 1) % 4], color);
        dl.line(near_corners[index], far_corners[index], color);
    }
}

/// Arrowhead marking where a translate handle can be grabbed.
fn draw_arrow_tip(dl: &mut DebugLines, tip: Vec3, dir: Vec3, size: f32, color: Vec3) {
    let (side_a, side_b) = perpendicular_pair(dir);
    let back = tip - dir * size;
    dl.line(tip, back + side_a * size * 0.5, color);
    dl.line(tip, back - side_a * size * 0.5, color);
    dl.line(tip, back + side_b * size * 0.5, color);
    dl.line(tip, back - side_b * size * 0.5, color);
}

/// Small cross marking a scale handle tip.
fn draw_cube_tip(dl: &mut DebugLines, tip: Vec3, size: f32, color: Vec3) {
    dl.line(tip - Vec3::X * size, tip + Vec3::X * size, color);
    dl.line(tip - Vec3::Y * size, tip + Vec3::Y * size, color);
    dl.line(tip - Vec3::Z * size, tip + Vec3::Z * size, color);
}

fn perpendicular_pair(dir: Vec3) -> (Vec3, Vec3) {
    let reference = if dir.x.abs() > 0.5 { Vec3::Y } else { Vec3::X };
    let side_a = dir.cross(reference).normalize_or_zero();
    let side_b = dir.cross(side_a).normalize_or_zero();
    (side_a, side_b)
}

fn draw_rotation_ring(dl: &mut DebugLines, center: Vec3, axis: [f32; 3], color: Vec3, r: f32) {
    const STEPS: usize = 32;
    for i in 0..STEPS {
        let a = (i as f32 / STEPS as f32) * std::f32::consts::TAU;
        let b = ((i + 1) as f32 / STEPS as f32) * std::f32::consts::TAU;
        let pa = center + ring_pt(axis, a, r);
        let pb = center + ring_pt(axis, b, r);
        dl.line(pa, pb, color);
    }
}

fn ring_pt(axis: [f32; 3], angle: f32, r: f32) -> Vec3 {
    let (sin, cos) = angle.sin_cos();
    if axis[0] > 0.5 {
        Vec3::new(0.0, cos * r, sin * r)
    } else if axis[1] > 0.5 {
        Vec3::new(cos * r, 0.0, sin * r)
    } else {
        Vec3::new(cos * r, sin * r, 0.0)
    }
}

// ---------------------------------------------------------------------------
// Gizmo screen-space helpers
// ---------------------------------------------------------------------------

fn world_to_screen(world_pos: Vec3, vp: Mat4, rect: egui::Rect) -> egui::Pos2 {
    let clip = vp * Vec4::new(world_pos.x, world_pos.y, world_pos.z, 1.0);
    if clip.w.abs() < 1e-6 {
        return rect.center();
    }
    let ndc = clip / clip.w;
    let sx = (ndc.x * 0.5 + 0.5) * rect.width() + rect.min.x;
    let sy = (1.0 - (ndc.y * 0.5 + 0.5)) * rect.height() + rect.min.y;
    egui::pos2(sx, sy)
}

/// World-space handle length that keeps the gizmo roughly a constant size
/// on screen regardless of camera distance, so handles stay grabbable both
/// close up and far away.
fn gizmo_axis_length(center: Vec3, vp: Mat4, rect: egui::Rect) -> f32 {
    const TARGET_PX: f32 = 85.0;
    let origin = world_to_screen(center, vp, rect);
    let pixels_per_unit = [Vec3::X, Vec3::Y, Vec3::Z]
        .into_iter()
        .map(|dir| (world_to_screen(center + dir, vp, rect) - origin).length())
        .fold(0.0_f32, f32::max);
    if pixels_per_unit <= f32::EPSILON {
        return 1.5;
    }
    (TARGET_PX / pixels_per_unit).clamp(0.05, 500.0)
}

fn point_segment_distance(point: egui::Pos2, start: egui::Pos2, end: egui::Pos2) -> f32 {
    let segment = end - start;
    let length_sq = segment.length_sq();
    if length_sq < 1e-6 {
        return (point - start).length();
    }
    let t = ((point - start).dot(segment) / length_sq).clamp(0.0, 1.0);
    (point - (start + segment * t)).length()
}

/// Snap step used while Ctrl-dragging a handle; Shift+Ctrl selects the fine
/// translate step.
fn snap_increment(mode: GizmoMode, fine: bool) -> f32 {
    match mode {
        GizmoMode::Translate => {
            if fine {
                0.1
            } else {
                1.0
            }
        }
        GizmoMode::Rotate => 15.0,
        GizmoMode::Scale => 0.1,
    }
}

const GIZMO_AXES: [(GizmoAxis, Vec3); 3] = [
    (GizmoAxis::X, Vec3::X),
    (GizmoAxis::Y, Vec3::Y),
    (GizmoAxis::Z, Vec3::Z),
];

/// Handle directions for the current gizmo space.
///
/// Falls back to world axes whenever the local basis is unavailable or
/// degenerate (zero scale), so handles never disappear.
fn gizmo_axis_directions(
    space: GizmoSpace,
    mode: GizmoMode,
    selected: Option<&EntityId>,
    pick_info: &[EntityPickInfo],
) -> [(GizmoAxis, Vec3); 3] {
    if space == GizmoSpace::Local && mode != GizmoMode::Rotate
        && let Some(info) = selected.and_then(|sel| pick_info.iter().find(|e| &e.id == sel)) {
            let x = info.world.x_axis.truncate().normalize_or_zero();
            let y = info.world.y_axis.truncate().normalize_or_zero();
            let z = info.world.z_axis.truncate().normalize_or_zero();
            if x != Vec3::ZERO && y != Vec3::ZERO && z != Vec3::ZERO {
                return [(GizmoAxis::X, x), (GizmoAxis::Y, y), (GizmoAxis::Z, z)];
            }
        }
    GIZMO_AXES
}

fn axis_direction_of(dirs: &[(GizmoAxis, Vec3); 3], axis: GizmoAxis) -> Vec3 {
    dirs.iter()
        .find(|(candidate, _)| *candidate == axis)
        .map(|(_, dir)| *dir)
        .unwrap_or(match axis {
            GizmoAxis::X => Vec3::X,
            GizmoAxis::Y => Vec3::Y,
            GizmoAxis::Z => Vec3::Z,
        })
}

/// Hit test covering the whole handle geometry, not only the tip.
///
/// Tips win over shafts so overlapping axes stay selectable; rotate mode
/// tests the rings that are actually drawn.
fn hit_test_gizmo_axis(
    screen_pos: egui::Pos2,
    center: Vec3,
    vp: Mat4,
    rect: egui::Rect,
    mode: GizmoMode,
    len: f32,
    dirs: &[(GizmoAxis, Vec3); 3],
) -> Option<GizmoAxis> {
    const TIP_RADIUS: f32 = 14.0;
    const LINE_RADIUS: f32 = 9.0;
    if mode == GizmoMode::Rotate {
        const RING_STEPS: usize = 32;
        let mut best: Option<(f32, GizmoAxis)> = None;
        for (axis, axis_array) in [
            (GizmoAxis::X, [1.0, 0.0, 0.0]),
            (GizmoAxis::Y, [0.0, 1.0, 0.0]),
            (GizmoAxis::Z, [0.0, 0.0, 1.0]),
        ] {
            for index in 0..RING_STEPS {
                let a = index as f32 / RING_STEPS as f32 * std::f32::consts::TAU;
                let b = (index + 1) as f32 / RING_STEPS as f32 * std::f32::consts::TAU;
                let pa = world_to_screen(center + ring_pt(axis_array, a, len), vp, rect);
                let pb = world_to_screen(center + ring_pt(axis_array, b, len), vp, rect);
                let dist = point_segment_distance(screen_pos, pa, pb);
                if dist <= LINE_RADIUS && best.is_none_or(|(best_dist, _)| dist < best_dist) {
                    best = Some((dist, axis));
                }
            }
        }
        return best.map(|(_, axis)| axis);
    }
    for (axis, dir) in dirs {
        let tip = world_to_screen(center + *dir * len, vp, rect);
        if (screen_pos - tip).length() <= TIP_RADIUS {
            return Some(*axis);
        }
    }
    let origin = world_to_screen(center, vp, rect);
    let mut best: Option<(f32, GizmoAxis)> = None;
    for (axis, dir) in dirs {
        let tip = world_to_screen(center + *dir * len, vp, rect);
        let dist = point_segment_distance(screen_pos, origin, tip);
        if dist <= LINE_RADIUS && best.is_none_or(|(best_dist, _)| dist < best_dist) {
            best = Some((dist, *axis));
        }
    }
    best.map(|(_, axis)| axis)
}

fn screen_delta_to_world(
    delta: egui::Vec2,
    dir: Vec3,
    center: Vec3,
    vp: Mat4,
    rect: egui::Rect,
) -> f32 {
    let p0 = world_to_screen(center, vp, rect);
    let p1 = world_to_screen(center + dir, vp, rect);
    let axis_screen = p1 - p0;
    let len = axis_screen.length();
    if len < 1.0 {
        return 0.0;
    }
    let axis_norm = axis_screen / len;
    delta.dot(axis_norm) / len
}

fn screen_delta_along_axis(
    delta: egui::Vec2,
    dir: Vec3,
    center: Vec3,
    vp: Mat4,
    rect: egui::Rect,
) -> f32 {
    let p0 = world_to_screen(center, vp, rect);
    let p1 = world_to_screen(center + dir, vp, rect);
    let axis_screen = p1 - p0;
    let length = axis_screen.length();
    if length < 1.0 {
        return 0.0;
    }
    delta.dot(axis_screen / length)
}

// ---------------------------------------------------------------------------
// Ray-AABB intersection
// ---------------------------------------------------------------------------

fn ray_aabb(origin: Vec3, dir: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let inv = Vec3::new(
        if dir.x.abs() > 1e-10 {
            1.0 / dir.x
        } else {
            f32::MAX
        },
        if dir.y.abs() > 1e-10 {
            1.0 / dir.y
        } else {
            f32::MAX
        },
        if dir.z.abs() > 1e-10 {
            1.0 / dir.z
        } else {
            f32::MAX
        },
    );
    let t1 = (min - origin) * inv;
    let t2 = (max - origin) * inv;
    let tmin = t1.min(t2).max_element();
    let tmax = t1.max(t2).min_element();
    if tmax >= tmin && tmax >= 0.0 {
        Some(tmin.max(0.0))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_hit_test_prefers_front_document_then_front_node() {
        let owner = EntityId::generate();
        let asset = AssetId::generate();
        let region = |node_id: &str, document_order, node_draw_order| SceneUiDrawRegion {
            selection: SceneUiNodeSelection {
                owner_entity: owner.clone(),
                document_asset: asset.clone(),
                node_id: node_id.to_owned(),
            },
            rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)),
            document_order,
            node_draw_order,
        };
        let regions = vec![
            region("back_document", 0, 99),
            region("front_parent", 1, 0),
            region("front_child", 1, 1),
        ];

        let picked = frontmost_ui_region(&regions, egui::pos2(50.0, 50.0))
            .expect("overlapping region must be picked");
        assert_eq!(picked.selection.node_id, "front_child");
        assert!(frontmost_ui_region(&regions, egui::pos2(150.0, 50.0)).is_none());
    }

    #[test]
    fn ui_selection_reads_primary_click_even_when_a_foreground_widget_owns_it() {
        let ctx = egui::Context::default();
        let viewport = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(200.0, 100.0));
        let click_position = viewport.center();
        let input = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(click_position),
                egui::Event::PointerButton {
                    pos: click_position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos: click_position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..egui::RawInput::default()
        };
        let mut selected_position = None;

        let _ = ctx.run_ui(input, |ui| {
            selected_position = ui_selection_click_position(ui.ctx(), viewport);
        });

        assert_eq!(selected_position, Some(click_position));
    }

    #[test]
    fn scene_view_ui_tools_default_to_visible_but_not_selectable() {
        let view = SceneView::new();
        assert!(view.show_ui_overlay);
        assert!(!view.ui_selection_enabled);
    }

    #[test]
    fn scene_camera_uses_unity_style_grab_direction_and_resets() {
        let mut camera = EditorViewCamera::default();
        let initial_yaw = camera.yaw;
        let initial_pitch = camera.pitch;
        camera.orbit_by_pointer_delta(egui::vec2(20.0, 10.0));
        assert!(camera.yaw < initial_yaw);
        assert!(camera.pitch > initial_pitch);

        camera.focus_on(Vec3::new(4.0, 2.0, -3.0));
        assert_eq!(camera.target, Vec3::new(4.0, 2.0, -3.0));
        assert_eq!(camera.distance, 6.0);
        camera.reset();
        assert_eq!(camera.target, Vec3::ZERO);
        assert_eq!(camera.distance, 10.0);
    }

    #[test]
    fn audio_distance_gizmo_preserves_an_ordered_non_negative_range() {
        assert_eq!(
            clamp_audio_distance(AudioDistanceField::Min, 12.0, 1.0, 10.0),
            10.0
        );
        assert_eq!(
            clamp_audio_distance(AudioDistanceField::Min, -2.0, 1.0, 10.0),
            0.0
        );
        assert_eq!(
            clamp_audio_distance(AudioDistanceField::Max, 0.25, 1.0, 10.0),
            1.0
        );
    }

    #[test]
    fn component_preview_updates_cloned_scene_without_mutating_authoring_source() {
        let scene = engine_authoring::test_fixtures::load_scene_fixture(
            r#"{
                "schema_version": 1,
                "entities": [{
                    "id": "entity_01JP0000000000000000000201",
                    "name": "previewed",
                    "components": {
                        "gameplay.speed": {"value": 1.0}
                    }
                }]
            }"#,
        )
        .expect("scene fixture");
        let entity = scene.entities().next().expect("fixture entity").0.clone();
        let component_type = ComponentTypeId::new("gameplay.speed");
        let preview = SceneComponentPreview {
            entity: entity.clone(),
            component_type: component_type.clone(),
            value: Value::Object(std::collections::BTreeMap::from([(
                "value".into(),
                Value::F64(4.0),
            )])),
        };

        let preview_scene =
            apply_component_preview(&scene, &preview).expect("valid preview must apply");

        assert_eq!(
            preview_scene.entity(&entity).unwrap().components[&component_type],
            preview.value
        );
        assert_ne!(
            scene.entity(&entity).unwrap().components[&component_type],
            preview.value,
            "transient preview must not mutate the authored scene"
        );
    }

    #[test]
    fn gizmo_preview_uses_drag_start_transform_as_stable_base() {
        let entity = EntityId::generate();
        let drag = GizmoDragState {
            axis: GizmoAxis::X,
            axis_dir: Vec3::X,
            mode: GizmoMode::Translate,
            accumulated_delta: 2.5,
            effective_delta: 2.5,
            origin_center: Vec3::new(1.0, 0.0, 0.0),
            base_transform: Value::Object(std::collections::BTreeMap::from([
                ("x".into(), Value::F64(1.0)),
                ("y".into(), Value::F64(0.0)),
                ("z".into(), Value::F64(0.0)),
            ])),
        };

        let preview = gizmo_component_preview(&entity, &drag).expect("gizmo preview");
        let Value::Object(fields) = preview.value else {
            panic!("transform preview must remain an object");
        };
        assert_eq!(fields.get("x"), Some(&Value::F64(3.5)));
        let Value::Object(base_fields) = &drag.base_transform else {
            panic!("drag base transform must remain an object");
        };
        assert_eq!(base_fields.get("x"), Some(&Value::F64(1.0)));
    }

    #[test]
    fn preview_app_spawns_builtin_mesh_and_scene_ui() {
        let scene = engine_authoring::test_fixtures::load_scene_fixture(
            r#"{
                "schema_version": 1,
                "entities": [
                    {
                        "id": "entity_01JP0000000000000000000201",
                        "name": "mesh",
                        "components": {
                            "engine.static_mesh_renderer": {
                                "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000102"},
                                "material": {"$type":"asset_ref","id":"asset_01JP0000000000000000000203"},
                                "material_slots": []
                            }
                        }
                    },
                    {
                        "id": "entity_01JP0000000000000000000202",
                        "name": "ui",
                        "components": {
                            "engine.ui_document": {"$type":"asset_ref","id":"asset_01JP0000000000000000000501"}
                        }
                    }
                ]
            }"#,
        )
        .expect("scene fixture");

        let (mut app, error, _bridge) = build_preview_app_with_sky(
            &scene,
            None,
            &engine::AssetManifest::default(),
            None,
            &engine::scene_bridge::SharedGltfImportCache::default(),
            &engine::SharedGpuMeshCache::default(),
            [640, 360],
            true,
        );

        assert!(error.is_none());
        let meshes = engine::Query::<&engine::Handle<engine::Mesh>>::new(app.world_mut());
        assert_eq!(meshes.iter().count(), 1);
        assert!(app.wants_ui());
    }

    #[test]
    fn preview_app_keeps_scene_when_a_component_is_invalid() {
        let scene = engine_authoring::test_fixtures::load_scene_fixture(
            r#"{
                "schema_version": 1,
                "entities": [
                    {
                        "id": "entity_01JP0000000000000000000201",
                        "name": "mesh",
                        "components": {
                            "engine.static_mesh_renderer": {
                                "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000102"},
                                "material": {"$type":"asset_ref","id":"asset_01JP0000000000000000000203"},
                                "material_slots": []
                            }
                        }
                    },
                    {
                        "id": "entity_01JP0000000000000000000202",
                        "name": "corrupted_animator",
                        "components": {
                            "engine.nav_mesh_surface": {"source": 5}
                        }
                    }
                ]
            }"#,
        )
        .expect("scene fixture");

        let (mut app, notice, _bridge) = build_preview_app_with_sky(
            &scene,
            None,
            &engine::AssetManifest::default(),
            None,
            &engine::scene_bridge::SharedGltfImportCache::default(),
            &engine::SharedGpuMeshCache::default(),
            [640, 360],
            true,
        );

        let Some(PreviewNotice::SkippedComponents(diagnostic)) = notice else {
            panic!("an invalid component must surface a skipped-components notice");
        };
        assert!(diagnostic.message.contains("engine.nav_mesh_surface"));
        assert_eq!(diagnostic.code, "editor.scene_view.components_skipped");
        assert_eq!(diagnostic.severity, engine_authoring::Severity::Warning);
        let meshes = engine::Query::<&engine::Handle<engine::Mesh>>::new(app.world_mut());
        assert_eq!(
            meshes.iter().count(),
            1,
            "the valid mesh entity must stay visible"
        );
    }

    #[test]
    fn preview_failure_notice_exposes_structured_diagnostic() {
        let notice = PreviewNotice::failure(
            "editor.scene_view.conversion_failed",
            "Scene View conversion failed: missing game component".to_owned(),
        );

        let diagnostic = notice.diagnostic();
        assert_eq!(diagnostic.code, "editor.scene_view.conversion_failed");
        assert_eq!(diagnostic.severity, engine_authoring::Severity::Error);
        assert_eq!(
            diagnostic.message,
            "Scene View conversion failed: missing game component"
        );
    }

    #[test]
    fn scene_view_reads_authored_lod_thresholds_in_order() {
        let scene = engine_authoring::test_fixtures::load_scene_fixture(
            r#"{
                "schema_version": 1,
                "entities": [{
                    "id": "entity_01JP0000000000000000000001",
                    "name": "lod",
                    "components": {
                        "engine.lod_group": {"levels": [
                            {"distance": 12.5, "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000101"}},
                            {"distance": 42.0, "mesh": {"$type":"asset_ref","id":"asset_01JP0000000000000000000102"}}
                        ]}
                    }
                }]
            }"#,
        )
        .expect("scene fixture");
        let entity = scene.entities().next().expect("fixture entity").0;

        assert_eq!(lod_distances(&scene, entity), vec![12.5, 42.0]);
    }

    #[test]
    fn apply_live_ui_document_refreshes_only_the_matching_placement() {
        let mut world = engine::ecs::World::new();

        let open_asset = engine_authoring::id::AssetId::generate();
        let other_asset = engine_authoring::id::AssetId::generate();

        let matching = world.spawn().expect("spawn matching entity");
        world
            .add_component(
                matching,
                engine::UiDocumentRef {
                    asset: open_asset.clone(),
                    document: UiDocument::default(),
                    source_path: None,
                    modified: None,
                },
            )
            .expect("attach matching document");

        let untouched = world.spawn().expect("spawn other entity");
        world
            .add_component(
                untouched,
                engine::UiDocumentRef {
                    asset: other_asset,
                    document: UiDocument::default(),
                    source_path: None,
                    modified: None,
                },
            )
            .expect("attach unrelated document");

        // A live authoring document that differs from the loaded default.
        let mut live = UiDocument::default();
        live.root.id = "live_root".to_owned();

        apply_live_ui_document(&mut world, Some((&open_asset, &live)));

        assert_eq!(
            world
                .get_component::<engine::UiDocumentRef>(matching)
                .expect("matching document remains")
                .document
                .root
                .id,
            "live_root",
            "the open asset's placement must adopt the live document"
        );
        assert_ne!(
            world
                .get_component::<engine::UiDocumentRef>(untouched)
                .expect("unrelated document remains")
                .document
                .root
                .id,
            "live_root",
            "an unrelated placement must keep its loaded document"
        );
    }

    fn manifest_with(path: &str) -> engine::AssetManifest {
        let mut manifest = engine::AssetManifest::default();
        manifest.insert(
            engine_authoring::id::AssetId::generate(),
            engine::ManifestEntry {
                path: path.into(),
                name: Some("Asset".into()),
                import_settings: engine::ImportSettings::default(),
            },
        );
        manifest
    }

    #[test]
    fn manifest_hash_is_stable_for_equal_content_and_changes_on_edits() {
        let id = engine_authoring::id::AssetId::generate();
        let entry = |path: &str| engine::ManifestEntry {
            path: path.into(),
            name: Some("Asset".into()),
            import_settings: engine::ImportSettings::default(),
        };
        let mut base = engine::AssetManifest::default();
        base.insert(id.clone(), entry("meshes/wolf.glb"));

        let mut same = engine::AssetManifest::default();
        same.insert(id.clone(), entry("meshes/wolf.glb"));
        assert_eq!(
            manifest_content_hash(&base),
            manifest_content_hash(&same),
            "equal manifest content must hash equally"
        );

        let mut moved = engine::AssetManifest::default();
        moved.insert(id, entry("meshes/wolf_v2.glb"));
        assert_ne!(
            manifest_content_hash(&base),
            manifest_content_hash(&moved),
            "changing an asset path must change the hash"
        );

        let mut added = base.clone();
        added.insert(
            engine_authoring::id::AssetId::generate(),
            entry("textures/fur.png"),
        );
        assert_ne!(
            manifest_content_hash(&base),
            manifest_content_hash(&added),
            "registering a new asset must change the hash"
        );
    }

    #[test]
    fn manifest_hash_changes_when_reimport_replaces_sub_assets() {
        let source = engine_authoring::id::AssetId::generate();
        let sub = engine_authoring::id::AssetId::derive(&source, "mesh:0");
        let with_sub = |index: u32| {
            let mut manifest = engine::AssetManifest::default();
            manifest.insert(
                source.clone(),
                engine::ManifestEntry {
                    path: "meshes/wolf.glb".into(),
                    name: Some("Wolf".into()),
                    import_settings: engine::ImportSettings {
                        sub_assets: vec![engine::ImportedSubAsset {
                            id: sub.as_str().to_owned(),
                            kind: engine::ImportedSubAssetKind::Mesh,
                            name: "body".into(),
                            index,
                            target_model_source: None,
                        }],
                        ..engine::ImportSettings::default()
                    },
                },
            );
            manifest
        };
        assert_ne!(
            manifest_content_hash(&with_sub(0)),
            manifest_content_hash(&with_sub(1)),
            "a reimport that changes sub-asset selectors must invalidate the preview"
        );
    }

    #[test]
    fn manifest_hash_changes_when_material_or_texture_override_changes() {
        let source = engine_authoring::id::AssetId::generate();
        let mut base = engine::AssetManifest::default();
        base.insert(
            source.clone(),
            engine::ManifestEntry {
                path: "models/hero.glb".into(),
                name: Some("Hero".into()),
                import_settings: engine::ImportSettings::default(),
            },
        );

        let mut material_override = base.clone();
        material_override
            .get_mut(&source)
            .expect("source must exist")
            .import_settings
            .material_remaps
            .insert(
                engine_authoring::id::AssetId::generate().as_str().to_owned(),
                engine_authoring::id::AssetId::generate().as_str().to_owned(),
            );
        assert_ne!(
            manifest_content_hash(&base),
            manifest_content_hash(&material_override),
            "a Material override must invalidate the Scene View preview"
        );

        let mut texture_override = base.clone();
        texture_override
            .get_mut(&source)
            .expect("source must exist")
            .import_settings
            .texture_remaps
            .insert(
                engine_authoring::id::AssetId::generate().as_str().to_owned(),
                engine_authoring::id::AssetId::generate().as_str().to_owned(),
            );
        assert_ne!(
            manifest_content_hash(&base),
            manifest_content_hash(&texture_override),
            "a Texture override must invalidate the Scene View preview"
        );
    }

    #[test]
    fn preview_key_differs_on_every_invalidating_input() {
        let base = PreviewKey {
            scene_revision: 1,
            manifest_hash: manifest_content_hash(&manifest_with("a.glb")),
            game_module: None,
            project_root: None,
            sky_enabled: true,
            animation_preview_enabled: false,
            animation_secondary_physics_enabled: true,
            particle_preview_enabled: true,
        };
        assert_eq!(base, base_clone(&base));
        assert_ne!(
            base,
            PreviewKey {
                scene_revision: 2,
                ..base_clone(&base)
            }
        );
        assert_ne!(
            base,
            PreviewKey {
                manifest_hash: manifest_content_hash(&manifest_with("b.glb")),
                ..base_clone(&base)
            }
        );
        assert_ne!(
            base,
            PreviewKey {
                sky_enabled: false,
                ..base_clone(&base)
            }
        );
        assert_ne!(
            base,
            PreviewKey {
                animation_preview_enabled: true,
                ..base_clone(&base)
            }
        );
        assert_ne!(
            base,
            PreviewKey {
                animation_secondary_physics_enabled: false,
                ..base_clone(&base)
            }
        );
        assert_ne!(
            base,
            PreviewKey {
                particle_preview_enabled: false,
                ..base_clone(&base)
            }
        );
        assert_ne!(
            base,
            PreviewKey {
                project_root: Some(std::path::PathBuf::from("proj")),
                ..base_clone(&base)
            }
        );
    }

    #[test]
    fn graph_parameter_changes_preserve_preview_time_but_mode_changes_restart() {
        let target = EntityId::generate();
        let mut view = SceneView::new();
        view.set_animation_preview_request(Some(AnimationPreviewRequest {
            target: target.clone(),
            mode: AnimationPreviewMode::Graph {
                parameters: BTreeMap::from([("running".to_owned(), false)]),
            },
        }));
        view.seek_animation_preview(2.5);

        view.set_animation_preview_request(Some(AnimationPreviewRequest {
            target: target.clone(),
            mode: AnimationPreviewMode::Graph {
                parameters: BTreeMap::from([("running".to_owned(), true)]),
            },
        }));
        assert_eq!(view.animation_preview_time(), 2.5);

        view.set_animation_preview_request(Some(AnimationPreviewRequest {
            target,
            mode: AnimationPreviewMode::Clip {
                clip: "Idle".to_owned(),
            },
        }));
        assert_eq!(view.animation_preview_time(), 0.0);
    }

    fn base_clone(key: &PreviewKey) -> PreviewKey {
        PreviewKey {
            scene_revision: key.scene_revision,
            manifest_hash: key.manifest_hash,
            game_module: key.game_module,
            project_root: key.project_root.clone(),
            sky_enabled: key.sky_enabled,
            animation_preview_enabled: key.animation_preview_enabled,
            animation_secondary_physics_enabled: key.animation_secondary_physics_enabled,
            particle_preview_enabled: key.particle_preview_enabled,
        }
    }

    #[test]
    fn animation_preview_interval_uses_runtime_sized_fixed_steps() {
        let mut app = engine::App::new();
        let mut remainder = 0.0;

        run_animation_preview_interval(&mut app, 0.05, &mut remainder);

        let fixed_time = app
            .world()
            .get_resource::<engine::FixedTime>()
            .expect("App must provide FixedTime");
        assert_eq!(fixed_time.step_count, 3);
        assert_eq!(
            fixed_time.fixed_delta,
            ANIMATION_PREVIEW_FIXED_STEP_SECONDS
        );
        assert!(remainder.abs() <= f32::EPSILON);
    }

    #[test]
    fn animation_preview_interval_accumulates_fractional_frames() {
        let mut app = engine::App::new();
        let mut remainder = 0.0;

        run_animation_preview_interval(
            &mut app,
            ANIMATION_PREVIEW_FIXED_STEP_SECONDS * 0.5,
            &mut remainder,
        );
        assert_eq!(
            app.world()
                .get_resource::<engine::FixedTime>()
                .expect("App must provide FixedTime")
                .step_count,
            0
        );

        run_animation_preview_interval(
            &mut app,
            ANIMATION_PREVIEW_FIXED_STEP_SECONDS * 0.5,
            &mut remainder,
        );
        let fixed_time = app
            .world()
            .get_resource::<engine::FixedTime>()
            .expect("App must provide FixedTime");
        assert_eq!(fixed_time.step_count, 1);
        assert_eq!(
            fixed_time.fixed_delta,
            ANIMATION_PREVIEW_FIXED_STEP_SECONDS
        );
        assert!(remainder.abs() <= f32::EPSILON);
    }

    #[test]
    fn animation_preview_pipeline_installs_only_requested_secondary_physics() {
        let mut animation_only = engine::App::new();
        install_animation_preview_systems(&mut animation_only, false, false);
        assert!(
            animation_only
                .world()
                .get_resource::<engine::SecondaryMotionWorlds>()
                .is_none()
        );
        run_animation_preview_step(&mut animation_only, 0.0);

        let mut with_secondary_physics = engine::App::new();
        install_animation_preview_systems(&mut with_secondary_physics, false, true);
        assert!(
            with_secondary_physics
                .world()
                .get_resource::<engine::SecondaryMotionWorlds>()
                .is_some()
        );
        run_animation_preview_step(&mut with_secondary_physics, 0.0);
    }

    #[test]
    fn transform_override_ids_cover_gizmo_and_inspector_targets() {
        let selected = EntityId::generate();
        let preview = SceneComponentPreview {
            entity: EntityId::generate(),
            component_type: transform_component_type(),
            value: engine_authoring::Value::Object(Default::default()),
        };

        assert!(current_transform_override_ids(None, false, None).is_empty());
        assert_eq!(
            current_transform_override_ids(None, true, Some(&selected)),
            vec![selected.clone()]
        );
        assert_eq!(
            current_transform_override_ids(Some(&preview), false, None),
            vec![preview.entity.clone()]
        );
        // A gizmo drag on the same entity the Inspector previews must not
        // produce a duplicate write target.
        assert_eq!(
            current_transform_override_ids(Some(&preview), true, Some(&preview.entity)),
            vec![preview.entity.clone()]
        );
    }

    #[test]
    fn authoring_local_transform_reads_translation_rotation_scale() {
        let scene = engine_authoring::test_fixtures::load_scene_fixture(
            r#"{
                "schema_version": 1,
                "entities": [{
                    "id": "entity_01JP0000000000000000000701",
                    "name": "moved",
                    "components": {
                        "engine.transform": {
                            "x": 1.0, "y": 2.0, "z": 3.0,
                            "rotation_y_degrees": 90.0,
                            "scale_x": 2.0, "scale_y": 2.0, "scale_z": 2.0
                        }
                    }
                }]
            }"#,
        )
        .expect("scene fixture");
        let entity = scene
            .entities()
            .next()
            .map(|(_, entity)| entity)
            .expect("fixture entity");
        let tx_type = engine_authoring::id::ComponentTypeId::new("engine.transform");

        let transform = authoring_local_transform(entity, &tx_type);
        assert_eq!(transform.translation, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(transform.scale, Vec3::splat(2.0));
        // 90 degrees about Y rotates +Z toward +X.
        let rotated = transform.rotation * Vec3::Z;
        assert!((rotated.x - 1.0).abs() < 1.0e-5, "rotated = {rotated:?}");
    }
}
