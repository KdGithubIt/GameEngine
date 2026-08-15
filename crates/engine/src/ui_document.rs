//! Runtime interpreter for declarative UI documents (Phase 53 / ADR 0046).
//!
//! [`UiDocumentOverlay`] implements [`crate::ui::UiSystem`] and draws a
//! [`UiDocument`](engine_authoring::ui::UiDocument) into egui each frame: it
//! walks the document tree and emits
//! `egui::Area` / `egui::Frame` / label / button widgets. There is no
//! virtual DOM or retained widget tree (ADR 0046 §2); the whole document is
//! re-interpreted every frame, matching egui's own immediate-mode model.
//!
//! Two world resources connect the interpreter to game code:
//!
//! - [`UiBindings`] holds named values ([`UiBindingValue`]) that
//!   `text.content` / `button.label` properties can reference via
//!   `UiString::Bind`. Game systems write into this table; the next frame's
//!   draw picks up the new value.
//! - [`UiEvents`] receives event names pushed when a button is clicked. A
//!   host or game system drains it once per frame.
//!
//! [`UiRuntimeDiagnostics`] records missing binding names (at most once per
//! name) so hosts can surface authoring mistakes without the interpreter
//! ever panicking on missing data.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use engine_authoring::id::AssetId;
use engine_authoring::ui::{
    UiAnchor, UiDocument, UiDocumentError, UiElementConstraints, UiLayout, UiNode, UiNodeKind,
    UiNumber, UiString,
};
use engine_ecs::{Query, Res, ResMut, World};

use crate::collision::CollisionEvents;
use crate::input::{Input, KeyCode};
use crate::save::{SaveData, SaveStore};
use crate::scripting::{
    apply_save_commands, arc_collision_snapshot, build_input_snapshot, ScriptComponent,
    ScriptEngine,
};
use crate::time::Time;
use crate::transform::Transform;
use crate::ui::{UiContext, UiSystem, UiViewport};

// ---------------------------------------------------------------------------
// Binding value and table
// ---------------------------------------------------------------------------

/// A value stored in [`UiBindings`] and referenced by `UiString::Bind`.
#[derive(Debug, Clone, PartialEq)]
pub enum UiBindingValue {
    /// A string value, rendered as-is.
    Text(String),
    /// A numeric value. Integral values render without a decimal point
    /// (see [`format_binding_value`]).
    Number(f64),
    /// A boolean value, rendered as `"true"` or `"false"`.
    Flag(bool),
}

/// A named table of runtime values that UI documents can bind to.
///
/// Game systems call [`UiBindings::set`] to publish values (score, health,
/// player name, ...); [`UiDocumentOverlay`] reads them back each frame via
/// [`resolve_ui_string`]. This is a plain resource with no interior
/// mutability: regular systems mutate it through `ResMut<UiBindings>` like
/// any other resource (ADR 0046 §4 rejects binding to arbitrary ECS
/// component paths, so this table is the only data channel into documents).
#[derive(Debug, Clone, Default)]
pub struct UiBindings {
    values: BTreeMap<String, UiBindingValue>,
}

impl UiBindings {
    /// Creates an empty binding table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets (or replaces) the value bound to `name`.
    pub fn set(&mut self, name: impl Into<String>, value: UiBindingValue) {
        self.values.insert(name.into(), value);
    }

    /// Returns the value bound to `name`, or `None` if it has not been set.
    pub fn get(&self, name: &str) -> Option<&UiBindingValue> {
        self.values.get(name)
    }

    /// Iterates all bindings in stable name order.
    ///
    /// The copied GameModule UI view uses this instead of exposing the table
    /// itself, keeping the engine-owned resource private across the module
    /// boundary while still producing deterministic payloads.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &UiBindingValue)> {
        self.values
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Removes one binding and returns its previous value, if present.
    pub fn remove(&mut self, name: &str) -> Option<UiBindingValue> {
        self.values.remove(name)
    }

