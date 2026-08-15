//! Declarative UI document data model (Phase 53 / ADR 0046).
//!
//! A UI document (`*.ui.json`) describes a tree of typed nodes that the
//! engine interprets into egui widgets each frame (see
//! `crates/engine/src/ui_document.rs`). This module owns the persisted
//! format: schema version, serde round-trip, and validation with stable
//! diagnostic codes, mirroring [`crate::material_asset`] (ADR 0029).
//!
//! The node model covers anchored panels, text, buttons, spacers, images,
//! progress bars, responsive stacks, grids, overlays, and scroll views.
//! String- and number-valued properties may be literals or named bindings
//! resolved at draw time from a `UiBindings` table; this module owns their
//! persisted representation, not runtime binding resolution.

use crate::diagnostic::Diagnostic;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Schema version for `.ui.json` files.
///
/// Version 2 adds image, progress, responsive container, grid, overlay, and
/// scroll-view nodes. Version 1 documents remain source-compatible and are
/// upgraded in memory when opened.
pub const UI_SCHEMA_VERSION: u32 = 3;

fn default_reference_resolution() -> [f32; 2] {
    [1920.0, 1080.0]
}

/// Axis used to match a reference resolution to the current viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UiScaleMatch {
    /// Match viewport width.
    Width,
    /// Match viewport height.
    Height,
    /// Blend width and height ratios.
    #[default]
    WidthHeight,
}

/// Document-wide responsive scale policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum UiScalePolicy {
    /// Preserve authored values as viewport pixels.
    #[default]
    ConstantPixels,
    /// Scale against the reference resolution.
    ScaleWithViewport {
        /// Viewport dimension used to calculate scale.
        #[serde(default)]
        match_axis: UiScaleMatch,
        /// Width contribution for blended matching, from zero to one.
        #[serde(default = "default_scale_blend")]
        blend: f32,
    },
    /// Scale using physical DPI when the host supplies it.
    ConstantPhysicalSize {
        /// DPI at which authored values have scale one.
        #[serde(default = "default_reference_dpi")]
        reference_dpi: f32,
    },
}

fn default_scale_blend() -> f32 {
    0.5
}

fn default_reference_dpi() -> f32 {
    96.0
}

/// Optional responsive constraints associated with one stable UI node ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct UiElementConstraints {
    /// Minimum logical width and height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_size: Option<[f32; 2]>,
    /// Maximum logical width and height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_size: Option<[f32; 2]>,
    /// Optional width divided by height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<f32>,
    /// Normalized minimum anchor inside the parent rectangle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_min: Option<[f32; 2]>,
    /// Normalized maximum anchor inside the parent rectangle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_max: Option<[f32; 2]>,
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// A declarative UI document: a schema version and a single root node.
///
/// `schema_version` is required and must equal [`UI_SCHEMA_VERSION`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiDocument {
    /// Schema version, always [`UI_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Logical resolution used by viewport and physical scaling policies.
    #[serde(default = "default_reference_resolution")]
    pub reference_resolution: [f32; 2],
    /// Deterministic document-wide viewport scale calculation.
    #[serde(default)]
    pub scale_policy: UiScalePolicy,
    /// Left, top, right, and bottom safe-area padding in logical units.
    #[serde(default)]
    pub safe_area_padding: [f32; 4],
    /// Responsive constraints keyed by stable document-unique node ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub constraints: BTreeMap<String, UiElementConstraints>,
    /// The root node of the UI tree.
    pub root: UiNode,
}

impl Default for UiDocument {
    /// Returns a minimal, valid document: an empty top-left vertical panel
    /// with id `"root"`.
    fn default() -> Self {
        Self {
            schema_version: UI_SCHEMA_VERSION,
            reference_resolution: default_reference_resolution(),
            scale_policy: UiScalePolicy::default(),
            safe_area_padding: [0.0; 4],
            constraints: BTreeMap::new(),
            root: UiNode {
                id: "root".to_string(),
                kind: UiNodeKind::Panel {
                    anchor: UiAnchor::TopLeft,
                    offset_x: 0.0,
                    offset_y: 0.0,
                    layout: UiLayout::Vertical,
                    spacing: UiNodeKind::default_panel_spacing(),
                    padding: UiNodeKind::default_panel_padding(),
                    background: None,
                },
                children: Vec::new(),
            },
        }
    }
}

impl UiDocument {
    /// Parses a `.ui.json` string.
    ///
    /// # Errors
    ///
    /// - [`UiDocumentError::Json`] for malformed JSON.
    /// - [`UiDocumentError::UnsupportedVersion`] when `schema_version` is not
    ///   [`UI_SCHEMA_VERSION`].
    pub fn from_json_str(json: &str) -> Result<Self, UiDocumentError> {
        let document: UiDocument = serde_json::from_str(json).map_err(UiDocumentError::Json)?;
        if document.schema_version != UI_SCHEMA_VERSION {
            return Err(UiDocumentError::UnsupportedVersion {
                found: document.schema_version,
            });
        }
        Ok(document)
    }

    /// Serializes this document to canonical pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialization fails.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Validates this document and returns structured diagnostics.
    ///
    /// An empty result means the document is valid. Checked conditions:
    ///
    /// - `ui.duplicate_node_id` — the same node `id` appears more than once
    ///   in the tree.
    /// - `ui.empty_node_id` — a node has an empty `id`.
    /// - `ui.non_finite_number` — a numeric property is NaN or infinite.
    /// - `ui.empty_event_name` — a [`UiNodeKind::Button`] has an empty
    ///   `event` name.
    /// - `ui.empty_bind_name` — a [`UiString::Bind`] has an empty name.
    pub fn validate(&self) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        if self
            .reference_resolution
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            diagnostics.push(Diagnostic::error(
                "ui.invalid_reference_resolution",
                "UI reference resolution must contain two finite positive values",
            ));
        }
        if self
            .safe_area_padding
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            diagnostics.push(Diagnostic::error(
                "ui.invalid_safe_area",
                "UI safe-area padding must contain finite non-negative values",
            ));
        }
        match self.scale_policy {
            UiScalePolicy::ScaleWithViewport { blend, .. }
                if !blend.is_finite() || !(0.0..=1.0).contains(&blend) =>
            {
                diagnostics.push(Diagnostic::error(
                    "ui.invalid_scale_blend",
                    "UI viewport scale blend must be between zero and one",
                ));
            }
            UiScalePolicy::ConstantPhysicalSize { reference_dpi }
                if !reference_dpi.is_finite() || reference_dpi <= 0.0 =>
            {
                diagnostics.push(Diagnostic::error(
                    "ui.invalid_reference_dpi",
                    "UI physical-size reference DPI must be finite and positive",
                ));
            }
            _ => {}
        }
        let mut seen_ids = BTreeSet::new();
        validate_node(&self.root, &mut seen_ids, &mut diagnostics);
        for (node_id, constraints) in &self.constraints {
            if !seen_ids.contains(node_id.as_str()) {
                diagnostics.push(Diagnostic::warning(
                    "ui.orphaned_constraints",
                    format!("responsive constraints target missing node `{node_id}`"),
                ));
            }
            validate_constraints(node_id, constraints, &mut diagnostics);
        }
        diagnostics
    }

    /// Calculates the scale used by both preview and runtime layout.
    pub fn viewport_scale(&self, viewport: [f32; 2], dpi: Option<f32>) -> f32 {
        let width = viewport[0].max(1.0) / self.reference_resolution[0].max(1.0);
        let height = viewport[1].max(1.0) / self.reference_resolution[1].max(1.0);
        match self.scale_policy {
            UiScalePolicy::ConstantPixels => 1.0,
            UiScalePolicy::ScaleWithViewport {
                match_axis: UiScaleMatch::Width,
                ..
            } => width,
            UiScalePolicy::ScaleWithViewport {
                match_axis: UiScaleMatch::Height,
                ..
            } => height,
            UiScalePolicy::ScaleWithViewport {
                match_axis: UiScaleMatch::WidthHeight,
                blend,
            } => width.powf(1.0 - blend.clamp(0.0, 1.0)) * height.powf(blend.clamp(0.0, 1.0)),
            UiScalePolicy::ConstantPhysicalSize { reference_dpi } => {
                dpi.unwrap_or(reference_dpi).max(1.0) / reference_dpi.max(1.0)
            }
        }
        .clamp(0.01, 100.0)
    }
}