    /// Removes all bindings.
    pub fn clear(&mut self) {
        self.values.clear();
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Click event names produced while drawing a [`UiDocument`].
///
/// [`crate::ui::UiSystem::run`] only receives a shared `&World`, so the
/// interpreter cannot take `ResMut<UiEvents>` while drawing. `UiEvents`
/// therefore wraps its buffer in a [`Mutex`] and exposes `&self` methods,
/// the same pattern used by `UiActionQueue` in
/// `crates/engine/examples/spire_lite.rs`: `push` locks briefly to record a
/// click, and a host or game system calls `take` once per frame to drain
/// events for routing (Rhai `on_event`, Behavior Trees, ADR 0046 §5).
#[derive(Debug, Default)]
pub struct UiEvents {
    events: Mutex<Vec<String>>,
}

/// Maximum number of events [`UiEvents`] retains before new pushes are
/// dropped (Phase 54 §1).
///
/// Protects against unbounded growth when no system drains the queue by
/// calling [`UiEvents::take`] (e.g. [`ui_event_relay_system`] was never
/// registered). Each dropped event is logged.
pub const MAX_QUEUED_UI_EVENTS: usize = 256;

impl UiEvents {
    /// Creates an empty event queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that the button bound to `name` was clicked.
    ///
    /// Once [`MAX_QUEUED_UI_EVENTS`] events are queued, further pushes are
    /// dropped and logged via `log::warn!` instead of growing the queue
    /// without bound.
    pub fn push(&self, name: impl Into<String>) {
        let mut events = self
            .events
            .lock()
            .expect("UI events mutex must not be poisoned");
        if events.len() >= MAX_QUEUED_UI_EVENTS {
            log::warn!("UiEvents queue is full ({MAX_QUEUED_UI_EVENTS} events); dropping event");
            return;
        }
        events.push(name.into());
    }

    /// Removes and returns all recorded event names, oldest first.
    pub fn take(&self) -> Vec<String> {
        self.events
            .lock()
            .expect("UI events mutex must not be poisoned")
            .drain(..)
            .collect()
    }

    /// Returns the number of events currently queued.
    pub fn len(&self) -> usize {
        self.events
            .lock()
            .expect("UI events mutex must not be poisoned")
            .len()
    }

    /// Returns `true` when no events are queued.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ---------------------------------------------------------------------------
// Frame-relayed event snapshot
// ---------------------------------------------------------------------------

/// A one-frame, read-only snapshot of the UI events recorded during the
/// previous frame (Phase 54 §1).
///
/// [`UiEvents`] is a `Mutex`-backed queue drained by a single call to
/// [`UiEvents::take`]; if every consumer (Rhai dispatch, Behavior Tree
/// conditions, game systems) drained it directly, only the first consumer in
/// a frame would ever see an event. [`ui_event_relay_system`] instead calls
/// `take` once per frame and republishes the result here, so any number of
/// systems can read the same events through `Res<UiEventFrame>` without
/// racing each other or mutating shared state.
///
/// # Latency
///
/// UI drawing (and therefore button clicks) happens at the end of a frame in
/// `App::run_ui_systems`, while the relay runs at the start of the frame
/// schedule. A click drawn during frame `N` is queued in [`UiEvents`] during
/// frame `N` and only appears in `UiEventFrame` once the relay runs again in
/// frame `N + 1`. This one-frame latency is acceptable for menu and HUD
/// interactions; it is documented here so consumers do not assume same-frame
/// delivery.
#[derive(Debug, Clone, Default)]
pub struct UiEventFrame {
    events: Vec<String>,
    generation: u64,
}

impl UiEventFrame {
    /// Creates an empty frame snapshot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the event names recorded for the current frame, oldest first.
    pub fn events(&self) -> &[String] {
        &self.events
    }

    /// Returns `true` if `name` was recorded for the current frame.
    pub fn contains(&self, name: &str) -> bool {
        self.events.iter().any(|event| event == name)
    }

    /// Returns `true` when no events were recorded for the current frame.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Returns the relay update generation, including empty frame snapshots.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Replaces the snapshot's contents, discarding the previous frame's events.
    fn replace(&mut self, events: Vec<String>) {
        self.events = events;
        self.generation = self.generation.wrapping_add(1);
    }
}

/// Moves queued [`UiEvents`] into this frame's [`UiEventFrame`] snapshot.
///
/// Register this first among the frame-schedule UI systems (before
/// [`ui_script_event_system`] and any game system that reads
/// `Res<UiEventFrame>`) so every consumer observes the same frame's events.
/// See [`UiEventFrame`]'s module documentation for the resulting one-frame
/// latency between a click and its delivery.
pub fn ui_event_relay_system(events: Res<UiEvents>, mut frame: ResMut<UiEventFrame>) {
    frame.replace(events.take());
}

/// Dispatches this frame's [`UiEventFrame`] events to every enabled, compiled
/// [`ScriptComponent`] entity's `on_event(ctx, event)` Rhai hook (ADR 0046
/// §5, Phase 54 §2).
///
/// Register this on the frame schedule, after [`ui_event_relay_system`].
/// **Do not** register it on the fixed schedule: the fixed schedule runs
/// zero or more times per frame depending on accumulated time, which would
/// either drop an event entirely (zero fixed steps that frame) or deliver it
/// more than once (multiple fixed steps), unlike the frame schedule's
/// exactly-once-per-frame guarantee.
///
/// An entity with multiple queued events receives one `on_event` call per
/// event, in queue order. `ctx.set_component(self, "engine.transform", ...)`
/// mutations are applied to the entity's own [`Transform`], mirroring
/// [`crate::scripting::scripting_update_system`]. Script errors and log
/// messages are forwarded through the `log` crate, matching
/// `scripting_update_system`'s behavior.
///
/// `ScriptEngine` is taken as `Option<ResMut<ScriptEngine>>`, not
/// `ResMut<ScriptEngine>`: this system is auto-registered by `App::new` for
/// every game, and most games that never use scripting never insert a
/// `ScriptEngine` resource. A required `ResMut` would fail every frame's
/// `ecs.update()` call for those games; the system instead no-ops when the
/// resource is absent. `SaveData` / `SaveStore` are likewise optional:
/// `ctx.save_set` / `ctx.save_write` / `ctx.save_load` commands queued by an
/// `on_event` hook are applied through
/// [`crate::scripting::apply_save_commands`] (Phase 56), the same function
/// [`crate::scripting::scripting_update_system`] uses, so both dispatch
/// sites apply save commands identically. `ctx.collisions()` is likewise
/// available here (Phase 57): `CollisionEvents` is optional, and the
/// per-entity snapshot is built the same crate-private way
/// [`crate::scripting::scripting_update_system`] builds it (`arc_collision_snapshot`).
pub fn ui_script_event_system(
    frame: Res<UiEventFrame>,
    input: Res<Input<KeyCode>>,
    script_engine: Option<ResMut<ScriptEngine>>,
    mut save_data: Option<ResMut<SaveData>>,
    mut save_store: Option<ResMut<SaveStore>>,
    collision_events: Option<Res<CollisionEvents>>,
    mut query: Query<(&mut ScriptComponent, Option<&mut Transform>)>,
) {
    let Some(mut script_engine) = script_engine else {
        return;
    };
    if frame.is_empty() {
        return;
    }
    let input_map = Arc::new(build_input_snapshot(&input));
    script_engine.set_save_snapshot(save_data.as_deref().map(|data| Arc::new(data.clone())));
    script_engine.set_collision_snapshot(arc_collision_snapshot(collision_events.as_deref()));

    for (entity, (script_comp, mut transform_opt)) in query.iter_mut() {
        if !script_comp.enabled {
            continue;
        }
        let script_id = script_comp.script.clone();
        if !script_engine.is_compiled(&script_id) {
            continue;
        }
        let entity_str = format!("{entity}");
        let transform_snap = transform_opt
            .as_ref()
            .map(|t| Arc::new(crate::scripting::transform_to_rhai_map(t)));

        for event in frame.events() {
            let result = script_engine.run_on_event(
                entity,
                &script_id,
                event.clone(),
                Arc::clone(&input_map),
                transform_snap.clone(),
            );

            if let Some(ref err) = result.error {
                log::error!("script error entity={entity} event={event}: {err}");
            }
            for msg in &result.logs {
                log::info!("[script {entity}] {msg}");
            }
            for msg in &result.save_errors {
                log::error!("[script {entity}] {msg}");
            }

            if let Some(transform) = transform_opt.as_deref_mut() {
                for cmd in &result.component_sets {
                    if cmd.component == "engine.transform" && cmd.entity_id == entity_str {
                        crate::scripting::apply_transform_fields(transform, &cmd.fields);
                    }
                }
            }

            apply_save_commands(&result, save_data.as_deref_mut(), save_store.as_deref_mut());
            script_engine.enqueue_api_commands(entity, &result);
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime diagnostics
// ---------------------------------------------------------------------------

/// Records UI binding problems observed while drawing, without panicking.
///
/// Uses the same `Mutex`-backed interior mutability as [`UiEvents`] and for
/// the same reason: [`crate::ui::UiSystem::run`] only has `&World`. Each
/// missing binding name is recorded at most once, even across many frames,
/// so a host can log or surface it without spamming duplicates.
#[derive(Debug, Default)]
pub struct UiRuntimeDiagnostics {
    missing_bindings: Mutex<BTreeSet<String>>,
}

impl UiRuntimeDiagnostics {
    /// Creates an empty diagnostics recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that binding `name` was missing when a document tried to
    /// resolve it. A name already recorded is not duplicated.
    pub fn record_missing_binding(&self, name: impl Into<String>) {
        self.missing_bindings
            .lock()
            .expect("UI runtime diagnostics mutex must not be poisoned")
            .insert(name.into());
    }

    /// Returns all distinct missing binding names recorded so far, in
    /// sorted order.
    pub fn missing_bindings(&self) -> Vec<String> {
        self.missing_bindings
            .lock()
            .expect("UI runtime diagnostics mutex must not be poisoned")
            .iter()
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// The placeholder text rendered when a bound value cannot be resolved.
const MISSING_BINDING_PLACEHOLDER: &str = "--";

/// Resolves a [`UiString`] against `bindings`.
///
/// A literal string always resolves to itself. A binding resolves to the
/// formatted value at `bindings[name]` (see [`format_binding_value`]), or
/// `None` when no such binding exists. Callers render the `"--"` placeholder
/// and record a diagnostic on `None` (ADR 0046 §4); this function never
/// panics.
pub fn resolve_ui_string(value: &UiString, bindings: &UiBindings) -> Option<String> {
    match value {
        UiString::Literal(text) => Some(text.clone()),
        UiString::Bind(name) => bindings.get(name).map(format_binding_value),
    }
}

/// Formats a [`UiBindingValue`] for display.
///
/// Numbers with no fractional part render without a decimal point (`30`,
/// not `30.0`); other numbers use the default `f64` display. Flags render
/// as `"true"` / `"false"`.
pub fn format_binding_value(value: &UiBindingValue) -> String {
    match value {
        UiBindingValue::Text(text) => text.clone(),
        UiBindingValue::Number(number) => {
            if number.is_finite() && number.fract() == 0.0 {
                format!("{}", *number as i64)
            } else {
                format!("{number}")
            }
        }
        UiBindingValue::Flag(flag) => flag.to_string(),
    }
}

/// Computes the anchored draw position for a panel within `viewport`.
///
/// `anchor` selects one of the viewport's nine reference points (corners,
/// edge midpoints, or center); `offset` is then added in viewport pixels.
/// This is a pure function so anchor math can be unit tested without an
/// egui context.
pub fn anchor_position(anchor: UiAnchor, viewport: egui::Rect, offset: (f32, f32)) -> egui::Pos2 {
    let base = match anchor {
        UiAnchor::TopLeft => viewport.left_top(),
        UiAnchor::TopCenter => egui::pos2(viewport.center().x, viewport.top()),
        UiAnchor::TopRight => viewport.right_top(),
        UiAnchor::CenterLeft => egui::pos2(viewport.left(), viewport.center().y),
        UiAnchor::Center => viewport.center(),
        UiAnchor::CenterRight => egui::pos2(viewport.right(), viewport.center().y),
        UiAnchor::BottomLeft => viewport.left_bottom(),
        UiAnchor::BottomCenter => egui::pos2(viewport.center().x, viewport.bottom()),
        UiAnchor::BottomRight => viewport.right_bottom(),
    };
    egui::pos2(base.x + offset.0, base.y + offset.1)
}

fn anchor_align(anchor: UiAnchor) -> egui::Align2 {
    match anchor {
        UiAnchor::TopLeft => egui::Align2::LEFT_TOP,
        UiAnchor::TopCenter => egui::Align2::CENTER_TOP,
        UiAnchor::TopRight => egui::Align2::RIGHT_TOP,
        UiAnchor::CenterLeft => egui::Align2::LEFT_CENTER,
        UiAnchor::Center => egui::Align2::CENTER_CENTER,
        UiAnchor::CenterRight => egui::Align2::RIGHT_CENTER,
        UiAnchor::BottomLeft => egui::Align2::LEFT_BOTTOM,
        UiAnchor::BottomCenter => egui::Align2::CENTER_BOTTOM,
        UiAnchor::BottomRight => egui::Align2::RIGHT_BOTTOM,
    }
}

fn to_color32(rgba: [f32; 4]) -> egui::Color32 {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(
        channel(rgba[0]),
        channel(rgba[1]),
        channel(rgba[2]),
        channel(rgba[3]),
    )
}

fn to_margin(padding: f32) -> egui::Margin {
    // Margin is stored as i8 in this egui version; clamp so panel padding
    // authored far outside a sane pixel range does not wrap around.
    let clamped = padding.clamp(0.0, i8::MAX as f32).round() as i8;
    egui::Margin::same(clamped)
}

#[derive(Clone)]
struct CachedUiImage {
    texture: egui::TextureHandle,
    pixels: [f32; 2],
    /// Source file modification time captured at load; a change reloads the
    /// texture so external edits appear without restarting.
    modified: Option<std::time::SystemTime>,
}

/// Draws one UI image, loading and caching its texture on first use.
///
/// `base_path` anchors relative `source` paths: editors pass the project
/// root and players their package root. `None` resolves against the process
/// working directory, which matches how packaged games run from their
/// package root. Missing files draw a labelled placeholder of the requested
/// `size` instead of failing. Returns the rectangle that was drawn.
pub fn draw_image_node_with_base(
    ui: &mut egui::Ui,
    source: &str,
    base_path: Option<&std::path::Path>,
    size: [f32; 2],
    tint: [f32; 4],
    nine_slice: Option<[f32; 4]>,
) -> egui::Rect {
    let resolved = match base_path {
        Some(base) if !std::path::Path::new(source).is_absolute() => base.join(source),
        _ => std::path::PathBuf::from(source),
    };
    let modified = std::fs::metadata(&resolved)
        .and_then(|metadata| metadata.modified())
        .ok();
    let cache_id = egui::Id::new(("ui_document_image", &resolved));
    let cached = ui
        .ctx()
        .data(|data| data.get_temp::<CachedUiImage>(cache_id))
        .filter(|cached| cached.modified == modified);
    let cached = cached.or_else(|| {
        let image = image::open(&resolved).ok()?.to_rgba8();
        let dimensions = image.dimensions();
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [dimensions.0 as usize, dimensions.1 as usize],
            image.as_raw(),
        );
        let cached = CachedUiImage {
            texture: ui.ctx().load_texture(
                source.to_owned(),
                color_image,
                egui::TextureOptions::LINEAR,
            ),
            pixels: [dimensions.0 as f32, dimensions.1 as f32],
            modified,
        };
        ui.ctx()
            .data_mut(|data| data.insert_temp(cache_id, cached.clone()));
        Some(cached)
    });
    let Some(cached) = cached else {
        return ui
            .allocate_ui(egui::vec2(size[0].max(1.0), size[1].max(1.0)), |ui| {
                ui.centered_and_justified(|ui| {
                    ui.weak(format!("Missing image\n{source}"));
                });
            })
            .response
            .rect;
    };
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(size[0].max(1.0), size[1].max(1.0)),
        egui::Sense::hover(),
    );
    let tint = to_color32(tint);
    let Some(border) = nine_slice else {
        let mut mesh = egui::Mesh::with_texture(cached.texture.id());
        mesh.add_rect_with_uv(
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            tint,
        );
        ui.painter().add(egui::Shape::mesh(mesh));
        return rect;
    };

    let source_width = cached.pixels[0].max(1.0);
    let source_height = cached.pixels[1].max(1.0);
    let left = border[0].clamp(0.0, source_width);
    let top = border[1].clamp(0.0, source_height);
    let right = border[2].clamp(0.0, source_width - left);
    let bottom = border[3].clamp(0.0, source_height - top);
    let destination_left = left.min(rect.width() * 0.5);
    let destination_right = right.min(rect.width() - destination_left);
    let destination_top = top.min(rect.height() * 0.5);
    let destination_bottom = bottom.min(rect.height() - destination_top);
    let xs = [
        rect.left(),
        rect.left() + destination_left,
        rect.right() - destination_right,
        rect.right(),
    ];
    let ys = [
        rect.top(),
        rect.top() + destination_top,
        rect.bottom() - destination_bottom,
        rect.bottom(),
    ];
    let us = [0.0, left / source_width, 1.0 - right / source_width, 1.0];
    let vs = [0.0, top / source_height, 1.0 - bottom / source_height, 1.0];
    let mut mesh = egui::Mesh::with_texture(cached.texture.id());
    for row in 0..3 {
        for column in 0..3 {
            mesh.add_rect_with_uv(
                egui::Rect::from_min_max(
                    egui::pos2(xs[column], ys[row]),
                    egui::pos2(xs[column + 1], ys[row + 1]),
                ),
                egui::Rect::from_min_max(
                    egui::pos2(us[column], vs[row]),
                    egui::pos2(us[column + 1], vs[row + 1]),
                ),
                tint,
            );
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    rect
}

// ---------------------------------------------------------------------------
// Interpreter
// ---------------------------------------------------------------------------

/// Controls optional behavior of one declarative UI draw pass.
///
/// Runtime hosts use [`Self::runtime`]. Editor previews use
/// [`Self::editor_preview`] so they can inspect the exact rectangles produced
/// by the runtime interpreter without dispatching gameplay button events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiDocumentDrawOptions {
    emit_events: bool,
    record_node_rects: bool,
    capture_input: bool,
}

impl UiDocumentDrawOptions {
    /// Draws an interactive runtime document without retaining layout records.
    pub const fn runtime() -> Self {
        Self {
            emit_events: true,
            record_node_rects: false,
            capture_input: true,
        }
    }

    /// Draws a passive editor preview and records every visible node rectangle.
    ///
    /// Buttons retain their normal appearance and layout, but clicks never
    /// enter [`UiEvents`]. This keeps Edit Mode inspection separate from game
    /// behavior while guaranteeing hit testing uses the rendered layout.
    ///
    /// The preview also claims no pointer input. Its panels are foreground
    /// [`egui::Area`]s, which would otherwise win hit testing over everything
    /// beneath them — including the editor's own panel resize handles, which
    /// share the Scene View's edges. Editor-side node picking reads pointer
    /// positions against the recorded rectangles instead, so selection keeps
    /// working without the overlay swallowing unrelated interaction.
    pub const fn editor_preview() -> Self {
        Self {
            emit_events: false,
            record_node_rects: true,
            capture_input: false,
        }
    }
}

impl Default for UiDocumentDrawOptions {
    fn default() -> Self {
        Self::runtime()
    }
}

/// One visible UI node rectangle produced by the runtime interpreter.
#[derive(Debug, Clone, PartialEq)]
pub struct UiNodeDrawRecord {
    /// Document-local stable node identifier.
    pub node_id: String,
    /// Screen-space rectangle after clipping to the active UI viewport.
    pub rect: egui::Rect,
    /// Paint traversal order within this document; larger values are in front.
    pub draw_order: u64,
}

/// Optional layout information returned from one declarative UI draw pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UiDocumentDrawReport {
    /// Visible nodes recorded in document traversal order.
    pub nodes: Vec<UiNodeDrawRecord>,
}

/// Layout report for one scene-placed UI document instance.
#[derive(Debug, Clone, PartialEq)]
pub struct UiDocumentInstanceDrawReport {
    /// Runtime entity carrying the [`UiDocumentRef`].
    pub entity: engine_ecs::Entity,
    /// Authoring asset referenced by that runtime entity.
    pub asset: AssetId,
    /// Order in which the host drew this document relative to other documents.
    pub document_order: u64,
    /// Exact visible node rectangles produced by the interpreter.
    pub nodes: Vec<UiNodeDrawRecord>,
}

struct UiNodeDrawRecorder {
    enabled: bool,
    next_draw_order: u64,
    nodes: Vec<UiNodeDrawRecord>,
}

impl UiNodeDrawRecorder {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            next_draw_order: 0,
            nodes: Vec::new(),
        }
    }

    fn begin_node(&mut self) -> u64 {
        let draw_order = self.next_draw_order;
        self.next_draw_order = self.next_draw_order.saturating_add(1);
        draw_order
    }

    fn record(&mut self, node: &UiNode, rect: egui::Rect, clip_rect: egui::Rect, draw_order: u64) {
        if !self.enabled {
            return;
        }
        let rect = rect.intersect(clip_rect);
        if rect.is_positive() {
            self.nodes.push(UiNodeDrawRecord {
                node_id: node.id.clone(),
                rect,
                draw_order,
            });
        }
    }

    fn finish(self) -> UiDocumentDrawReport {
        UiDocumentDrawReport { nodes: self.nodes }
    }
}

/// A [`crate::ui::UiSystem`] that interprets a [`UiDocument`] into egui
/// widgets every frame.
///
/// # Top-level panel promotion
///
/// The document's root node, together with any [`UiNodeKind::Panel`] nodes
/// that are *direct* children of the root, are each drawn as an
/// independently anchored `egui::Area`. Panel nodes nested more deeply are
/// drawn as ordinary nested `egui::Frame`s inside their parent's layout, not
/// as separate floating areas. This lets a single document describe several
/// independently positioned HUD panels (e.g. a top-left score panel and a
/// bottom-right minimap) as siblings under one root, while panels nested for
/// visual grouping stay attached to their parent's position.
pub struct UiDocumentOverlay {
    document: UiDocument,
}

impl UiDocumentOverlay {
    /// Wraps `document` for interpretation.
    pub fn new(document: UiDocument) -> Self {
        Self { document }
    }

    /// Returns the wrapped document.
    pub fn document(&self) -> &UiDocument {
        &self.document
    }

    /// Replaces the wrapped document, e.g. after a hot reload.
    pub fn set_document(&mut self, document: UiDocument) {
        self.document = document;
    }
}

impl UiSystem for UiDocumentOverlay {
    fn run(&mut self, ctx: &egui::Context, world: &World) {
        draw_ui_document(ctx, &self.document, world);
    }
}

/// Draws `document` into `ctx`, reading bindings, events, diagnostics, and
/// the viewport rectangle from `world` resources.
///
/// This is the shared drawing entry point behind two hosts (Phase 54 §4):
/// [`UiDocumentOverlay::run`] (Phase 53's `App::add_ui_system` path) and
/// [`crate::app::App::run_ui_systems`], which also draws every
/// [`UiDocumentRef`] found in the world so scene-placed UI documents render
/// without a Rust `UiSystem` registration. Both hosts behave identically
/// because they call this one function.
///
/// Missing resources degrade gracefully: an absent `UiBindings` renders every
/// binding as the missing-value placeholder, an absent `UiEvents` silently
/// drops click events, and an absent `UiContext` anchors panels to a
/// zero-sized viewport at the origin.
pub fn draw_ui_document(ctx: &egui::Context, document: &UiDocument, world: &World) {
    let _ = draw_ui_document_with_options(ctx, document, world, UiDocumentDrawOptions::runtime());
}

/// Explicit per-draw inputs for one declarative UI draw pass.
///
/// Decouples the interpreter from the ECS `World` so non-ECS hosts — notably the
/// editor UI Builder preview — can drive the exact same layout by supplying
/// bindings and a viewport directly. The `World`-based
/// [`draw_ui_document_with_options`] builds one of these from world resources
/// and delegates to [`draw_ui_document_with_frame`], so both hosts share a
/// single layout implementation and cannot drift (ADR 0073).
pub struct UiDrawFrame<'a> {
    /// Named values that `text.content`, `button.label`, and progress bounds
    /// resolve against.
    pub bindings: &'a UiBindings,
    /// Sink for button click events. Ignored when
    /// [`UiDocumentDrawOptions::editor_preview`] disables event emission, so
    /// editor previews may pass `None`.
    pub events: Option<&'a UiEvents>,
    /// Recorder for missing-binding names, or `None` to silently drop them.
    pub diagnostics: Option<&'a UiRuntimeDiagnostics>,
    /// Presented rectangle and target screen size the document lays out
    /// against.
    pub viewport: UiViewport,
    /// Root that relative image `source` paths resolve against. `None` resolves
    /// against the process working directory (the runtime `World` path); the
    /// editor passes the project root so authored `assets/...` sources load in
    /// the preview.
    pub base_path: Option<&'a Path>,
}

/// Draws `document` with explicit interaction and layout-reporting behavior.
///
/// The returned rectangles come from the same widget responses used for
/// painting. Callers can therefore implement editor selection without a
/// second layout implementation that could drift from runtime rendering.
pub fn draw_ui_document_with_options(
    ctx: &egui::Context,
    document: &UiDocument,
    world: &World,
    options: UiDocumentDrawOptions,
) -> UiDocumentDrawReport {
    let default_bindings = UiBindings::default();
    let bindings = world
        .get_resource::<UiBindings>()
        .unwrap_or(&default_bindings);
    let events = world.get_resource::<UiEvents>();
    let diagnostics = world.get_resource::<UiRuntimeDiagnostics>();
    let viewport = world
        .get_resource::<UiContext>()
        .map(|ui_context| ui_context.viewport)
        .unwrap_or(UiViewport::direct(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::Vec2::ZERO,
        )));
    draw_ui_document_with_frame(
        ctx,
        document,
        UiDrawFrame {
            bindings,
            events,
            diagnostics,
            viewport,
            base_path: None,
        },
        options,
    )
}

/// Draws `document` from explicit [`UiDrawFrame`] inputs, without an ECS
/// `World`.
///
/// This is the shared layout core behind [`draw_ui_document_with_options`]. Both
/// the runtime overlay and the editor UI Builder preview call it, so they render
/// identically for equal inputs (ADR 0073).
pub fn draw_ui_document_with_frame(
    ctx: &egui::Context,
    document: &UiDocument,
    frame: UiDrawFrame<'_>,
    options: UiDocumentDrawOptions,
) -> UiDocumentDrawReport {
    let UiDrawFrame {
        bindings,
        events,
        diagnostics,
        viewport,
        base_path,
    } = frame;
    // Editor previews retain button appearance and layout but must not enqueue
    // gameplay events, so suppress the sink rather than the widget.
    let events = options.emit_events.then_some(events).flatten();
    let viewport_rect = viewport.rect();
    let scale = ui_document_scale(document, viewport);
    let padding = document.safe_area_padding.map(|value| value * scale);
    let safe_viewport = egui::Rect::from_min_max(
        viewport_rect.min + egui::vec2(padding[0], padding[1]),
        viewport_rect.max - egui::vec2(padding[2], padding[3]),
    );
    let cx = NodeDrawCx {
        bindings,
        events,
        diagnostics,
        constraints: &document.constraints,
        base_path,
        scale,
        capture_input: options.capture_input,
    };
    let mut recorder = UiNodeDrawRecorder::new(options.record_node_rects);

    let root = &document.root;
    if matches!(root.kind, UiNodeKind::Panel { .. }) {
        draw_top_level_panel(root, ctx, safe_viewport, cx, true, &mut recorder);
    }
    for child in &root.children {
        if matches!(child.kind, UiNodeKind::Panel { .. }) {
            draw_top_level_panel(child, ctx, safe_viewport, cx, false, &mut recorder);
        }
    }
    recorder.finish()
}

/// Total factor from a document's authored logical units to presented points.
///
/// Two independent factors multiply here (ADR 0090):
///
/// - the document's own responsive scale for the target screen
///   ([`UiDocument::viewport_scale`]), which answers "how large is this HUD on a
///   screen of that size?", and
/// - the viewport's [`UiViewport::display_scale`], which answers "how much
///   smaller than the target screen is the surface showing it?".
///
/// Editor previews need the product; a game filling its own window has a
/// display scale of one and is therefore unaffected.
pub fn ui_document_scale(document: &UiDocument, viewport: UiViewport) -> f32 {
    let screen = viewport.screen_size();
    document.viewport_scale([screen.x, screen.y], None) * viewport.display_scale()
}

/// Scales egui's own widget metrics by the document scale.
///
/// Authored dimensions are multiplied by `cx.scale` at every use site, but nodes
/// that deliberately keep egui's defaults — a [`UiNodeKind::Button`]'s label
/// font and padding, scroll bars, indentation — are sized by the style. Without
/// this they would stay at host point sizes and grow relative to everything
/// else as the presented screen shrinks, which is exactly the mismatch ADR 0090
/// removes.
fn apply_scaled_style(ui: &mut egui::Ui, scale: f32) {
    if (scale - 1.0).abs() <= f32::EPSILON {
        return;
    }
    let style = ui.style_mut();
    for font in style.text_styles.values_mut() {
        font.size = (font.size * scale).max(1.0);
    }
    let spacing = &mut style.spacing;
    spacing.item_spacing *= scale;
    spacing.button_padding *= scale;
    spacing.indent *= scale;
    spacing.interact_size *= scale;
    spacing.icon_width *= scale;
    spacing.icon_width_inner *= scale;
    spacing.icon_spacing *= scale;
    spacing.slider_width *= scale;
    spacing.slider_rail_height *= scale;
    spacing.combo_width *= scale;
    spacing.text_edit_width *= scale;
    spacing.scroll.bar_width *= scale;
    spacing.scroll.bar_inner_margin *= scale;
    spacing.scroll.bar_outer_margin *= scale;
}

/// Immutable inputs shared by every node in one draw pass.
///
/// Bundled into one `Copy` context so the recursive draw functions pass a single
/// value instead of a long, error-prone argument list, and so a new shared input
/// (such as `base_path`) threads through without editing every call site.
#[derive(Clone, Copy)]
struct NodeDrawCx<'a> {
    /// Named values that text, labels, and progress bounds resolve against.
    bindings: &'a UiBindings,
    /// Button click sink, already gated by the draw options.
    events: Option<&'a UiEvents>,
    /// Missing-binding recorder, or `None` to drop diagnostics.
    diagnostics: Option<&'a UiRuntimeDiagnostics>,
    /// Per-node responsive constraints keyed by node id.
    constraints: &'a BTreeMap<String, UiElementConstraints>,
    /// Root for resolving relative image sources.
    base_path: Option<&'a Path>,
    /// Responsive pixel scale applied to authored sizes and offsets.
    scale: f32,
    /// Whether the document's foreground areas claim pointer input.
    ///
    /// `false` for the editor preview, whose overlay must not win hit
    /// testing over the Scene View or the editor chrome around it.
    capture_input: bool,
}

fn draw_top_level_panel(
    node: &UiNode,
    ctx: &egui::Context,
    viewport_rect: egui::Rect,
    cx: NodeDrawCx<'_>,
    skip_panel_children: bool,
    recorder: &mut UiNodeDrawRecorder,
) {
    let draw_order = recorder.begin_node();
    let UiNodeKind::Panel {
        anchor,
        offset_x,
        offset_y,
        layout,
        spacing,
        padding,
        background,
    } = &node.kind
    else {
        return;
    };

    let position = anchor_position(
        *anchor,
        viewport_rect,
        (*offset_x * cx.scale, *offset_y * cx.scale),
    );
    let response = egui::Area::new(egui::Id::new(("ui_document_panel", node.id.as_str())))
        .fixed_pos(position)
        .pivot(anchor_align(*anchor))
        .interactable(cx.capture_input)
        // Keep the bounded area at its authored anchor when possible. The
        // content wrapper below caps oversized panels; `constrain_to` then
        // only has to handle offsets that place a bounded panel partly outside
        // the viewport.
        .constrain_to(viewport_rect)
        .show(ctx, |ui| {
            // Areas live on a context layer rather than inside the Scene or
            // Game View's parent Ui. Reapply the host viewport as a clip so a
            // HUD cannot paint over editor docks when the viewport shrinks.
            ui.set_clip_rect(ui_document_clip_rect(ui.clip_rect(), viewport_rect));
            // An `Area` inherits the context style, not a parent `Ui`, so the
            // document scale has to be folded in here; nested nodes then
            // inherit the scaled style from this `Ui`.
            apply_scaled_style(ui, cx.scale);
            // `Area::constrain_to` constrains position, not size. Put the
            // panel contents in a shrinking scroll viewport so a long spacer,
            // expanding list, or oversized responsive constraint cannot make
            // the persisted Area rect exceed the host viewport. Keeping both
            // axes scrollable avoids discarding authored content when either
            // dimension overflows, while hidden bars preserve the document's
            // existing visual appearance.
            let viewport_size = viewport_rect.size().max(egui::Vec2::ZERO);
            egui::ScrollArea::both()
                .id_salt(("ui_document_panel_overflow", node.id.as_str()))
                .max_width(viewport_size.x)
                .max_height(viewport_size.y)
                // Area content is initially measured from its previous frame.
                // Raising the scrolled minimum to the cap lets content grow
                // again after a smaller frame; auto-shrink still returns the
                // outer rect to the actual content size when it fits.
                .min_scrolled_width(viewport_size.x)
                .min_scrolled_height(viewport_size.y)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    apply_node_constraints(ui, cx.constraints.get(&node.id), cx.scale);
                    let mut frame =
                        egui::Frame::new().inner_margin(to_margin(*padding * cx.scale));
                    if let Some(background) = background {
                        frame = frame.fill(to_color32(*background));
                    }
                    frame.show(ui, |ui| {
                        ui.spacing_mut().item_spacing =
                            egui::vec2(*spacing * cx.scale, *spacing * cx.scale);
                        let children = node.children.iter().filter(|child| {
                            !skip_panel_children
                                || !matches!(child.kind, UiNodeKind::Panel { .. })
                        });
                        match layout {
                            UiLayout::Vertical => {
                                ui.vertical(|ui| {
                                    for child in children {
                                        draw_node(ui, child, cx, recorder);
                                    }
                                });
                            }
                            UiLayout::Horizontal => {
                                ui.horizontal(|ui| {
                                    for child in children {
                                        draw_node(ui, child, cx, recorder);
                                    }
                                });
                            }
                        }
                    });
                });
        });
    recorder.record(node, response.response.rect, viewport_rect, draw_order);
}

fn ui_document_clip_rect(inherited: egui::Rect, viewport: egui::Rect) -> egui::Rect {
    inherited.intersect(viewport)
}

// The interpreter shares one layout path between the runtime overlay and the
// editor preview; `cx` carries the immutable inputs, `recorder` the mutable
// layout instrumentation (ADR 0073).
fn draw_node(
    ui: &mut egui::Ui,
    node: &UiNode,
    cx: NodeDrawCx<'_>,
    recorder: &mut UiNodeDrawRecorder,
) {
    let draw_order = recorder.begin_node();
    apply_node_constraints(ui, cx.constraints.get(&node.id), cx.scale);
    let rect = match &node.kind {
        UiNodeKind::Panel {
            layout,
            spacing,
            padding,
            background,
            ..
        } => {
            let mut frame = egui::Frame::new().inner_margin(to_margin(*padding * cx.scale));
            if let Some(background) = background {
                frame = frame.fill(to_color32(*background));
            }
            frame
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing =
                        egui::vec2(*spacing * cx.scale, *spacing * cx.scale);
                    match layout {
                        UiLayout::Vertical => {
                            ui.vertical(|ui| {
                                for child in &node.children {
                                    draw_node(ui, child, cx, recorder);
                                }
                            });
                        }
                        UiLayout::Horizontal => {
                            ui.horizontal(|ui| {
                                for child in &node.children {
                                    draw_node(ui, child, cx, recorder);
                                }
                            });
                        }
                    }
                })
                .response
                .rect
        }
        UiNodeKind::Text {
            content,
            size,
            color,
        } => {
            let text = resolve_or_placeholder(content, cx.bindings, cx.diagnostics);
            ui.label(
                egui::RichText::new(text)
                    .size(*size * cx.scale)
                    .color(to_color32(*color)),
            )
            .rect
        }
        UiNodeKind::Button { label, event } => {
            let text = resolve_or_placeholder(label, cx.bindings, cx.diagnostics);
            let response = ui.button(text);
            if response.clicked()
                && let Some(events) = cx.events {
                    events.push(event.clone());
                }
            response.rect
        }
        UiNodeKind::Spacer { size } => {
            ui.allocate_space(egui::Vec2::splat((*size * cx.scale).max(0.0)))
                .1
        }
        UiNodeKind::Image {
            source,
            width,
            height,
            tint,
            nine_slice,
        } => draw_image_node_with_base(
            ui,
            source,
            cx.base_path,
            [*width * cx.scale, *height * cx.scale],
            *tint,
            nine_slice.map(|border| border.map(|value| value * cx.scale)),
        ),
        UiNodeKind::ProgressBar {
            value,
            maximum,
            width,
            height,
            fill,
            background,
            show_label,
        } => {
            let value = resolve_ui_number(value, cx.bindings, cx.diagnostics).unwrap_or(0.0);
            let maximum = resolve_ui_number(maximum, cx.bindings, cx.diagnostics).unwrap_or(1.0);
            let fraction = if maximum.abs() <= f32::EPSILON {
                0.0
            } else {
                (value / maximum).clamp(0.0, 1.0)
            };
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2((*width * cx.scale).max(1.0), (*height * cx.scale).max(1.0)),
                egui::Sense::hover(),
            );
            ui.painter().rect_filled(rect, 2.0, to_color32(*background));
            let fill_rect = egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.left() + rect.width() * fraction, rect.bottom()),
            );
            ui.painter().rect_filled(fill_rect, 2.0, to_color32(*fill));
            if *show_label {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{} / {}", format_number(value), format_number(maximum)),
                    egui::FontId::proportional((rect.height() * 0.7).max(8.0)),
                    egui::Color32::WHITE,
                );
            }
            rect
        }
        UiNodeKind::Stack {
            direction,
            spacing,
            padding,
            background,
        } => draw_stack(
            ui,
            node,
            *direction,
            *spacing,
            *padding,
            *background,
            cx,
            recorder,
        ),
        UiNodeKind::Grid {
            columns,
            spacing,
            padding,
            background,
        } => {
            let mut frame = egui::Frame::new().inner_margin(to_margin(*padding * cx.scale));
            if let Some(background) = background {
                frame = frame.fill(to_color32(*background));
            }
            frame
                .show(ui, |ui| {
                    egui::Grid::new(("ui_document_grid", node.id.as_str()))
                        .num_columns((*columns).max(1))
                        .spacing([*spacing * cx.scale, *spacing * cx.scale])
                        .show(ui, |ui| {
                            for (index, child) in node.children.iter().enumerate() {
                                draw_node(ui, child, cx, recorder);
                                if (index + 1) % (*columns).max(1) == 0 {
                                    ui.end_row();
                                }
                            }
                        });
                })
                .response
                .rect
        }
        UiNodeKind::Overlay {
            padding,
            background,
        } => {
            let available = ui
                .available_rect_before_wrap()
                .shrink((*padding * cx.scale).max(0.0));
            let (rect, _) = ui.allocate_exact_size(available.size(), egui::Sense::hover());
            if let Some(background) = background {
                ui.painter().rect_filled(rect, 0.0, to_color32(*background));
            }
            for child in &node.children {
                let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                draw_node(&mut child_ui, child, cx, recorder);
            }
            rect
        }
        UiNodeKind::ScrollView {
            horizontal,
            vertical,
            max_width,
            max_height,
        } => {
            let mut area = egui::ScrollArea::new([*horizontal, *vertical]);
            if let Some(width) = max_width {
                area = area.max_width(*width * cx.scale);
            }
            if let Some(height) = max_height {
                area = area.max_height(*height * cx.scale);
            }
            area.show(ui, |ui| {
                for child in &node.children {
                    draw_node(ui, child, cx, recorder);
                }
            })
            .inner_rect
        }
    };
    recorder.record(node, rect, ui.clip_rect(), draw_order);
}