fn validate_constraints(
    node_id: &str,
    constraints: &UiElementConstraints,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let valid_size = |size: [f32; 2]| size.iter().all(|value| value.is_finite() && *value >= 0.0);
    if constraints
        .minimum_size
        .is_some_and(|size| !valid_size(size))
        || constraints
            .maximum_size
            .is_some_and(|size| !valid_size(size))
    {
        diagnostics.push(Diagnostic::error(
            "ui.invalid_size_constraints",
            format!("node `{node_id}` has invalid min/max size constraints"),
        ));
    }
    if constraints
        .aspect_ratio
        .is_some_and(|ratio| !ratio.is_finite() || ratio <= 0.0)
    {
        diagnostics.push(Diagnostic::error(
            "ui.invalid_aspect_constraint",
            format!("node `{node_id}` has an invalid aspect ratio"),
        ));
    }
    let valid_anchor = |anchor: [f32; 2]| {
        anchor
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
    };
    if constraints
        .anchor_min
        .is_some_and(|anchor| !valid_anchor(anchor))
        || constraints
            .anchor_max
            .is_some_and(|anchor| !valid_anchor(anchor))
    {
        diagnostics.push(Diagnostic::error(
            "ui.invalid_anchor_constraints",
            format!("node `{node_id}` has anchors outside zero to one"),
        ));
    }
}

fn validate_node<'a>(
    node: &'a UiNode,
    seen_ids: &mut BTreeSet<&'a str>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if node.id.is_empty() {
        diagnostics.push(Diagnostic::error(
            "ui.empty_node_id",
            "UI node id must not be empty",
        ));
    } else if !seen_ids.insert(node.id.as_str()) {
        diagnostics.push(Diagnostic::error(
            "ui.duplicate_node_id",
            format!("UI node id `{}` is used more than once", node.id),
        ));
    }

    match &node.kind {
        UiNodeKind::Panel {
            offset_x,
            offset_y,
            spacing,
            padding,
            background,
            anchor: _,
            layout: _,
        } => {
            push_non_finite(&node.id, "offset_x", *offset_x, diagnostics);
            push_non_finite(&node.id, "offset_y", *offset_y, diagnostics);
            push_non_finite(&node.id, "spacing", *spacing, diagnostics);
            push_non_finite(&node.id, "padding", *padding, diagnostics);
            if let Some(background) = background {
                for (index, channel) in background.iter().enumerate() {
                    push_non_finite(
                        &node.id,
                        &format!("background[{index}]"),
                        *channel,
                        diagnostics,
                    );
                }
            }
        }
        UiNodeKind::Text {
            content,
            size,
            color,
        } => {
            push_non_finite(&node.id, "size", *size, diagnostics);
            for (index, channel) in color.iter().enumerate() {
                push_non_finite(&node.id, &format!("color[{index}]"), *channel, diagnostics);
            }
            push_empty_bind(&node.id, content, diagnostics);
        }
        UiNodeKind::Button { label, event } => {
            push_empty_bind(&node.id, label, diagnostics);
            if event.is_empty() {
                diagnostics.push(Diagnostic::error(
                    "ui.empty_event_name",
                    format!("Button node `{}` has an empty event name", node.id),
                ));
            }
        }
        UiNodeKind::Spacer { size } => {
            push_non_finite(&node.id, "size", *size, diagnostics);
        }
        UiNodeKind::Image {
            source,
            width,
            height,
            tint,
            nine_slice,
        } => {
            if source.trim().is_empty() {
                diagnostics.push(Diagnostic::error(
                    "ui.empty_image_source",
                    format!("Image node `{}` has an empty source path", node.id),
                ));
            }
            push_non_finite(&node.id, "width", *width, diagnostics);
            push_non_finite(&node.id, "height", *height, diagnostics);
            for (index, channel) in tint.iter().enumerate() {
                push_non_finite(&node.id, &format!("tint[{index}]"), *channel, diagnostics);
            }
            if let Some(border) = nine_slice {
                for (index, value) in border.iter().enumerate() {
                    push_non_finite(
                        &node.id,
                        &format!("nine_slice[{index}]"),
                        *value,
                        diagnostics,
                    );
                    if *value < 0.0 {
                        diagnostics.push(Diagnostic::error(
                            "ui.negative_nine_slice",
                            format!("Image node `{}` has a negative nine-slice border", node.id),
                        ));
                    }
                }
            }
        }
        UiNodeKind::ProgressBar {
            value,
            maximum,
            width,
            height,
            fill,
            background,
            ..
        } => {
            validate_number(&node.id, "value", value, diagnostics);
            validate_number(&node.id, "maximum", maximum, diagnostics);
            push_non_finite(&node.id, "width", *width, diagnostics);
            push_non_finite(&node.id, "height", *height, diagnostics);
            for (field, color) in [("fill", fill), ("background", background)] {
                for (index, channel) in color.iter().enumerate() {
                    push_non_finite(
                        &node.id,
                        &format!("{field}[{index}]"),
                        *channel,
                        diagnostics,
                    );
                }
            }
        }
        UiNodeKind::Stack {
            spacing,
            padding,
            background,
            ..
        }
        | UiNodeKind::Grid {
            spacing,
            padding,
            background,
            ..
        } => {
            push_non_finite(&node.id, "spacing", *spacing, diagnostics);
            push_non_finite(&node.id, "padding", *padding, diagnostics);
            if let Some(background) = background {
                for (index, channel) in background.iter().enumerate() {
                    push_non_finite(
                        &node.id,
                        &format!("background[{index}]"),
                        *channel,
                        diagnostics,
                    );
                }
            }
            if matches!(&node.kind, UiNodeKind::Grid { columns: 0, .. }) {
                diagnostics.push(Diagnostic::error(
                    "ui.zero_grid_columns",
                    format!("Grid node `{}` must have at least one column", node.id),
                ));
            }
        }
        UiNodeKind::Overlay {
            padding,
            background,
        } => {
            push_non_finite(&node.id, "padding", *padding, diagnostics);
            if let Some(background) = background {
                for (index, channel) in background.iter().enumerate() {
                    push_non_finite(
                        &node.id,
                        &format!("background[{index}]"),
                        *channel,
                        diagnostics,
                    );
                }
            }
        }
        UiNodeKind::ScrollView {
            max_width,
            max_height,
            ..
        } => {
            if let Some(value) = max_width {
                push_non_finite(&node.id, "max_width", *value, diagnostics);
            }
            if let Some(value) = max_height {
                push_non_finite(&node.id, "max_height", *value, diagnostics);
            }
        }
    }

    for child in &node.children {
        validate_node(child, seen_ids, diagnostics);
    }
}

fn validate_number(
    node_id: &str,
    field: &str,
    value: &UiNumber,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match value {
        UiNumber::Literal(value) => push_non_finite(node_id, field, *value, diagnostics),
        UiNumber::Bind(name) if name.trim().is_empty() => diagnostics.push(Diagnostic::error(
            "ui.empty_bind_name",
            format!("UI node `{node_id}` field `{field}` has an empty $bind name"),
        )),
        UiNumber::Bind(_) => {}
    }
}

fn push_non_finite(node_id: &str, field: &str, value: f32, diagnostics: &mut Vec<Diagnostic>) {
    if !value.is_finite() {
        diagnostics.push(Diagnostic::error(
            "ui.non_finite_number",
            format!("UI node `{node_id}` field `{field}` must be a finite number, found {value}"),
        ));
    }
}

fn push_empty_bind(node_id: &str, value: &UiString, diagnostics: &mut Vec<Diagnostic>) {
    if let UiString::Bind(name) = value
        && name.is_empty()
    {
        diagnostics.push(Diagnostic::error(
            "ui.empty_bind_name",
            format!("UI node `{node_id}` has an empty $bind name"),
        ));
    }
}

// ---------------------------------------------------------------------------
// Node
// ---------------------------------------------------------------------------

/// One node in a [`UiDocument`] tree.
///
/// `id` MUST be unique within the whole document; it is used for egui `Id`
/// generation and for identifying nodes in diagnostics. JSON shape:
///
/// ```json
/// {"id": "hud", "type": "panel", "anchor": "top_left", "children": []}
/// ```
///
/// The `type` tag and its per-kind fields are flattened into the same JSON
/// object as `id` and `children` (see [`UiNodeKind`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiNode {
    /// Document-unique identifier for this node.
    pub id: String,
    /// The node kind and its kind-specific properties.
    #[serde(flatten)]
    pub kind: UiNodeKind,
    /// Nested child nodes, drawn according to the parent's layout rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<UiNode>,
}