#[allow(clippy::too_many_arguments)]
fn draw_stack(
    ui: &mut egui::Ui,
    node: &UiNode,
    direction: UiLayout,
    spacing: f32,
    padding: f32,
    background: Option<[f32; 4]>,
    cx: NodeDrawCx<'_>,
    recorder: &mut UiNodeDrawRecorder,
) -> egui::Rect {
    let mut frame = egui::Frame::new().inner_margin(to_margin(padding * cx.scale));
    if let Some(background) = background {
        frame = frame.fill(to_color32(background));
    }
    frame
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(spacing * cx.scale, spacing * cx.scale);
            let draw = |ui: &mut egui::Ui| {
                for child in &node.children {
                    draw_node(ui, child, cx, recorder);
                }
            };
            match direction {
                UiLayout::Vertical => {
                    ui.vertical(draw);
                }
                UiLayout::Horizontal => {
                    ui.horizontal(draw);
                }
            }
        })
        .response
        .rect
}

fn apply_node_constraints(
    ui: &mut egui::Ui,
    constraints: Option<&UiElementConstraints>,
    scale: f32,
) {
    let Some(constraints) = constraints else {
        return;
    };
    if let Some([width, height]) = constraints.minimum_size {
        ui.set_min_size(egui::vec2(width * scale, height * scale));
    }
    if let Some([width, height]) = constraints.maximum_size {
        ui.set_max_size(egui::vec2(width * scale, height * scale));
    }
    if let Some(aspect) = constraints.aspect_ratio.filter(|value| *value > 0.0) {
        let available = ui.available_size();
        let size = if available.x / available.y.max(f32::EPSILON) > aspect {
            egui::vec2(available.y * aspect, available.y)
        } else {
            egui::vec2(available.x, available.x / aspect)
        };
        ui.set_max_size(size);
    }
}