/// The kind-specific data of a [`UiNode`], internally tagged by a stable
/// snake-case `type` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiNodeKind {
    /// An anchored container that lays out its children vertically or
    /// horizontally.
    Panel {
        /// Which corner or edge of the viewport this panel is positioned
        /// relative to.
        #[serde(default)]
        anchor: UiAnchor,
        /// Horizontal offset in pixels from the anchor point.
        #[serde(default)]
        offset_x: f32,
        /// Vertical offset in pixels from the anchor point.
        #[serde(default)]
        offset_y: f32,
        /// Whether children stack vertically or horizontally.
        #[serde(default)]
        layout: UiLayout,
        /// Gap in pixels between consecutive children.
        #[serde(default = "UiNodeKind::default_panel_spacing")]
        spacing: f32,
        /// Inner margin in pixels between the panel edge and its children.
        #[serde(default = "UiNodeKind::default_panel_padding")]
        padding: f32,
        /// Optional linear RGBA background fill. `None` means transparent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<[f32; 4]>,
    },
    /// A text label, optionally bound to a runtime value.
    Text {
        /// The text to display, either a literal or a named binding.
        content: UiString,
        /// Font size in points.
        #[serde(default = "UiNodeKind::default_text_size")]
        size: f32,
        /// Linear RGBA text color.
        #[serde(default = "UiNodeKind::default_text_color")]
        color: [f32; 4],
    },
    /// A clickable widget that pushes a named event into `UiEvents` when
    /// clicked.
    Button {
        /// The button's visible label, either a literal or a named binding.
        label: UiString,
        /// The event name pushed into `UiEvents` on click. Functions are
        /// never serialized (ADR 0046 §5); this is a data name only.
        event: String,
    },
    /// A fixed-size gap between siblings.
    Spacer {
        /// The gap size in pixels.
        #[serde(default = "UiNodeKind::default_spacer_size")]
        size: f32,
    },
    /// A raster image with optional nine-slice borders.
    Image {
        /// Project-relative or package-relative image path.
        source: String,
        /// Requested layout width in logical pixels.
        #[serde(default = "UiNodeKind::default_image_width")]
        width: f32,
        /// Requested layout height in logical pixels.
        #[serde(default = "UiNodeKind::default_image_height")]
        height: f32,
        /// Linear RGBA tint multiplied with the source image.
        #[serde(default = "UiNodeKind::default_text_color")]
        tint: [f32; 4],
        /// Optional left/top/right/bottom fixed borders in source pixels.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nine_slice: Option<[f32; 4]>,
    },
    /// A horizontal value bar suitable for health, stamina, and timers.
    ProgressBar {
        /// Current value, either literal or supplied by a named binding.
        value: UiNumber,
        /// Full-scale value, either literal or supplied by a named binding.
        maximum: UiNumber,
        /// Desired width in logical pixels.
        #[serde(default = "UiNodeKind::default_progress_width")]
        width: f32,
        /// Desired height in logical pixels.
        #[serde(default = "UiNodeKind::default_progress_height")]
        height: f32,
        /// Linear RGBA fill color.
        #[serde(default = "UiNodeKind::default_progress_fill")]
        fill: [f32; 4],
        /// Linear RGBA track color.
        #[serde(default = "UiNodeKind::default_progress_background")]
        background: [f32; 4],
        /// Whether the resolved numeric value is drawn over the bar.
        #[serde(default)]
        show_label: bool,
    },
    /// A responsive vertical or horizontal child stack.
    Stack {
        /// Child flow direction.
        #[serde(default)]
        direction: UiLayout,
        /// Gap between adjacent children.
        #[serde(default = "UiNodeKind::default_panel_spacing")]
        spacing: f32,
        /// Inner margin around all children.
        #[serde(default = "UiNodeKind::default_panel_padding")]
        padding: f32,
        /// Optional linear RGBA background.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<[f32; 4]>,
    },
    /// A deterministic row-major grid container.
    Grid {
        /// Number of columns before wrapping to the next row.
        #[serde(default = "UiNodeKind::default_grid_columns")]
        columns: usize,
        /// Horizontal and vertical cell gap.
        #[serde(default = "UiNodeKind::default_panel_spacing")]
        spacing: f32,
        /// Inner margin around the grid.
        #[serde(default = "UiNodeKind::default_panel_padding")]
        padding: f32,
        /// Optional linear RGBA background.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<[f32; 4]>,
    },
    /// A container that draws every child into the same available rectangle.
    Overlay {
        /// Inner margin around overlaid children.
        #[serde(default)]
        padding: f32,
        /// Optional linear RGBA background.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        background: Option<[f32; 4]>,
    },
    /// A clipping container with optional horizontal and vertical scrolling.
    ScrollView {
        /// Enables horizontal scrolling.
        #[serde(default)]
        horizontal: bool,
        /// Enables vertical scrolling.
        #[serde(default = "UiNodeKind::default_true")]
        vertical: bool,
        /// Optional maximum viewport width.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_width: Option<f32>,
        /// Optional maximum viewport height.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_height: Option<f32>,
    },
}

impl UiNodeKind {
    fn default_panel_spacing() -> f32 {
        4.0
    }

    fn default_panel_padding() -> f32 {
        8.0
    }

    fn default_text_size() -> f32 {
        16.0
    }