fn resolve_ui_number(
    value: &UiNumber,
    bindings: &UiBindings,
    diagnostics: Option<&UiRuntimeDiagnostics>,
) -> Option<f32> {
    match value {
        UiNumber::Literal(value) => Some(*value),
        UiNumber::Bind(name) => match bindings.get(name) {
            Some(UiBindingValue::Number(value)) => Some(*value as f32),
            Some(UiBindingValue::Flag(value)) => Some(if *value { 1.0 } else { 0.0 }),
            Some(UiBindingValue::Text(value)) => value.parse().ok(),
            None => {
                if let Some(diagnostics) = diagnostics {
                    diagnostics.record_missing_binding(name.clone());
                }
                None
            }
        },
    }
}

fn format_number(value: f32) -> String {
    if value.fract().abs() <= f32::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn resolve_or_placeholder(
    value: &UiString,
    bindings: &UiBindings,
    diagnostics: Option<&UiRuntimeDiagnostics>,
) -> String {
    match resolve_ui_string(value, bindings) {
        Some(text) => text,
        None => {
            if let (UiString::Bind(name), Some(diagnostics)) = (value, diagnostics) {
                diagnostics.record_missing_binding(name.clone());
            }
            MISSING_BINDING_PLACEHOLDER.to_string()
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Describes why [`load_ui_document`] failed.
#[derive(Debug)]
pub enum UiDocumentLoadError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The file contents were not a valid UI document.
    Document(UiDocumentError),
    /// Parsed document failed whole-document validation.
    Validation(Vec<engine_authoring::Diagnostic>),
}

impl fmt::Display for UiDocumentLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "failed to read UI document file: {error}"),
            Self::Document(error) => write!(f, "failed to parse UI document: {error}"),
            Self::Validation(diagnostics) => write!(
                f,
                "UI document validation failed with {} error(s)",
                diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.severity == engine_authoring::Severity::Error)
                    .count()
            ),
        }
    }
}

impl std::error::Error for UiDocumentLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Document(error) => Some(error),
            Self::Validation(_) => None,
        }
    }
}

/// Reads and parses a `.ui.json` file from `path`.
///
/// # Errors
///
/// - [`UiDocumentLoadError::Io`] when the file cannot be read.
/// - [`UiDocumentLoadError::Document`] when the contents are not a valid
///   [`UiDocument`] (malformed JSON or an unsupported `schema_version`).
pub fn load_ui_document(path: &Path) -> Result<UiDocument, UiDocumentLoadError> {
    let contents = std::fs::read_to_string(path).map_err(UiDocumentLoadError::Io)?;
    let document = UiDocument::from_json_str(&contents).map_err(UiDocumentLoadError::Document)?;
    let diagnostics = document.validate();
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == engine_authoring::Severity::Error)
    {
        return Err(UiDocumentLoadError::Validation(diagnostics));
    }
    Ok(document)
}

// ---------------------------------------------------------------------------
// Scene-placed UI documents
// ---------------------------------------------------------------------------

/// Attaches a loaded [`UiDocument`] to a scene entity so the built-in host
/// (`App::run_ui_systems`) draws it every frame without a Rust
/// [`crate::ui::UiSystem`] registration (Phase 54 §4).
///
/// Spawned by the `engine.ui_document` authoring component
/// (`crate::scene_bridge::spawn_ui_document_component`). `source_path` and
/// `modified` are `None` for the built-in document and for documents that
/// failed to load; they are populated for manifest-loaded documents so
/// [`ui_document_reload_system`] can detect file changes and hot-reload.
#[derive(Debug, Clone)]
pub struct UiDocumentRef {
    /// The authoring asset id this document was resolved from.
    pub asset: AssetId,
    /// The currently active document tree.
    pub document: UiDocument,
    /// Absolute filesystem path the document was loaded from, or `None` for
    /// the built-in document or a document that failed to load.
    pub source_path: Option<PathBuf>,
    /// The source file's modification time as of the last successful load,
    /// used by [`ui_document_reload_system`] to detect changes.
    pub modified: Option<SystemTime>,
}

/// Optional runtime visibility override for a scene-placed UI document.
///
/// Absence means visible, preserving every existing authored scene. Project
/// Rust commands attach this component only when a document is explicitly
/// shown or hidden, so visibility remains runtime state and is not silently
/// persisted into authoring data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiDocumentVisibility {
    /// Whether the sibling [`UiDocumentRef`] should be drawn.
    pub visible: bool,
}

impl Default for UiDocumentVisibility {
    fn default() -> Self {
        Self { visible: true }
    }
}

/// Returns whether a scene-placed UI document should be drawn this frame.
pub(crate) fn ui_document_is_visible(world: &World, entity: engine_ecs::Entity) -> bool {
    world
        .get_component::<UiDocumentVisibility>(entity)
        .is_none_or(|visibility| visibility.visible)
}

/// Minimum interval between filesystem mtime checks performed by
/// [`ui_document_reload_system`] (Phase 54 §5).
pub const UI_RELOAD_INTERVAL_SECONDS: f32 = 0.5;

/// Frame-delta accumulator driving [`ui_document_reload_system`]'s polling
/// interval.
///
/// The runtime ECS has no `Local<T>`-style per-system state, so this small
/// resource plays the same role as [`crate::time::FixedTime`]'s internal
/// accumulator field: it is inserted once by `App::new` and only ever
/// touched by this one system.
#[derive(Debug, Clone, Default)]
pub struct UiReloadTimer {
    accumulator: f32,
}

impl UiReloadTimer {
    /// Creates a timer with no accumulated time.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `delta` seconds; returns `true` (and resets the accumulator) once
    /// [`UI_RELOAD_INTERVAL_SECONDS`] have accumulated.
    fn tick(&mut self, delta: f32) -> bool {
        self.accumulator += delta;
        if self.accumulator >= UI_RELOAD_INTERVAL_SECONDS {
            self.accumulator = 0.0;
            true
        } else {
            false
        }
    }
}