    fn default_text_color() -> [f32; 4] {
        [1.0, 1.0, 1.0, 1.0]
    }

    fn default_spacer_size() -> f32 {
        8.0
    }

    fn default_image_width() -> f32 {
        128.0
    }

    fn default_image_height() -> f32 {
        128.0
    }

    fn default_progress_width() -> f32 {
        200.0
    }

    fn default_progress_height() -> f32 {
        18.0
    }

    fn default_progress_fill() -> [f32; 4] {
        [0.2, 0.8, 0.3, 1.0]
    }

    fn default_progress_background() -> [f32; 4] {
        [0.1, 0.1, 0.1, 0.8]
    }

    fn default_grid_columns() -> usize {
        2
    }

    fn default_true() -> bool {
        true
    }

    /// Returns whether nodes of this kind accept children.
    pub fn is_container(&self) -> bool {
        matches!(
            self,
            Self::Panel { .. }
                | Self::Stack { .. }
                | Self::Grid { .. }
                | Self::Overlay { .. }
                | Self::ScrollView { .. }
        )
    }
}

/// The nine anchor points a [`UiNodeKind::Panel`] can be positioned
/// relative to within the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiAnchor {
    /// The viewport's top-left corner.
    #[default]
    TopLeft,
    /// The horizontal center of the viewport's top edge.
    TopCenter,
    /// The viewport's top-right corner.
    TopRight,
    /// The vertical center of the viewport's left edge.
    CenterLeft,
    /// The center of the viewport.
    Center,
    /// The vertical center of the viewport's right edge.
    CenterRight,
    /// The viewport's bottom-left corner.
    BottomLeft,
    /// The horizontal center of the viewport's bottom edge.
    BottomCenter,
    /// The viewport's bottom-right corner.
    BottomRight,
}

/// The stacking direction for a [`UiNodeKind::Panel`]'s children.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiLayout {
    /// Children stack top to bottom.
    #[default]
    Vertical,
    /// Children stack left to right.
    Horizontal,
}

// ---------------------------------------------------------------------------
// UiString: literal or named binding
// ---------------------------------------------------------------------------

/// A string-valued node property that is either a literal value or a named
/// binding resolved at draw time from a `UiBindings` table (ADR 0046 §4).
///
/// JSON forms:
///
/// - `"hello"` deserializes as `UiString::Literal("hello".to_string())`.
/// - `{"$bind": "score"}` deserializes as
///   `UiString::Bind("score".to_string())`.
///
/// Serialization and deserialization are implemented by hand (rather than
/// derived) so the public shape stays a plain two-variant enum while the
/// `Bind` variant's JSON uses the `$bind` map key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiString {
    /// A fixed string value, used as-is.
    Literal(String),
    /// A named lookup into a runtime `UiBindings` table.
    Bind(String),
}

/// A numeric UI property that is either literal or resolved from bindings.
#[derive(Debug, Clone, PartialEq)]
pub enum UiNumber {
    /// Fixed numeric value.
    Literal(f32),
    /// Named runtime binding.
    Bind(String),
}

impl Serialize for UiNumber {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Literal(value) => serializer.serialize_f32(*value),
            Self::Bind(name) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$bind", name)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for UiNumber {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Literal(f32),
            Bind {
                #[serde(rename = "$bind")]
                bind: String,
            },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Literal(value) => Ok(Self::Literal(value)),
            Repr::Bind { bind } => Ok(Self::Bind(bind)),
        }
    }
}

impl Serialize for UiString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            UiString::Literal(text) => serializer.serialize_str(text),
            UiString::Bind(name) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$bind", name)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for UiString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Literal(String),
            Bind {
                #[serde(rename = "$bind")]
                bind: String,
            },
        }

        match Repr::deserialize(deserializer)? {
            Repr::Literal(text) => Ok(UiString::Literal(text)),
            Repr::Bind { bind } => Ok(UiString::Bind(bind)),
        }
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Describes why a [`UiDocument`] operation failed.
#[derive(Debug)]
pub enum UiDocumentError {
    /// The JSON could not be parsed.
    Json(serde_json::Error),
    /// The document uses a schema version newer than this build supports.
    UnsupportedVersion {
        /// The version number found in the document.
        found: u32,
    },
}

impl fmt::Display for UiDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "UI document JSON error: {e}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "UI document schema_version {found} is not supported \
                 (max: {UI_SCHEMA_VERSION})"
            ),
        }
    }
}