/// Hot-reloads [`UiDocumentRef`] components whose source file changed on
/// disk (Phase 54 §5, desktop-only).
///
/// Every [`UI_RELOAD_INTERVAL_SECONDS`] of accumulated [`Time`] delta, every
/// [`UiDocumentRef`] with a `source_path` has its file mtime compared against
/// the stored `modified` value. A newer mtime triggers a reload: success
/// replaces both `document` and `modified`; failure (the file is mid-write,
/// contains invalid JSON, or was removed) keeps the previous document and
/// logs one `log::warn!`, so a Play session never loses its UI over a
/// transient parse error. References without a `source_path` (the built-in
/// document, or one that never loaded) are skipped.
#[cfg(not(target_arch = "wasm32"))]
pub fn ui_document_reload_system(
    time: Res<Time>,
    mut timer: ResMut<UiReloadTimer>,
    mut documents: Query<&mut UiDocumentRef>,
) {
    if !timer.tick(time.delta_seconds) {
        return;
    }

    for (_, document_ref) in documents.iter_mut() {
        let Some(source_path) = document_ref.source_path.clone() else {
            continue;
        };
        let modified = match std::fs::metadata(&source_path).and_then(|meta| meta.modified()) {
            Ok(modified) => modified,
            Err(error) => {
                log::warn!(
                    "ui_document_reload: failed to read metadata for {}: {error}",
                    source_path.display()
                );
                continue;
            }
        };
        if document_ref.modified == Some(modified) {
            continue;
        }
        match load_ui_document(&source_path) {
            Ok(document) => {
                document_ref.document = document;
                document_ref.modified = Some(modified);
            }
            Err(error) => {
                log::warn!(
                    "ui_document_reload: failed to reload {}: {error}",
                    source_path.display()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save::SaveValue;
    use engine_authoring::ui::{
        UiAnchor as Anchor, UiLayout as Layout, UiScaleMatch, UiScalePolicy,
    };

    #[test]
    fn format_binding_value_renders_integral_number_without_decimal_point() {
        let text = format_binding_value(&UiBindingValue::Number(30.0));
        assert_eq!(text, "30");
    }

    #[test]
    fn format_binding_value_renders_fractional_number_with_decimal_point() {
        let text = format_binding_value(&UiBindingValue::Number(3.5));
        assert_eq!(text, "3.5");
    }

    #[test]
    fn format_binding_value_renders_flags_as_true_or_false() {
        assert_eq!(format_binding_value(&UiBindingValue::Flag(true)), "true");
        assert_eq!(format_binding_value(&UiBindingValue::Flag(false)), "false");
    }

    #[test]
    fn format_binding_value_renders_text_as_is() {
        let text = format_binding_value(&UiBindingValue::Text("hello".to_string()));
        assert_eq!(text, "hello");
    }

    #[test]
    fn resolve_ui_string_passes_through_literal() {
        let bindings = UiBindings::new();
        let resolved = resolve_ui_string(&UiString::Literal("hi".to_string()), &bindings);
        assert_eq!(resolved, Some("hi".to_string()));
    }

    #[test]
    fn resolve_ui_string_resolves_present_binding() {
        let mut bindings = UiBindings::new();
        bindings.set("score", UiBindingValue::Number(42.0));
        let resolved = resolve_ui_string(&UiString::Bind("score".to_string()), &bindings);
        assert_eq!(resolved, Some("42".to_string()));
    }

    #[test]
    fn resolve_ui_string_returns_none_for_missing_binding() {
        let bindings = UiBindings::new();
        let resolved = resolve_ui_string(&UiString::Bind("missing".to_string()), &bindings);
        assert_eq!(resolved, None);
    }

    #[test]
    fn anchor_position_covers_all_nine_anchors_on_a_known_rect() {
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let zero = (0.0, 0.0);
        assert_eq!(
            anchor_position(Anchor::TopLeft, viewport, zero),
            egui::pos2(0.0, 0.0)
        );
        assert_eq!(
            anchor_position(Anchor::TopCenter, viewport, zero),
            egui::pos2(400.0, 0.0)
        );
        assert_eq!(
            anchor_position(Anchor::TopRight, viewport, zero),
            egui::pos2(800.0, 0.0)
        );
        assert_eq!(
            anchor_position(Anchor::CenterLeft, viewport, zero),
            egui::pos2(0.0, 300.0)
        );
        assert_eq!(
            anchor_position(Anchor::Center, viewport, zero),
            egui::pos2(400.0, 300.0)
        );
        assert_eq!(
            anchor_position(Anchor::CenterRight, viewport, zero),
            egui::pos2(800.0, 300.0)
        );
        assert_eq!(
            anchor_position(Anchor::BottomLeft, viewport, zero),
            egui::pos2(0.0, 600.0)
        );
        assert_eq!(
            anchor_position(Anchor::BottomCenter, viewport, zero),
            egui::pos2(400.0, 600.0)
        );
        assert_eq!(
            anchor_position(Anchor::BottomRight, viewport, zero),
            egui::pos2(800.0, 600.0)
        );
    }

    #[test]
    fn anchor_position_applies_offset() {
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let position = anchor_position(Anchor::TopLeft, viewport, (12.0, 8.0));
        assert_eq!(position, egui::pos2(12.0, 8.0));

        let position = anchor_position(Anchor::BottomRight, viewport, (-12.0, -8.0));
        assert_eq!(position, egui::pos2(788.0, 592.0));
    }

    #[test]
    fn ui_events_push_and_take_drains_recorded_events() {
        let events = UiEvents::new();
        assert!(events.is_empty());
        events.push("start_game");
        events.push("open_menu");
        assert_eq!(events.len(), 2);
        let drained = events.take();
        assert_eq!(
            drained,
            vec!["start_game".to_string(), "open_menu".to_string()]
        );
        assert!(events.is_empty());
        assert!(events.take().is_empty());
    }

    #[test]
    fn ui_runtime_diagnostics_records_missing_binding_once_across_frames() {
        let diagnostics = UiRuntimeDiagnostics::new();
        diagnostics.record_missing_binding("score");
        diagnostics.record_missing_binding("score");
        diagnostics.record_missing_binding("health");
        let missing = diagnostics.missing_bindings();
        assert_eq!(missing, vec!["health".to_string(), "score".to_string()]);
    }

    #[test]
    fn ui_bindings_set_get_and_clear_round_trip() {
        let mut bindings = UiBindings::new();
        assert!(bindings.get("score").is_none());
        bindings.set("score", UiBindingValue::Number(10.0));
        assert_eq!(bindings.get("score"), Some(&UiBindingValue::Number(10.0)));
        bindings.clear();
        assert!(bindings.get("score").is_none());
    }

    fn document_with_button(event: &str) -> UiDocument {
        UiDocument {
            schema_version: engine_authoring::ui::UI_SCHEMA_VERSION,
            root: UiNode {
                id: "root".to_string(),
                kind: UiNodeKind::Panel {
                    anchor: Anchor::TopLeft,
                    offset_x: 0.0,
                    offset_y: 0.0,
                    layout: Layout::Vertical,
                    spacing: 4.0,
                    padding: 8.0,
                    background: None,
                },
                children: vec![
                    UiNode {
                        id: "label".to_string(),
                        kind: UiNodeKind::Text {
                            content: UiString::Literal("Hud".to_string()),
                            size: 16.0,
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                        children: Vec::new(),
                    },
                    UiNode {
                        id: "button".to_string(),
                        kind: UiNodeKind::Button {
                            label: UiString::Literal("Start".to_string()),
                            event: event.to_string(),
                        },
                        children: Vec::new(),
                    },
                ],
            },
            ..UiDocument::default()
        }
    }

    /// Headless egui interaction test.
    ///
    /// `egui::Context::default()` runs without a window; `ctx.run` drives a
    /// full frame given synthesized `RawInput`. The document is drawn twice
    /// (egui widgets need one frame to register their interactive rect
    /// before a synthesized pointer click on the second frame is recognized
    /// against the same `Id`), then the produced `FullOutput` is checked for
    /// non-empty shapes to confirm the interpreter actually emitted
    /// widgets. See module docs for why `UiEvents`/`UiRuntimeDiagnostics`
    /// mechanics are exercised directly instead of relying on a synthesized
    /// click to land precisely — attempting to compute the button's screen
    /// rect ahead of layout proved brittle across egui's own internal
    /// spacing, so this test asserts the document renders and the
    /// event/diagnostics resources work in isolation (see the dedicated
    /// unit tests above).
    #[test]
    fn overlay_draws_document_and_produces_non_empty_output() {
        let world = World::new();
        let mut overlay = UiDocumentOverlay::new(document_with_button("start_game"));
        let ctx = egui::Context::default();

        let mut last_output = None;
        for _ in 0..2 {
            let raw_input = egui::RawInput::default();
            let output = ctx.run_ui(raw_input, |ui| {
                overlay.run(ui.ctx(), &world);
            });
            last_output = Some(output);
        }

        let output = last_output.expect("must have run at least one frame");
        assert!(
            !output.shapes.is_empty(),
            "document draw must produce shapes"
        );
    }

    #[test]
    fn editor_preview_reports_runtime_node_rectangles_in_front_to_back_order() {
        let mut world = World::new();
        let document = document_with_button("start_game");
        let ctx = egui::Context::default();
        world.insert_resource(UiContext::new(
            ctx.clone(),
            UiViewport::direct(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 360.0),
            )),
        ));

        let mut report = UiDocumentDrawReport::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            report = draw_ui_document_with_options(
                ui.ctx(),
                &document,
                &world,
                UiDocumentDrawOptions::editor_preview(),
            );
        });

        assert_eq!(
            report
                .nodes
                .iter()
                .map(|node| node.node_id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["root", "label", "button"])
        );
        assert!(report.nodes.iter().all(|node| node.rect.is_positive()));
        let root_order = report
            .nodes
            .iter()
            .find(|node| node.node_id == "root")
            .expect("root record")
            .draw_order;
        let button_order = report
            .nodes
            .iter()
            .find(|node| node.node_id == "button")
            .expect("button record")
            .draw_order;
        assert!(
            button_order > root_order,
            "children must pick above parents"
        );
    }

    #[test]
    fn editor_preview_button_click_does_not_emit_gameplay_event() {
        let mut world = World::new();
        let document = document_with_button("start_game");
        let ctx = egui::Context::default();
        world.insert_resource(UiContext::new(
            ctx.clone(),
            UiViewport::direct(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 360.0),
            )),
        ));
        world.insert_resource(UiEvents::new());

        let mut report = UiDocumentDrawReport::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            report = draw_ui_document_with_options(
                ui.ctx(),
                &document,
                &world,
                UiDocumentDrawOptions::editor_preview(),
            );
        });
        let button_center = report
            .nodes
            .iter()
            .find(|node| node.node_id == "button")
            .expect("button record")
            .rect
            .center();
        let click_input = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(button_center),
                egui::Event::PointerButton {
                    pos: button_center,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos: button_center,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..egui::RawInput::default()
        };
        let _ = ctx.run_ui(click_input, |ui| {
            let _ = draw_ui_document_with_options(
                ui.ctx(),
                &document,
                &world,
                UiDocumentDrawOptions::editor_preview(),
            );
        });

        assert!(
            world
                .get_resource::<UiEvents>()
                .expect("events resource")
                .is_empty(),
            "Edit Mode clicks must not enter the gameplay event queue"
        );
    }

    /// Returns the layer egui would route a click to at the fixture
    /// document's button, after the overlay has been drawn with `options`.
    ///
    /// `Areas::layer_id_at` skips non-interactable areas, so a preview
    /// overlay must not appear here: everything behind it — including the
    /// editor's own panel resize handles, which share the Scene View's edges
    /// — is reached through this lookup.
    fn layer_under_overlay(
        options: UiDocumentDrawOptions,
    ) -> (Option<egui::LayerId>, egui::LayerId) {
        let mut world = World::new();
        let document = document_with_button("start_game");
        let ctx = egui::Context::default();
        world.insert_resource(UiContext::new(
            ctx.clone(),
            UiViewport::direct(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(640.0, 360.0),
            )),
        ));
        world.insert_resource(UiEvents::new());

        let mut report = UiDocumentDrawReport::default();
        // Several frames so the area state egui consults has settled.
        for _ in 0..3 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                report = draw_ui_document_with_options(ui.ctx(), &document, &world, options);
            });
        }
        let center = report
            .nodes
            .iter()
            .find(|node| node.node_id == "button")
            .map(|node| node.rect.center())
            // `runtime()` records nothing; the fixture panel is anchored at
            // the viewport's top-left, so probe just inside it.
            .unwrap_or_else(|| egui::pos2(8.0, 8.0));
        let document_layer = egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new(("ui_document_panel", document.root.id.as_str())),
        );
        (ctx.layer_id_at(center), document_layer)
    }

    #[test]
    fn the_editor_preview_overlay_does_not_claim_pointer_input() {
        // The overlay draws into its own area layer, which wins egui hit
        // testing over everything behind it. In the editor that includes the
        // bottom dock's resize handle, which shares the Scene View's lower
        // edge: a HUD panel reaching that edge made the dock impossible to
        // drag anywhere the panel covered.
        let (hit, document_layer) = layer_under_overlay(UiDocumentDrawOptions::editor_preview());

        assert_ne!(
            hit,
            Some(document_layer),
            "editor chrome behind a preview panel must stay reachable"
        );
    }

    #[test]
    fn a_playing_document_still_claims_pointer_input() {
        let (hit, document_layer) = layer_under_overlay(UiDocumentDrawOptions::runtime());

        assert_eq!(
            hit,
            Some(document_layer),
            "a playing document must keep receiving pointer input"
        );
    }

    #[test]
    fn a_preview_panel_taller_than_its_viewport_leaves_the_dock_edge_grabbable() {
        // Reproduces the editor layout: a resizable bottom dock, a Scene View
        // filling the rest, and a HUD panel whose spacer makes its area
        // extend far below the visible viewport. The area is clipped when
        // painted but still occupies its full rect for hit testing, so it
        // used to cover the dock's resize edge and make it ungrabbable
        // wherever the panel sat horizontally.
        let mut world = World::new();
        let document = UiDocument {
            root: UiNode {
                id: "hud_root".into(),
                kind: UiNodeKind::Panel {
                    anchor: UiAnchor::TopLeft,
                    offset_x: 18.0,
                    offset_y: 18.0,
                    layout: UiLayout::Vertical,
                    spacing: 7.0,
                    padding: 14.0,
                    background: None,
                },
                children: vec![UiNode {
                    id: "tall_spacer".into(),
                    kind: UiNodeKind::Spacer { size: 500.0 },
                    children: Vec::new(),
                }],
            },
            ..UiDocument::default()
        };

        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 600.0));
        let ctx = egui::Context::default();
        let viewport = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1000.0, 400.0));
        world.insert_resource(UiContext::new(ctx.clone(), UiViewport::direct(viewport)));
        world.insert_resource(UiEvents::new());

        let input = egui::RawInput {
            screen_rect: Some(screen),
            ..egui::RawInput::default()
        };
        // Several frames so egui's stored area state has settled.
        for _ in 0..3 {
            let _ = ctx.run_ui(input.clone(), |ui| {
                egui::Panel::bottom("dock")
                    .resizable(true)
                    .default_size(200.0)
                    .show_inside(ui, |ui| {
                        ui.label("assets");
                    });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    ui.label("scene view");
                });
                let _ = draw_ui_document_with_options(
                    ui.ctx(),
                    &document,
                    &world,
                    UiDocumentDrawOptions::editor_preview(),
                );
            });
        }

        // A point on the dock's top edge, horizontally inside the HUD panel.
        let dock_edge = egui::pos2(60.0, screen.max.y - 200.0);
        let document_layer = egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new(("ui_document_panel", "hud_root")),
        );

        assert_ne!(
            ctx.layer_id_at(dock_edge),
            Some(document_layer),
            "the preview must not cover the dock's resize edge"
        );
    }

    #[test]
    fn a_panel_taller_than_its_viewport_stays_inside_it() {
        // A HUD whose content exceeds the viewport (here a long spacer) used
        // to keep its full rect. Areas live on a layer above the host's, so
        // the overflow sat over the editor docks below the Scene View —
        // visually behind them, but above in layer order, painting over them
        // and taking their pointer input.
        let mut world = World::new();
        let document = UiDocument {
            root: UiNode {
                id: "hud_root".into(),
                kind: UiNodeKind::Panel {
                    anchor: UiAnchor::TopLeft,
                    offset_x: 18.0,
                    offset_y: 18.0,
                    layout: UiLayout::Vertical,
                    spacing: 7.0,
                    padding: 14.0,
                    background: None,
                },
                children: vec![UiNode {
                    id: "tall_spacer".into(),
                    kind: UiNodeKind::Spacer { size: 500.0 },
                    children: Vec::new(),
                }],
            },
            ..UiDocument::default()
        };

        let ctx = egui::Context::default();
        let viewport = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(640.0, 200.0));
        world.insert_resource(UiContext::new(ctx.clone(), UiViewport::direct(viewport)));
        world.insert_resource(UiEvents::new());

        // Several frames: an `Area` sizes itself from the previous pass, so
        // the overflow only becomes visible once its content has been laid
        // out at least once.
        for _ in 0..3 {
            let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
                let _ = draw_ui_document_with_options(
                    ui.ctx(),
                    &document,
                    &world,
                    UiDocumentDrawOptions::editor_preview(),
                );
            });
        }

        // Inspect the persisted Area state rather than the clipped paint
        // primitives. Painting was already clipped correctly, but egui still
        // retained the oversized Area rect for layer ordering and hit tests.
        let area_id = egui::Id::new(("ui_document_panel", "hud_root"));
        let area_rect = ctx
            .memory(|memory| memory.area_rect(area_id))
            .expect("the document panel area must be stored after drawing");

        assert!(
            viewport.contains_rect(area_rect),
            "the panel area escaped its viewport {viewport:?}: {area_rect:?}",
        );
    }

    #[test]
    fn overlay_run_without_ui_resources_does_not_panic() {
        let world = World::new();
        let mut overlay = UiDocumentOverlay::new(UiDocument::default());
        let ctx = egui::Context::default();
        let raw_input = egui::RawInput::default();
        let _ = ctx.run_ui(raw_input, |ui| {
            overlay.run(ui.ctx(), &world);
        });
    }

    #[test]
    fn load_ui_document_reads_valid_file() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let path = dir.path().join("hud.ui.json");
        std::fs::write(
            &path,
            r#"{"schema_version":3,"root":{"id":"root","type":"spacer","size":8.0}}"#,
        )
        .expect("must write file");

        let document = load_ui_document(&path).expect("must load valid document");
        assert_eq!(document.root.id, "root");
    }

    #[test]
    fn load_ui_document_reports_malformed_json() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let path = dir.path().join("bad.ui.json");
        std::fs::write(&path, "{ not valid json").expect("must write file");

        assert!(matches!(
            load_ui_document(&path),
            Err(UiDocumentLoadError::Document(UiDocumentError::Json(_)))
        ));
    }

    #[test]
    fn load_ui_document_reports_unsupported_version() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let path = dir.path().join("future.ui.json");
        std::fs::write(
            &path,
            r#"{"schema_version":4,"root":{"id":"root","type":"spacer","size":8.0}}"#,
        )
        .expect("must write file");

        assert!(matches!(
            load_ui_document(&path),
            Err(UiDocumentLoadError::Document(
                UiDocumentError::UnsupportedVersion { found: 4 }
            ))
        ));
    }

    #[test]
    fn load_ui_document_reports_missing_file() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let path = dir.path().join("missing.ui.json");
        assert!(matches!(
            load_ui_document(&path),
            Err(UiDocumentLoadError::Io(_))
        ));
    }

    // --- UiEventFrame relay (Phase 54 §1) -----------------------------------

    #[test]
    fn ui_event_relay_system_moves_events_into_frame_and_clears_next_run() {
        let mut app = engine_ecs::App::new();
        app.insert_resource(UiEvents::new());
        app.insert_resource(UiEventFrame::new());
        app.add_system(ui_event_relay_system);

        app.world()
            .get_resource::<UiEvents>()
            .expect("UiEvents must be present")
            .push("start_game");
        app.world()
            .get_resource::<UiEvents>()
            .expect("UiEvents must be present")
            .push("open_menu");

        app.update().expect("relay system must run");
        let frame = app
            .world()
            .get_resource::<UiEventFrame>()
            .expect("UiEventFrame must be present");
        assert!(frame.contains("start_game"));
        assert!(frame.contains("open_menu"));
        assert_eq!(frame.events().len(), 2);

        app.update().expect("relay system must run again");
        let frame = app
            .world()
            .get_resource::<UiEventFrame>()
            .expect("UiEventFrame must be present");
        assert!(
            frame.is_empty(),
            "a frame with no new UiEvents pushes must relay as empty"
        );
    }

    #[test]
    fn ui_events_push_drops_events_past_the_queue_cap() {
        let events = UiEvents::new();
        for i in 0..(MAX_QUEUED_UI_EVENTS + 44) {
            events.push(format!("event_{i}"));
        }
        assert_eq!(events.len(), MAX_QUEUED_UI_EVENTS);
    }

    // --- Rhai on_event dispatch (Phase 54 §2) -------------------------------

    #[test]
    fn ui_script_event_system_dispatches_frame_events_to_on_event_hook() {
        let mut app = engine_ecs::App::new();
        app.insert_resource(Input::<KeyCode>::new());

        let mut frame = UiEventFrame::new();
        frame.replace(vec!["start_game".to_string()]);
        app.insert_resource(frame);

        let asset_id = AssetId::generate();
        let mut script_engine = ScriptEngine::default();
        script_engine
            .compile(
                &asset_id,
                r#"fn on_event(ctx, event) {
                    if event == "start_game" {
                        ctx.set_component(ctx.self_entity(), "engine.transform", #{ x: 7.0, y: 0.0, z: 0.0 });
                    }
                }"#,
            )
            .expect("test script must compile");
        app.insert_resource(script_engine);

        let entity = app
            .world_mut()
            .spawn_with(ScriptComponent::new(asset_id))
            .expect("must spawn script entity");
        app.world_mut()
            .add_component(entity, Transform::default())
            .expect("must attach Transform");

        app.add_system(ui_script_event_system);
        app.update().expect("script event system must run");

        let transform = app
            .world()
            .get_component::<Transform>(entity)
            .expect("entity must keep Transform");
        assert!(
            (transform.translation.x - 7.0).abs() < f32::EPSILON,
            "on_event must have run for the relayed `start_game` event; x={}",
            transform.translation.x
        );
    }

    #[test]
    fn ui_script_event_system_applies_save_set_and_save_write_commands() {
        // Confirms this dispatch site applies save commands the same way
        // `scripting_update_system` does (Phase 56 / ADR 0048 §4), through
        // the shared `apply_save_commands` helper.
        let dir = tempfile::tempdir().expect("must create temp dir");
        let mut app = engine_ecs::App::new();
        app.insert_resource(Input::<KeyCode>::new());
        app.insert_resource(SaveData::default());
        app.insert_resource(SaveStore::new(dir.path().to_path_buf()));

        let mut frame = UiEventFrame::new();
        frame.replace(vec!["save_now".to_string()]);
        app.insert_resource(frame);

        let asset_id = AssetId::generate();
        let mut script_engine = ScriptEngine::default();
        script_engine
            .compile(
                &asset_id,
                r#"fn on_event(ctx, event) {
                    if event == "save_now" {
                        ctx.save_set("chapter", 9.0);
                        ctx.save_write(0);
                    }
                }"#,
            )
            .expect("test script must compile");
        app.insert_resource(script_engine);

        app.world_mut()
            .spawn_with(ScriptComponent::new(asset_id))
            .expect("must spawn script entity");

        app.add_system(ui_script_event_system);
        app.update().expect("script event system must run");

        let save_data = app
            .world()
            .get_resource::<SaveData>()
            .expect("SaveData resource must remain present");
        assert_eq!(save_data.get("chapter"), Some(&SaveValue::Number(9.0)));

        let mut store = SaveStore::new(dir.path().to_path_buf());
        let loaded = store
            .read_slot(0)
            .expect("slot 0 file must exist on disk after save_write");
        assert_eq!(loaded.get("chapter"), Some(&SaveValue::Number(9.0)));
    }

    #[test]
    fn ui_script_event_system_does_not_fail_without_a_script_engine() {
        // Most games never insert a `ScriptEngine` resource, but this system
        // is auto-registered by `App::new` for every game (Phase 54 §2). It
        // must no-op rather than fail `ecs.update()`.
        let mut app = engine_ecs::App::new();
        app.insert_resource(Input::<KeyCode>::new());
        let mut frame = UiEventFrame::new();
        frame.replace(vec!["start_game".to_string()]);
        app.insert_resource(frame);

        app.add_system(ui_script_event_system);
        assert!(app.update().is_ok());
    }

    // --- Behavior Tree condition integration (Phase 54 §E) -----------------

    /// Proves the intended `UiEventFrame` → Behavior Tree wiring pattern.
    ///
    /// [`crate::behavior_tree::BehaviorTreeBehaviorRegistry`] conditions are
    /// static lookups set via `set_condition`; a `BehaviorTreeContext`
    /// implementation has no access to the ECS `World` (and therefore no
    /// direct `Res<UiEventFrame>` access) inside `check_condition`. The
    /// supported integration is for a frame-schedule system to read
    /// `UiEventFrame` once and translate it into a registry condition value
    /// before the Behavior Tree tick runs, exactly as this test does.
    #[test]
    fn behavior_tree_condition_reflects_ui_event_frame_contents() {
        use crate::behavior_tree::{
            BehaviorStatus, BehaviorTreeBehaviorRegistry, BehaviorTreeExecutor,
        };
        use engine_authoring::{
            BehaviorTreeNodeKind, CompiledBehaviorNode, CompiledBehaviorTree, GraphId, NodeId,
        };

        let executor = BehaviorTreeExecutor::new(CompiledBehaviorTree {
            source: GraphId::generate(),
            root: CompiledBehaviorNode {
                source: NodeId::generate(),
                kind: BehaviorTreeNodeKind::Root,
                behavior: None,
                children: vec![CompiledBehaviorNode {
                    source: NodeId::generate(),
                    kind: BehaviorTreeNodeKind::Condition,
                    behavior: Some("ui.event.open_menu".to_string()),
                    children: Vec::new(),
                }],
            },
        });

        let mut frame = UiEventFrame::new();
        frame.replace(vec!["open_menu".to_string()]);
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry.set_condition(
            "ui.event.open_menu",
            if frame.contains("open_menu") {
                BehaviorStatus::Success
            } else {
                BehaviorStatus::Failure
            },
        );
        assert_eq!(
            executor.tick(&mut registry).unwrap(),
            BehaviorStatus::Success
        );

        frame.replace(Vec::new());
        registry.set_condition(
            "ui.event.open_menu",
            if frame.contains("open_menu") {
                BehaviorStatus::Success
            } else {
                BehaviorStatus::Failure
            },
        );
        assert_eq!(
            executor.tick(&mut registry).unwrap(),
            BehaviorStatus::Failure
        );
    }

    // --- Scene-placed UI drawing (Phase 54 §4) ------------------------------

    #[test]
    fn draw_ui_document_renders_a_scene_placed_ui_document_ref() {
        let mut world = World::new();
        let entity = world.spawn().expect("must spawn entity");
        world
            .add_component(
                entity,
                UiDocumentRef {
                    asset: AssetId::generate(),
                    document: document_with_button("scene_button"),
                    source_path: None,
                    modified: None,
                },
            )
            .expect("must attach UiDocumentRef");

        let ctx = egui::Context::default();
        let mut last_output = None;
        for _ in 0..2 {
            let raw_input = egui::RawInput::default();
            let output = ctx.run_ui(raw_input, |ui| {
                for entity in world.entities() {
                    if let Some(document_ref) = world.get_component::<UiDocumentRef>(entity) {
                        draw_ui_document(ui.ctx(), &document_ref.document, &world);
                    }
                }
            });
            last_output = Some(output);
        }

        let output = last_output.expect("must have run at least one frame");
        assert!(
            !output.shapes.is_empty(),
            "UiDocumentRef draw must produce shapes"
        );
    }

    // --- Hot reload (Phase 54 §5) -------------------------------------------

    #[test]
    fn ui_document_reload_system_reloads_when_source_file_changes() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let path = dir.path().join("hud.ui.json");
        std::fs::write(
            &path,
            r#"{"schema_version":3,"root":{"id":"root","type":"spacer","size":1.0}}"#,
        )
        .expect("must write initial file");
        let initial_document = load_ui_document(&path).expect("initial document must load");

        let mut app = engine_ecs::App::new();
        app.insert_resource(Time::default());
        app.insert_resource(UiReloadTimer::new());
        let entity = app
            .world_mut()
            .spawn_with(UiDocumentRef {
                asset: AssetId::generate(),
                document: initial_document,
                source_path: Some(path.clone()),
                // A coarse-resolution filesystem clock may not visibly
                // advance mtime within one test run, so the stored value is
                // set explicitly in the past rather than relying on real
                // clock resolution (Phase 54 spec Cautions).
                modified: Some(std::time::UNIX_EPOCH),
            })
            .expect("must spawn UiDocumentRef entity");
        app.add_system(ui_document_reload_system);

        std::fs::write(
            &path,
            r#"{"schema_version":3,"root":{"id":"root","type":"spacer","size":2.0}}"#,
        )
        .expect("must rewrite file");

        if let Some(time) = app.world_mut().get_resource_mut::<Time>() {
            time.delta_seconds = UI_RELOAD_INTERVAL_SECONDS + 0.01;
        }
        app.update().expect("reload system must run");

        let document_ref = app
            .world()
            .get_component::<UiDocumentRef>(entity)
            .expect("component must remain attached");
        assert!(
            matches!(
                &document_ref.document.root.kind,
                UiNodeKind::Spacer { size } if (*size - 2.0).abs() < f32::EPSILON
            ),
            "reload must replace the document with the rewritten file's contents"
        );
        assert_ne!(
            document_ref.modified,
            Some(std::time::UNIX_EPOCH),
            "reload must record the new mtime"
        );
    }

    #[test]
    fn ui_document_reload_system_keeps_previous_document_on_invalid_json() {
        let dir = tempfile::tempdir().expect("must create temp dir");
        let path = dir.path().join("hud.ui.json");
        std::fs::write(
            &path,
            r#"{"schema_version":3,"root":{"id":"root","type":"spacer","size":1.0}}"#,
        )
        .expect("must write initial file");
        let initial_document = load_ui_document(&path).expect("initial document must load");

        let mut app = engine_ecs::App::new();
        app.insert_resource(Time::default());
        app.insert_resource(UiReloadTimer::new());
        let entity = app
            .world_mut()
            .spawn_with(UiDocumentRef {
                asset: AssetId::generate(),
                document: initial_document.clone(),
                source_path: Some(path.clone()),
                modified: Some(std::time::UNIX_EPOCH),
            })
            .expect("must spawn UiDocumentRef entity");
        app.add_system(ui_document_reload_system);

        std::fs::write(&path, "{ not valid json").expect("must rewrite file with invalid JSON");

        if let Some(time) = app.world_mut().get_resource_mut::<Time>() {
            time.delta_seconds = UI_RELOAD_INTERVAL_SECONDS + 0.01;
        }
        app.update().expect("reload system must run");

        let document_ref = app
            .world()
            .get_component::<UiDocumentRef>(entity)
            .expect("component must remain attached");
        assert_eq!(
            document_ref.document, initial_document,
            "invalid JSON must not replace the previous document"
        );
    }

    #[test]
    fn a_scaled_presentation_keeps_a_panel_at_the_same_fraction_of_the_screen() {
        // ADR 0090. A HUD authored for a 1920x1080 screen must cover the same
        // fraction of a Scene View game frame as of the shipped window. While
        // the presented rectangle doubled as the layout screen, this panel kept
        // its authored point size and grew to cover the whole frame as the
        // editor dock shrank.
        let screen = egui::vec2(1920.0, 1080.0);
        let mut document = UiDocument::default();
        document.root.children.push(UiNode {
            id: "block".to_owned(),
            kind: UiNodeKind::Spacer { size: 240.0 },
            children: Vec::new(),
        });

        let measure = |presented: egui::Vec2| -> egui::Vec2 {
            let ctx = egui::Context::default();
            let bindings = UiBindings::new();
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, presented);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                ..Default::default()
            };
            let mut report = UiDocumentDrawReport::default();
            // An `Area` sizes itself from the previous pass, so the recorded
            // rectangle settles after the first frame.
            for _ in 0..3 {
                ctx.begin_pass(input.clone());
                report = draw_ui_document_with_frame(
                    &ctx,
                    &document,
                    UiDrawFrame {
                        bindings: &bindings,
                        events: None,
                        diagnostics: None,
                        viewport: UiViewport::scaled(rect, screen),
                        base_path: None,
                    },
                    UiDocumentDrawOptions::editor_preview(),
                );
                let _ = ctx.end_pass();
            }
            let root = report
                .nodes
                .iter()
                .find(|node| node.node_id == document.root.id)
                .map(|node| node.rect)
                .expect("root panel must be recorded");
            egui::vec2(root.width() / presented.x, root.height() / presented.y)
        };

        let shipped = measure(screen);
        assert!(
            shipped.x > 0.05 && shipped.x < 0.5,
            "test document must cover a distinctive fraction, got {shipped:?}"
        );
        for factor in [0.5, 0.25] {
            let previewed = measure(screen * factor);
            assert!(
                (previewed.x - shipped.x).abs() < 0.01 && (previewed.y - shipped.y).abs() < 0.01,
                "presenting the screen at {factor} must keep the covered fraction                  {shipped:?}, got {previewed:?}"
            );
        }
    }

    #[test]
    fn document_scale_multiplies_the_responsive_and_display_factors() {
        let document = UiDocument {
            reference_resolution: [1920.0, 1080.0],
            scale_policy: UiScalePolicy::ScaleWithViewport {
                match_axis: UiScaleMatch::Height,
                blend: 0.5,
            },
            ..UiDocument::default()
        };
        // A 3840x2160 target screen doubles the responsive scale; presenting
        // that screen in a 960-wide frame quarters the display scale.
        let viewport = UiViewport::scaled(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 540.0)),
            egui::vec2(3840.0, 2160.0),
        );

        assert!((ui_document_scale(&document, viewport) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn constant_pixel_documents_are_unscaled_when_the_game_owns_the_window() {
        let document = UiDocument::default();
        let viewport = UiViewport::direct(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 720.0),
        ));

        assert!((ui_document_scale(&document, viewport) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ui_document_clip_is_limited_to_the_host_viewport() {
        let inherited = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1600.0, 1000.0));
        let viewport = egui::Rect::from_min_max(egui::pos2(250.0, 80.0), egui::pos2(1200.0, 700.0));

        assert_eq!(ui_document_clip_rect(inherited, viewport), viewport);
    }

    #[test]
    fn draw_frame_anchors_bottom_right_panel_to_the_viewport_corner() {
        // Locks the invariant behind ADR 0073: the editor preview drives this
        // frame path, so a bottom-right–anchored root panel must land at the
        // viewport's bottom-right exactly as the runtime overlay places it.
        let ctx = egui::Context::default();
        // `safe_area_padding` defaults to zeros, so the anchored panel reaches
        // the raw viewport corner without an inset.
        let mut document = UiDocument::default();
        if let UiNodeKind::Panel { anchor, .. } = &mut document.root.kind {
            *anchor = Anchor::BottomRight;
        }
        document.root.children.push(UiNode {
            id: "label".to_owned(),
            kind: UiNodeKind::Text {
                content: UiString::Literal("Hi".to_owned()),
                size: 16.0,
                color: [1.0; 4],
            },
            children: Vec::new(),
        });
        let bindings = UiBindings::new();
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let input = egui::RawInput {
            screen_rect: Some(viewport),
            ..Default::default()
        };

        // egui resolves an Area's size the frame after it is first shown, so the
        // anchored rectangle settles on the second pass.
        let mut report = UiDocumentDrawReport::default();
        for _ in 0..2 {
            ctx.begin_pass(input.clone());
            report = draw_ui_document_with_frame(
                &ctx,
                &document,
                UiDrawFrame {
                    bindings: &bindings,
                    events: None,
                    diagnostics: None,
                    viewport: UiViewport::direct(viewport),
                    base_path: None,
                },
                UiDocumentDrawOptions::editor_preview(),
            );
            let _ = ctx.end_pass();
        }

        let root_rect = report
            .nodes
            .iter()
            .find(|node| node.node_id == document.root.id)
            .map(|node| node.rect)
            .expect("root panel must be recorded");
        assert!(
            root_rect.right() >= 760.0 && root_rect.bottom() >= 560.0,
            "bottom-right anchored panel must sit against the viewport corner, got {root_rect:?}"
        );
    }
}