impl std::error::Error for UiDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::UnsupportedVersion { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str, kind: UiNodeKind) -> UiNode {
        UiNode {
            id: id.to_string(),
            kind,
            children: Vec::new(),
        }
    }

    #[test]
    fn default_document_validates_clean() {
        let doc = UiDocument::default();
        assert!(doc.validate().is_empty());
        assert_eq!(doc.schema_version, UI_SCHEMA_VERSION);
    }

    #[test]
    fn panel_node_roundtrips_through_json() {
        let node = leaf(
            "hud",
            UiNodeKind::Panel {
                anchor: UiAnchor::BottomRight,
                offset_x: 4.0,
                offset_y: -4.0,
                layout: UiLayout::Horizontal,
                spacing: 6.0,
                padding: 10.0,
                background: Some([0.1, 0.2, 0.3, 0.4]),
            },
        );
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: node.clone(),
            ..UiDocument::default()
        };
        let json = doc.to_json_string().expect("must serialize");
        let loaded = UiDocument::from_json_str(&json).expect("must parse");
        assert_eq!(loaded.root, node);
    }

    #[test]
    fn text_node_roundtrips_through_json() {
        let node = leaf(
            "label",
            UiNodeKind::Text {
                content: UiString::Literal("Hello".to_string()),
                size: 22.0,
                color: [0.9, 0.8, 0.7, 1.0],
            },
        );
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: node.clone(),
            ..UiDocument::default()
        };
        let json = doc.to_json_string().expect("must serialize");
        let loaded = UiDocument::from_json_str(&json).expect("must parse");
        assert_eq!(loaded.root, node);
    }

    #[test]
    fn button_node_roundtrips_through_json() {
        let node = leaf(
            "start_button",
            UiNodeKind::Button {
                label: UiString::Bind("start_label".to_string()),
                event: "start_game".to_string(),
            },
        );
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: node.clone(),
            ..UiDocument::default()
        };
        let json = doc.to_json_string().expect("must serialize");
        let loaded = UiDocument::from_json_str(&json).expect("must parse");
        assert_eq!(loaded.root, node);
    }

    #[test]
    fn spacer_node_roundtrips_through_json() {
        let node = leaf("gap", UiNodeKind::Spacer { size: 12.0 });
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: node.clone(),
            ..UiDocument::default()
        };
        let json = doc.to_json_string().expect("must serialize");
        let loaded = UiDocument::from_json_str(&json).expect("must parse");
        assert_eq!(loaded.root, node);
    }

    #[test]
    fn ui_string_literal_serializes_as_plain_json_string() {
        let value = UiString::Literal("hello".to_string());
        let json = serde_json::to_string(&value).expect("must serialize");
        assert_eq!(json, "\"hello\"");
        let loaded: UiString = serde_json::from_str(&json).expect("must parse");
        assert_eq!(loaded, value);
    }

    #[test]
    fn ui_string_bind_serializes_as_bind_object() {
        let value = UiString::Bind("score".to_string());
        let json = serde_json::to_string(&value).expect("must serialize");
        assert_eq!(json, "{\"$bind\":\"score\"}");
        let loaded: UiString = serde_json::from_str(&json).expect("must parse");
        assert_eq!(loaded, value);
    }

    #[test]
    fn from_json_str_requires_a_schema_version() {
        let json = r#"{"root": {"id": "root", "type": "spacer", "size": 8.0}}"#;
        assert!(matches!(
            UiDocument::from_json_str(json),
            Err(UiDocumentError::Json(_))
        ));
    }

    #[test]
    fn from_json_str_rejects_a_superseded_version() {
        let json =
            r#"{"schema_version": 1, "root": {"id": "root", "type": "spacer", "size": 8.0}}"#;
        assert!(matches!(
            UiDocument::from_json_str(json),
            Err(UiDocumentError::UnsupportedVersion { found: 1 })
        ));
    }

    #[test]
    fn from_json_str_rejects_unsupported_version() {
        let json =
            r#"{"schema_version": 4, "root": {"id": "root", "type": "spacer", "size": 8.0}}"#;
        assert!(matches!(
            UiDocument::from_json_str(json),
            Err(UiDocumentError::UnsupportedVersion { found: 4 })
        ));
    }

    #[test]
    fn version_two_visual_nodes_roundtrip() {
        let document = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: UiNode {
                id: "root".into(),
                kind: UiNodeKind::Stack {
                    direction: UiLayout::Vertical,
                    spacing: 6.0,
                    padding: 10.0,
                    background: Some([0.0, 0.0, 0.0, 0.5]),
                },
                children: vec![
                    leaf(
                        "portrait",
                        UiNodeKind::Image {
                            source: "assets/ui/portrait.png".into(),
                            width: 96.0,
                            height: 96.0,
                            tint: [1.0; 4],
                            nine_slice: Some([8.0; 4]),
                        },
                    ),
                    leaf(
                        "health",
                        UiNodeKind::ProgressBar {
                            value: UiNumber::Bind("player_hp".into()),
                            maximum: UiNumber::Bind("player_max_hp".into()),
                            width: 240.0,
                            height: 20.0,
                            fill: [0.8, 0.1, 0.1, 1.0],
                            background: [0.1, 0.1, 0.1, 0.8],
                            show_label: true,
                        },
                    ),
                ],
            },
            ..UiDocument::default()
        };
        let json = document.to_json_string().expect("serialize v2 UI");
        let loaded = UiDocument::from_json_str(&json).expect("parse v2 UI");
        assert_eq!(loaded, document);
        assert!(loaded.validate().is_empty());
    }

    #[test]
    fn duplicate_node_id_is_reported() {
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: UiNode {
                id: "root".to_string(),
                kind: UiNodeKind::Panel {
                    anchor: UiAnchor::TopLeft,
                    offset_x: 0.0,
                    offset_y: 0.0,
                    layout: UiLayout::Vertical,
                    spacing: 4.0,
                    padding: 8.0,
                    background: None,
                },
                children: vec![
                    leaf("child", UiNodeKind::Spacer { size: 1.0 }),
                    leaf("child", UiNodeKind::Spacer { size: 1.0 }),
                ],
            },
            ..UiDocument::default()
        };
        let diagnostics = doc.validate();
        assert!(diagnostics.iter().any(|d| d.code == "ui.duplicate_node_id"));
    }

    #[test]
    fn empty_node_id_is_reported() {
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: leaf("", UiNodeKind::Spacer { size: 1.0 }),
            ..UiDocument::default()
        };
        let diagnostics = doc.validate();
        assert!(diagnostics.iter().any(|d| d.code == "ui.empty_node_id"));
    }

    #[test]
    fn non_finite_number_is_reported() {
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: leaf("gap", UiNodeKind::Spacer { size: f32::NAN }),
            ..UiDocument::default()
        };
        let diagnostics = doc.validate();
        assert!(diagnostics.iter().any(|d| d.code == "ui.non_finite_number"));
    }

    #[test]
    fn empty_event_name_is_reported() {
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: leaf(
                "button",
                UiNodeKind::Button {
                    label: UiString::Literal("Go".to_string()),
                    event: String::new(),
                },
            ),
            ..UiDocument::default()
        };
        let diagnostics = doc.validate();
        assert!(diagnostics.iter().any(|d| d.code == "ui.empty_event_name"));
    }

    #[test]
    fn empty_bind_name_is_reported() {
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: leaf(
                "label",
                UiNodeKind::Text {
                    content: UiString::Bind(String::new()),
                    size: 16.0,
                    color: [1.0, 1.0, 1.0, 1.0],
                },
            ),
            ..UiDocument::default()
        };
        let diagnostics = doc.validate();
        assert!(diagnostics.iter().any(|d| d.code == "ui.empty_bind_name"));
    }

    #[test]
    fn valid_document_with_nested_children_produces_no_diagnostics() {
        let doc = UiDocument {
            schema_version: UI_SCHEMA_VERSION,
            root: UiNode {
                id: "root".to_string(),
                kind: UiNodeKind::Panel {
                    anchor: UiAnchor::TopLeft,
                    offset_x: 0.0,
                    offset_y: 0.0,
                    layout: UiLayout::Vertical,
                    spacing: 4.0,
                    padding: 8.0,
                    background: None,
                },
                children: vec![
                    leaf(
                        "title",
                        UiNodeKind::Text {
                            content: UiString::Literal("Score".to_string()),
                            size: 18.0,
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                    ),
                    leaf(
                        "score",
                        UiNodeKind::Text {
                            content: UiString::Bind("score".to_string()),
                            size: 18.0,
                            color: [1.0, 1.0, 1.0, 1.0],
                        },
                    ),
                    leaf(
                        "start",
                        UiNodeKind::Button {
                            label: UiString::Literal("Start".to_string()),
                            event: "start_game".to_string(),
                        },
                    ),
                ],
            },
            ..UiDocument::default()
        };
        assert!(doc.validate().is_empty());
    }
}
