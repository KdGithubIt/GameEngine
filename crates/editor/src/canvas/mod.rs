//! egui painter-backed graph canvas.

use crate::geometry::fallback_position_for_index;
use crate::session::{EditorSession, GraphNodeInsertKind};
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2 as EguiVec2,
};
use engine_authoring::{Edge, EdgeId, Node, NodeId, Value, Vec2};
use std::collections::BTreeMap;

const NODE_SIZE: EguiVec2 = EguiVec2::new(170.0, 72.0);
/// Empty graph-space padding kept between automatically placed nodes.
const NODE_PLACEMENT_GAP: f64 = 30.0;
/// Vertical alternatives checked before advancing to the next column.
const NODE_PLACEMENT_ROWS_PER_COLUMN: u32 = 8;
const CANVAS_MARGIN: f32 = 40.0;
const EDGE_HIT_RADIUS: f32 = 10.0;
const TRANSITION_BADGE_RADIUS: f32 = 8.0;
const TRANSITION_LABEL_OFFSET: f32 = 26.0;
const CURVE_SAMPLE_COUNT: usize = 24;
const MAX_CANVAS_LABEL_CHARACTERS: usize = 36;
/// Horizontal padding between a node border and its text.
const NODE_TEXT_PADDING: f32 = 10.0;
/// Row width reserved for the top-right pin indicator.
const PIN_INDICATOR_WIDTH: f32 = 30.0;
const TRANSITION_HANDLE_RADIUS: f32 = 7.0;

/// Transient graph canvas state.
///
/// This state is editor-local and is never persisted into `GraphView`.
#[derive(Default)]
pub struct GraphCanvasState {
    drag: Option<DragState>,
    connect_drag: Option<NodeId>,
    context_edge: Option<EdgeId>,
    /// Graph-space point where the empty-canvas context menu was opened.
    context_add_position: Option<Vec2>,
    /// Graph-space center of the most recently painted canvas.
    visible_center_graph: Option<Vec2>,
    /// Screen offset applied to every node while painting and hit testing.
    pan: EguiVec2,
    /// Whether a middle-button pan gesture is currently held.
    panning: bool,
    /// Pending request to pan until every node is visible.
    frame_all_requested: bool,
}

impl GraphCanvasState {
    /// Asks the next canvas frame to pan until every node is visible.
    ///
    /// The canvas rectangle is known only while the canvas is being drawn, so
    /// callers outside it record the request instead of computing the offset.
    pub fn request_frame_all(&mut self) {
        self.frame_all_requested = true;
    }

    /// Returns the center of the visible canvas in persisted graph space.
    pub fn visible_center_graph(&self) -> Option<Vec2> {
        self.visible_center_graph
    }
}

/// Screen placement of the graph's coordinate origin.
///
/// Panning moves only this origin. Node positions stay in graph space, so
/// scrolling the view can never be mistaken for a layout edit.
#[derive(Clone, Copy)]
struct CanvasView {
    origin: Pos2,
}

impl CanvasView {
    fn new(canvas_rect: Rect, pan: EguiVec2) -> Self {
        Self {
            origin: canvas_rect.left_top() + EguiVec2::splat(CANVAS_MARGIN) + pan,
        }
    }

    /// Screen position of a graph-space point.
    fn to_screen(self, position: Vec2) -> Pos2 {
        self.origin + EguiVec2::new(position.x as f32, position.y as f32)
    }

    /// Graph-space position represented by a screen-space point.
    fn to_graph(self, position: Pos2) -> Vec2 {
        let offset = position - self.origin;
        Vec2::new(f64::from(offset.x), f64::from(offset.y))
    }

    /// Screen rectangle covered by a node at the given graph-space position.
    fn node_rect(self, position: Vec2) -> Rect {
        Rect::from_min_size(self.to_screen(position), NODE_SIZE)
    }
}

/// Chooses a readable graph-space position for a newly inserted node.
///
/// An explicit canvas position wins. Toolbar insertion otherwise starts to the
/// right of the selected node, then falls back to the visible canvas center.
/// Every candidate is checked against the current persisted/fallback layout.
pub fn next_node_position(
    session: &EditorSession,
    requested: Option<Vec2>,
    visible_center: Option<Vec2>,
) -> Vec2 {
    let positions = node_positions(session);
    let selected_preferred = session
        .graph_view()
        .and_then(|view| view.selection.nodes.iter().next())
        .and_then(|node| positions.get(node))
        .map(|position| Vec2::new(position.x + node_horizontal_step(), position.y));
    let preferred = requested
        .or(selected_preferred)
        .or(visible_center)
        .unwrap_or_else(|| Vec2::new(0.0, 0.0));
    find_available_node_position(&positions, preferred)
}

fn node_horizontal_step() -> f64 {
    f64::from(NODE_SIZE.x) + NODE_PLACEMENT_GAP
}

fn node_vertical_step() -> f64 {
    f64::from(NODE_SIZE.y) + NODE_PLACEMENT_GAP
}

fn find_available_node_position(positions: &BTreeMap<NodeId, Vec2>, preferred: Vec2) -> Vec2 {
    let mut column = 0_u32;
    loop {
        let x = preferred.x + f64::from(column) * node_horizontal_step();
        let centered = Vec2::new(x, preferred.y);
        if node_position_is_available(positions, &centered) {
            return centered;
        }
        for distance in 1..=NODE_PLACEMENT_ROWS_PER_COLUMN {
            for direction in [1.0, -1.0] {
                let candidate = Vec2::new(
                    x,
                    preferred.y + direction * f64::from(distance) * node_vertical_step(),
                );
                if node_position_is_available(positions, &candidate) {
                    return candidate;
                }
            }
        }
        column += 1;
    }
}

fn node_position_is_available(positions: &BTreeMap<NodeId, Vec2>, candidate: &Vec2) -> bool {
    positions.values().all(|position| {
        (position.x - candidate.x).abs() >= node_horizontal_step()
            || (position.y - candidate.y).abs() >= node_vertical_step()
    })
}

struct DragState {
    node: NodeId,
    start_position: Vec2,
    accumulated_offset: EguiVec2,
}

impl DragState {
    /// Returns the graph-space position represented by the current gesture.
    fn position(&self) -> Vec2 {
        Vec2::new(
            self.start_position.x + f64::from(self.accumulated_offset.x),
            self.start_position.y + f64::from(self.accumulated_offset.y),
        )
    }
}

/// Replaces a stale layout position with the gesture's final preview position.
fn apply_released_preview(preview_position: &mut Vec2, drag: &DragState) -> Vec2 {
    let position = drag.position();
    *preview_position = position;
    position
}

/// User action produced by the graph canvas.
pub enum GraphCanvasAction {
    /// A node was clicked.
    NodeClicked {
        /// Clicked node.
        node: NodeId,
    },
    /// A node drag was released at the given graph-space position.
    NodeDragReleased {
        /// Dragged node.
        node: NodeId,
        /// New graph-space node position.
        position: Vec2,
    },
    /// A node kind was chosen from the empty-canvas context menu.
    AddNode {
        /// Schema-backed node kind for the active graph domain.
        kind: GraphNodeInsertKind,
        /// Preferred graph-space location derived from the context-menu click.
        position: Vec2,
    },
    /// A semantic edge was clicked.
    EdgeClicked {
        /// Clicked edge.
        edge: EdgeId,
    },
    /// The selected node should become the source of the next connection.
    ConnectFrom {
        /// Connection source node.
        node: NodeId,
    },
    /// A transition handle was dragged directly between two nodes.
    ConnectNodes {
        /// Source node where the handle drag began.
        source: NodeId,
        /// Target node under the released pointer.
        target: NodeId,
    },
    /// The node's persisted pin state should change.
    SetPinned {
        /// Node whose presentation state changes.
        node: NodeId,
        /// New pin state.
        pinned: bool,
    },
    /// The node should be removed through the semantic command path.
    DeleteNode {
        /// Node to delete.
        node: NodeId,
    },
    /// The selected semantic edge should be removed.
    DeleteEdge {
        /// Edge to delete.
        edge: EdgeId,
    },
    /// The graph should run the existing incremental layout operation.
    ApplyIncrementalLayout,
}

/// Draws the graph canvas and returns the user actions produced this frame.
pub fn show_graph_canvas(
    ui: &mut egui::Ui,
    session: &EditorSession,
    state: &mut GraphCanvasState,
    pending_connect: Option<&NodeId>,
) -> Vec<GraphCanvasAction> {
    let desired_size = ui.available_size();
    let (canvas_rect, canvas_response) = ui.allocate_exact_size(desired_size, Sense::click());
    let painter = ui.painter_at(canvas_rect);
    painter.rect_filled(canvas_rect, 0.0, Color32::from_rgb(24, 27, 31));
    painter.rect_stroke(
        canvas_rect,
        0.0,
        Stroke::new(1.0_f32, Color32::from_rgb(58, 64, 72)),
        StrokeKind::Inside,
    );

    // Process node interaction before painting edges so all graph elements can
    // use the same-frame drag preview position.
    let layout_positions = node_positions(session);
    let mut preview_positions = layout_positions.clone();
    if std::mem::take(&mut state.frame_all_requested) {
        state.pan = frame_all_pan(canvas_rect, &layout_positions);
    }
    apply_canvas_pan(ui, state, canvas_rect);
    let canvas_view = CanvasView::new(canvas_rect, state.pan);
    state.visible_center_graph = Some(canvas_view.to_graph(canvas_rect.center()));
    let hovered_edge = ui
        .input(|input| input.pointer.hover_pos())
        .and_then(|pointer| {
            closest_edge_at(
                canvas_view,
                session,
                &layout_positions,
                pointer,
                EDGE_HIT_RADIUS,
            )
        });
    let mut actions = Vec::new();
    if canvas_response.clicked()
        && let Some(edge) = canvas_response.interact_pointer_pos().and_then(|pointer| {
            closest_edge_at(
                canvas_view,
                session,
                &layout_positions,
                pointer,
                EDGE_HIT_RADIUS,
            )
        }) {
            actions.push(GraphCanvasAction::EdgeClicked { edge });
        }

    if canvas_response.secondary_clicked() {
        state.context_add_position = canvas_response
            .interact_pointer_pos()
            .map(|pointer| canvas_view.to_graph(pointer));
        state.context_edge = canvas_response.interact_pointer_pos().and_then(|pointer| {
            closest_edge_at(
                canvas_view,
                session,
                &layout_positions,
                pointer,
                EDGE_HIT_RADIUS,
            )
        });
    }
    let context_edge = state.context_edge.clone();
    let context_add_position = state
        .context_add_position
        .unwrap_or_else(|| canvas_view.to_graph(canvas_rect.center()));
    canvas_response.context_menu(|ui| {
        if let Some(edge) = context_edge.as_ref() {
            let label = if session.is_animation_graph() {
                "Delete Transition"
            } else {
                "Delete Edge"
            };
            if ui.button(label).clicked() {
                actions.push(GraphCanvasAction::DeleteEdge { edge: edge.clone() });
                state.context_edge = None;
                ui.close();
            }
            return;
        }
        ui.menu_button("Add Node", |ui| {
            for kind in session.available_graph_node_kinds() {
                if ui.button(kind.label()).clicked() {
                    actions.push(GraphCanvasAction::AddNode {
                        kind,
                        position: context_add_position,
                    });
                    state.context_add_position = None;
                    ui.close();
                }
            }
        });
        if ui.button("Incremental Layout").clicked() {
            actions.push(GraphCanvasAction::ApplyIncrementalLayout);
            ui.close();
        }
        ui.separator();
        // A node dragged past the canvas edge can no longer be clicked, so
        // view recovery must not depend on finding it first.
        if ui
            .button("Frame All Nodes")
            .on_hover_text("Pan the view until every node is visible again")
            .clicked()
        {
            state.frame_all_requested = true;
            ui.close();
        }
        if ui
            .button("Reset View")
            .on_hover_text("Return the view to the graph origin")
            .clicked()
        {
            state.pan = EguiVec2::ZERO;
            ui.close();
        }
    });
    for node_id in session.graph().nodes.keys() {
        let Some(position) = layout_positions.get(node_id).copied() else {
            continue;
        };
        let rect = canvas_view.node_rect(position);
        let id = ui.make_persistent_id(("graph_node", node_id.as_str()));
        let hover_title = session
            .graph()
            .nodes
            .get(node_id)
            .map(node_title)
            .unwrap_or_else(|| "Missing node".to_owned());
        let response = ui
            .interact(rect, id, Sense::click_and_drag())
            .on_hover_text(format!("{hover_title}\n{}", node_id.as_str()));

        let pinned = session
            .graph_view()
            .and_then(|view| view.nodes.get(node_id))
            .is_some_and(|layout| layout.pinned);
        response.context_menu(|ui| {
            if ui.button("Connect From").clicked() {
                actions.push(GraphCanvasAction::ConnectFrom {
                    node: node_id.clone(),
                });
                ui.close();
            }
            if ui.button(if pinned { "Unpin" } else { "Pin" }).clicked() {
                actions.push(GraphCanvasAction::SetPinned {
                    node: node_id.clone(),
                    pinned: !pinned,
                });
                ui.close();
            }
            ui.separator();
            if ui.button("Delete").clicked() {
                actions.push(GraphCanvasAction::DeleteNode {
                    node: node_id.clone(),
                });
                ui.close();
            }
        });

        // Only the primary button moves a node. Any other button belongs to the
        // canvas gestures, so a middle-button pan that starts over a node pans
        // the view instead of dragging it away.
        if response.drag_started_by(egui::PointerButton::Primary) {
            state.drag = Some(DragState {
                node: node_id.clone(),
                start_position: position,
                accumulated_offset: EguiVec2::ZERO,
            });
        }

        let mut preview_position = position;
        if response.dragged_by(egui::PointerButton::Primary)
            && let Some(drag) = &mut state.drag
                && drag.node == *node_id {
                    drag.accumulated_offset += response.drag_motion();
                    preview_position = drag.position();
                }

        if response.drag_stopped_by(egui::PointerButton::Primary) {
            if let Some(drag) = state.drag.take() {
                if drag.node == *node_id {
                    let position = apply_released_preview(&mut preview_position, &drag);
                    // Keep the released preview visible until the command is
                    // applied after this frame's UI pass.
                    actions.push(GraphCanvasAction::NodeDragReleased {
                        node: node_id.clone(),
                        position,
                    });
                } else {
                    state.drag = Some(drag);
                }
            }
        } else if response.clicked() {
            actions.push(GraphCanvasAction::NodeClicked {
                node: node_id.clone(),
            });
        }

        if session.is_animation_graph() {
            let handle_center = transition_handle_center(rect);
            let handle_rect = Rect::from_center_size(
                handle_center,
                EguiVec2::splat(TRANSITION_HANDLE_RADIUS * 2.5),
            );
            let handle_response = ui
                .interact(
                    handle_rect,
                    ui.make_persistent_id(("animation_transition_handle", node_id.as_str())),
                    Sense::click_and_drag(),
                )
                .on_hover_text("Drag to another State to create a transition");
            if handle_response.drag_started_by(egui::PointerButton::Primary) {
                state.connect_drag = Some(node_id.clone());
            }
            if handle_response.clicked() {
                actions.push(GraphCanvasAction::ConnectFrom {
                    node: node_id.clone(),
                });
            }
            if handle_response.drag_stopped_by(egui::PointerButton::Primary)
                && let Some(source) = state.connect_drag.take() {
                    let target = ui
                        .input(|input| input.pointer.latest_pos())
                        .and_then(|pointer| {
                            node_at_screen_position(canvas_view, &layout_positions, pointer)
                        });
                    if let Some(target) = target.filter(|target| target != &source) {
                        actions.push(GraphCanvasAction::ConnectNodes { source, target });
                    }
                }
        }

        preview_positions.insert(node_id.clone(), preview_position);
    }

    // Paint edges from the same preview positions as nodes. This prevents
    // transition lines from lagging one frame behind a dragged node.
    paint_edges(
        &painter,
        canvas_view,
        session,
        &preview_positions,
        hovered_edge.as_ref(),
    );
    draw_connect_drag(
        &painter,
        canvas_view,
        state.connect_drag.as_ref(),
        &preview_positions,
        ui.input(|input| input.pointer.hover_pos()),
    );
    draw_pending_connect(&painter, canvas_view, pending_connect, &preview_positions);
    for (node_id, node) in &session.graph().nodes {
        let Some(position) = preview_positions.get(node_id).copied() else {
            continue;
        };
        let pinned = session
            .graph_view()
            .and_then(|view| view.nodes.get(node_id))
            .is_some_and(|layout| layout.pinned);
        let selected = session
            .graph_view()
            .is_some_and(|view| view.selection.nodes.contains(node_id));
        draw_node(
            &painter,
            canvas_view,
            DrawNodeParams {
                position,
                node_id,
                title: compact_canvas_text(&node_title(node)),
                subtitle: node_subtitle(session, node),
                selected,
                pinned,
                show_id: !session.is_animation_graph(),
            },
        );
        if session.is_animation_graph() {
            draw_transition_handle(
                &painter,
                canvas_view.node_rect(position),
                pending_connect == Some(node_id) || state.connect_drag.as_ref() == Some(node_id),
            );
        }
    }

    // Nodes can be dragged past the canvas edge, and an invisible node cannot
    // be clicked back into view. Name the two recoveries where they are needed.
    if !preview_positions.is_empty()
        && !preview_positions
            .values()
            .any(|position| canvas_view.node_rect(*position).intersects(canvas_rect))
    {
        painter.text(
            canvas_rect.center(),
            Align2::CENTER_CENTER,
            "No node is in view — middle-drag to pan, or right-click for Frame All Nodes",
            FontId::proportional(13.0),
            Color32::from_rgb(170, 178, 188),
        );
    }

    actions
}

/// Applies a middle-button drag to the canvas pan offset.
///
/// The gesture is read from raw pointer input instead of a widget response
/// because it must keep working when it starts over a node, which owns the
/// interaction at that position.
fn apply_canvas_pan(ui: &egui::Ui, state: &mut GraphCanvasState, canvas_rect: Rect) {
    let (pressed_in_canvas, held, delta) = ui.input(|input| {
        let pressed = input.pointer.button_pressed(egui::PointerButton::Middle);
        let inside = input
            .pointer
            .interact_pos()
            .is_some_and(|pointer| canvas_rect.contains(pointer));
        (
            pressed && inside,
            input.pointer.button_down(egui::PointerButton::Middle),
            input.pointer.delta(),
        )
    });
    if pressed_in_canvas {
        state.panning = true;
    }
    if !held {
        state.panning = false;
        return;
    }
    if state.panning {
        state.pan += delta;
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
}

/// Pan offset that centers every node inside the canvas.
///
/// Returns no offset for an empty graph, which leaves the view at the graph
/// origin where newly added nodes appear.
fn frame_all_pan(canvas_rect: Rect, positions: &BTreeMap<NodeId, Vec2>) -> EguiVec2 {
    let mut bounds: Option<Rect> = None;
    for position in positions.values() {
        let node = Rect::from_min_size(Pos2::new(position.x as f32, position.y as f32), NODE_SIZE);
        bounds = Some(match bounds {
            Some(bounds) => bounds.union(node),
            None => node,
        });
    }
    let Some(bounds) = bounds else {
        return EguiVec2::ZERO;
    };
    canvas_rect.center()
        - (canvas_rect.left_top() + EguiVec2::splat(CANVAS_MARGIN) + bounds.center().to_vec2())
}

/// Sampled screen-space route used by painting and precise edge picking.
struct EdgePath {
    points: Vec<Pos2>,
    badge_center: Pos2,
    label_center: Pos2,
}

impl EdgePath {
    fn from_points(points: Vec<Pos2>) -> Option<Self> {
        if points.len() < 2 {
            return None;
        }
        let midpoint_index = points.len() / 2;
        let badge_center = points[midpoint_index];
        let tangent_start = points[midpoint_index.saturating_sub(1)];
        let tangent_end = points[(midpoint_index + 1).min(points.len() - 1)];
        let tangent = tangent_end - tangent_start;
        let normal = if tangent.length_sq() > f32::EPSILON {
            let direction = tangent.normalized();
            EguiVec2::new(-direction.y, direction.x)
        } else {
            EguiVec2::Y
        };
        Some(Self {
            points,
            badge_center,
            label_center: badge_center + normal * TRANSITION_LABEL_OFFSET,
        })
    }

    fn tangent_at_badge(&self) -> EguiVec2 {
        let midpoint_index = self.points.len() / 2;
        self.points[(midpoint_index + 1).min(self.points.len() - 1)]
            - self.points[midpoint_index.saturating_sub(1)]
    }
}

fn closest_edge_at(
    view: CanvasView,
    session: &EditorSession,
    positions: &BTreeMap<NodeId, Vec2>,
    pointer: Pos2,
    maximum_distance: f32,
) -> Option<EdgeId> {
    session
        .graph()
        .edges
        .iter()
        .filter_map(|(edge_id, edge)| {
            let path = edge_screen_path(view, session, edge, positions)?;
            let distance = edge_pointer_distance(session, edge, &path, pointer);
            (distance <= maximum_distance).then_some((edge_id, distance))
        })
        .min_by(|(left_id, left_distance), (right_id, right_distance)| {
            left_distance
                .total_cmp(right_distance)
                .then_with(|| left_id.as_str().cmp(right_id.as_str()))
        })
        .map(|(edge, _)| edge.clone())
}

fn paint_edges(
    painter: &egui::Painter,
    view: CanvasView,
    session: &EditorSession,
    positions: &BTreeMap<NodeId, Vec2>,
    hovered_edge: Option<&EdgeId>,
) {
    let mut routed_edges = session
        .graph()
        .edges
        .iter()
        .filter_map(|(edge_id, edge)| {
            let path = edge_screen_path(view, session, edge, positions)?;
            let selected = session
                .graph_view()
                .is_some_and(|view| view.selection.edges.contains(edge_id));
            let hovered = hovered_edge == Some(edge_id);
            Some((edge_id, edge, path, selected, hovered))
        })
        .collect::<Vec<_>>();
    // Selected and hovered transitions are painted last so their full route
    // remains visible where unrelated state-machine paths cross.
    routed_edges
        .sort_by_key(|(_, _, _, selected, hovered)| (u8::from(*selected) * 2) + u8::from(*hovered));

    for (_, edge, path, selected, hovered) in routed_edges {
        let stroke = if selected {
            Stroke::new(3.0_f32, Color32::from_rgb(238, 197, 97))
        } else if hovered {
            Stroke::new(3.0_f32, Color32::from_rgb(134, 190, 255))
        } else {
            Stroke::new(2.0_f32, Color32::from_rgb(107, 148, 196))
        };
        painter.add(egui::Shape::line(path.points.clone(), stroke));
        draw_arrow_head(
            painter,
            path.points[path.points.len() - 2],
            path.points[path.points.len() - 1],
            stroke,
        );

        if session.is_animation_graph()
            && let Some(label) = animation_edge_label(session, edge) {
                draw_transition_badge(painter, &path, stroke);
                draw_transition_label(painter, path.label_center, &label, selected, hovered);
            }
    }
}

fn edge_screen_path(
    view: CanvasView,
    session: &EditorSession,
    edge: &Edge,
    positions: &BTreeMap<NodeId, Vec2>,
) -> Option<EdgePath> {
    let from_rect = view.node_rect(positions.get(&edge.from.node).copied()?);
    let to_rect = view.node_rect(positions.get(&edge.to.node).copied()?);

    if edge.from.node == edge.to.node {
        let start = if session.is_animation_graph() {
            transition_handle_center(from_rect)
        } else {
            Pos2::new(from_rect.right() - 34.0, from_rect.top())
        };
        let end = Pos2::new(from_rect.right(), from_rect.top() + 28.0);
        let control_a = start + EguiVec2::new(58.0, -42.0);
        let control_b = end + EguiVec2::new(68.0, -48.0);
        return EdgePath::from_points(sample_cubic_curve(start, control_a, control_b, end));
    }

    let (from, to) = if session.is_animation_graph() {
        let from = transition_handle_center(from_rect);
        (from, rect_boundary_toward(to_rect, from))
    } else {
        (
            from_rect.right_center(),
            Pos2::new(to_rect.left(), to_rect.center().y),
        )
    };
    let delta = to - from;
    let length = delta.length();
    if length <= f32::EPSILON {
        return EdgePath::from_points(vec![from, to]);
    }

    let has_reverse = session.graph().edges.values().any(|candidate| {
        candidate.from.node == edge.to.node && candidate.to.node == edge.from.node
    });
    let direction = delta / length;
    let normal = EguiVec2::new(-direction.y, direction.x);
    let bend = if session.is_animation_graph() && has_reverse {
        (length * 0.18).clamp(28.0, 52.0)
    } else {
        0.0
    };
    let control_a = from + delta * 0.33 + normal * bend;
    let control_b = from + delta * 0.67 + normal * bend;
    EdgePath::from_points(sample_cubic_curve(from, control_a, control_b, to))
}

fn rect_boundary_toward(rect: Rect, target: Pos2) -> Pos2 {
    let center = rect.center();
    let delta = target - center;
    if delta.length_sq() <= f32::EPSILON {
        return rect.right_center();
    }
    let horizontal_scale = if delta.x.abs() > f32::EPSILON {
        (rect.width() * 0.5) / delta.x.abs()
    } else {
        f32::INFINITY
    };
    let vertical_scale = if delta.y.abs() > f32::EPSILON {
        (rect.height() * 0.5) / delta.y.abs()
    } else {
        f32::INFINITY
    };
    center + delta * horizontal_scale.min(vertical_scale)
}

fn sample_cubic_curve(start: Pos2, control_a: Pos2, control_b: Pos2, end: Pos2) -> Vec<Pos2> {
    (0..=CURVE_SAMPLE_COUNT)
        .map(|step| {
            let t = step as f32 / CURVE_SAMPLE_COUNT as f32;
            let inverse = 1.0 - t;
            let start_weight = inverse * inverse * inverse;
            let control_a_weight = 3.0 * inverse * inverse * t;
            let control_b_weight = 3.0 * inverse * t * t;
            let end_weight = t * t * t;
            Pos2::new(
                (start.x * start_weight)
                    + (control_a.x * control_a_weight)
                    + (control_b.x * control_b_weight)
                    + (end.x * end_weight),
                (start.y * start_weight)
                    + (control_a.y * control_a_weight)
                    + (control_b.y * control_b_weight)
                    + (end.y * end_weight),
            )
        })
        .collect()
}

fn edge_pointer_distance(
    session: &EditorSession,
    edge: &Edge,
    path: &EdgePath,
    pointer: Pos2,
) -> f32 {
    let mut distance = path
        .points
        .windows(2)
        .map(|segment| point_segment_distance(pointer, segment[0], segment[1]))
        .fold(f32::INFINITY, f32::min);
    if session.is_animation_graph() {
        distance =
            distance.min((pointer.distance(path.badge_center) - TRANSITION_BADGE_RADIUS).max(0.0));
        if let Some(label) = animation_edge_label(session, edge) {
            let label_rect = transition_label_rect(path.label_center, &label);
            if label_rect.contains(pointer) {
                distance = 0.0;
            }
        }
    }
    distance
}

fn transition_label_rect(center: Pos2, label: &str) -> Rect {
    let width = ((label.chars().count() as f32 * 7.0) + 16.0).clamp(42.0, 280.0);
    Rect::from_center_size(center, EguiVec2::new(width, 24.0))
}

fn draw_transition_badge(painter: &egui::Painter, path: &EdgePath, stroke: Stroke) {
    let tangent = path.tangent_at_badge();
    let direction = if tangent.length_sq() > f32::EPSILON {
        tangent.normalized()
    } else {
        EguiVec2::X
    };
    let normal = EguiVec2::new(-direction.y, direction.x);
    painter.circle_filled(
        path.badge_center,
        TRANSITION_BADGE_RADIUS,
        Color32::from_rgb(31, 35, 41),
    );
    painter.circle_stroke(path.badge_center, TRANSITION_BADGE_RADIUS, stroke);
    let tip = path.badge_center + direction * 4.5;
    let base = path.badge_center - direction * 3.0;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, base + normal * 3.0, base - normal * 3.0],
        stroke.color,
        Stroke::NONE,
    ));
}

fn draw_transition_label(
    painter: &egui::Painter,
    center: Pos2,
    label: &str,
    selected: bool,
    hovered: bool,
) {
    let text_color = Color32::from_rgb(224, 230, 238);
    let galley = painter.layout_no_wrap(label.to_owned(), FontId::proportional(12.0), text_color);
    let rect = Rect::from_center_size(center, galley.size() + EguiVec2::new(14.0, 7.0));
    let fill = if selected {
        Color32::from_rgb(74, 63, 38)
    } else if hovered {
        Color32::from_rgb(38, 57, 77)
    } else {
        Color32::from_rgb(31, 35, 41)
    };
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0_f32, Color32::from_rgb(78, 89, 103)),
        StrokeKind::Inside,
    );
    painter.galley(rect.center() - galley.size() * 0.5, galley, text_color);
}

/// Draws a compact directional marker without introducing persisted routing.
fn draw_arrow_head(painter: &egui::Painter, from: Pos2, to: Pos2, stroke: Stroke) {
    let delta = to - from;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let direction = delta / length;
    let perpendicular = EguiVec2::new(-direction.y, direction.x);
    let base = to - direction * 11.0;
    painter.line_segment([to, base + perpendicular * 5.0], stroke);
    painter.line_segment([to, base - perpendicular * 5.0], stroke);
}

/// Returns pointer distance to a finite line segment for precise edge picking.
fn point_segment_distance(point: Pos2, from: Pos2, to: Pos2) -> f32 {
    let segment = to - from;
    let length_squared = segment.length_sq();
    if length_squared <= f32::EPSILON {
        return point.distance(from);
    }
    let offset = point - from;
    let projection = (offset.dot(segment) / length_squared).clamp(0.0, 1.0);
    point.distance(from + segment * projection)
}

fn animation_edge_label(session: &EditorSession, edge: &Edge) -> Option<String> {
    let source_is_entry = session
        .graph()
        .nodes
        .get(&edge.from.node)
        .is_some_and(|node| node.node_type.as_str() == "anim.entry");
    if source_is_entry {
        return Some("initial".to_owned());
    }
    let condition = match edge.annotations.get("condition") {
        Some(Value::String(condition)) if !condition.is_empty() => condition.clone(),
        _ => "unconditional".to_owned(),
    };
    let fade = edge
        .annotations
        .get("fade_duration")
        .and_then(|value| match value {
            Value::F64(value) => Some(*value),
            Value::I64(value) => Some(*value as f64),
            Value::U64(value) => Some(*value as f64),
            _ => None,
        });
    let label = match fade {
        Some(fade) => format!("{condition} / {fade:.2}s"),
        None => condition,
    };
    Some(compact_canvas_text(&label))
}

fn draw_pending_connect(
    painter: &egui::Painter,
    view: CanvasView,
    pending_connect: Option<&NodeId>,
    positions: &BTreeMap<NodeId, Vec2>,
) {
    let Some(source) = pending_connect else {
        return;
    };
    let Some(position) = positions.get(source).copied() else {
        return;
    };
    let center = view.node_rect(position).center();
    painter.circle_stroke(
        center,
        7.0,
        Stroke::new(2.0_f32, Color32::from_rgb(238, 197, 97)),
    );
}

fn transition_handle_center(node_rect: Rect) -> Pos2 {
    node_rect.right_center() + EguiVec2::new(10.0, 0.0)
}

fn draw_transition_handle(painter: &egui::Painter, node_rect: Rect, active: bool) {
    let center = transition_handle_center(node_rect);
    let color = if active {
        Color32::from_rgb(238, 197, 97)
    } else {
        Color32::from_rgb(107, 148, 196)
    };
    painter.circle_filled(
        center,
        TRANSITION_HANDLE_RADIUS,
        Color32::from_rgb(31, 35, 41),
    );
    painter.circle_stroke(
        center,
        TRANSITION_HANDLE_RADIUS,
        Stroke::new(if active { 3.0_f32 } else { 2.0_f32 }, color),
    );
}

fn draw_connect_drag(
    painter: &egui::Painter,
    view: CanvasView,
    source: Option<&NodeId>,
    positions: &BTreeMap<NodeId, Vec2>,
    pointer: Option<Pos2>,
) {
    let (Some(source), Some(pointer)) = (source, pointer) else {
        return;
    };
    let Some(position) = positions.get(source) else {
        return;
    };
    let start = transition_handle_center(view.node_rect(*position));
    let delta = pointer - start;
    let points = sample_cubic_curve(start, start + delta * 0.33, start + delta * 0.67, pointer);
    let stroke = Stroke::new(2.0_f32, Color32::from_rgb(238, 197, 97));
    painter.add(egui::Shape::line(points, stroke));
    draw_arrow_head(painter, start, pointer, stroke);
}

fn node_at_screen_position(
    view: CanvasView,
    positions: &BTreeMap<NodeId, Vec2>,
    pointer: Pos2,
) -> Option<NodeId> {
    positions
        .iter()
        .find(|(_, position)| view.node_rect(**position).contains(pointer))
        .map(|(node, _)| node.clone())
}

struct DrawNodeParams<'a> {
    position: Vec2,
    node_id: &'a NodeId,
    title: String,
    subtitle: String,
    selected: bool,
    pinned: bool,
    show_id: bool,
}

fn draw_node(painter: &egui::Painter, view: CanvasView, params: DrawNodeParams<'_>) {
    let DrawNodeParams {
        position,
        node_id,
        title,
        subtitle,
        selected,
        pinned,
        show_id,
    } = params;
    let rect = view.node_rect(position);
    let fill = if selected {
        Color32::from_rgb(47, 72, 102)
    } else {
        Color32::from_rgb(39, 43, 49)
    };
    let stroke = if selected {
        Stroke::new(2.0_f32, Color32::from_rgb(119, 177, 255))
    } else {
        Stroke::new(1.0_f32, Color32::from_rgb(80, 88, 98))
    };
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);

    // A pinned node keeps its indicator in the top-right corner, so the title
    // must give up that much of the row to avoid painting underneath it.
    let text_width = rect.width() - NODE_TEXT_PADDING * 2.0;
    let title_width = if pinned {
        text_width - PIN_INDICATOR_WIDTH
    } else {
        text_width
    };
    draw_node_line(
        painter,
        rect.left_top() + EguiVec2::new(NODE_TEXT_PADDING, 10.0),
        &title,
        FontId::proportional(15.0),
        Color32::from_rgb(238, 241, 245),
        title_width,
    );
    draw_node_line(
        painter,
        rect.left_top() + EguiVec2::new(NODE_TEXT_PADDING, 34.0),
        &subtitle,
        FontId::proportional(11.0),
        Color32::from_rgb(170, 178, 188),
        text_width,
    );
    if show_id {
        draw_node_line(
            painter,
            rect.left_top() + EguiVec2::new(NODE_TEXT_PADDING, 51.0),
            node_id.as_str(),
            FontId::monospace(8.0),
            Color32::from_rgb(124, 132, 143),
            text_width,
        );
    }
    if pinned {
        painter.text(
            rect.right_top() + EguiVec2::new(-NODE_TEXT_PADDING, 10.0),
            Align2::RIGHT_TOP,
            "pin",
            FontId::proportional(12.0),
            Color32::from_rgb(238, 197, 97),
        );
    }
}

/// Paints one line of node text, elided to the node's own width.
///
/// A character-count limit cannot know how wide the glyphs are: a name made of
/// wide characters used to run past the node border and paint over whatever
/// node sits next to it. Laying the line out with an explicit `max_width` and a
/// single row keeps every label inside its node.
fn draw_node_line(
    painter: &egui::Painter,
    position: Pos2,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) {
    let job = node_line_job(text, font, color, max_width);
    painter.galley(position, painter.layout_job(job), color);
}

/// Builds the single-row, elided layout used by every node label.
fn node_line_job(
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(font, color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job
}

fn node_subtitle(session: &EditorSession, node: &Node) -> String {
    match node.node_type.as_str() {
        "anim.entry" => "Entry point".to_owned(),
        "anim.state" => {
            let motion_slot = match &node.properties {
                Value::Object(properties) => {
                    properties.get("motion_slot").and_then(|value| match value {
                        Value::String(slot) => Some(slot.as_str()),
                        _ => None,
                    })
                }
                _ => None,
            };
            let slot_name = motion_slot
                .and_then(|motion_slot| {
                    session
                        .motion_slots()
                        .ok()?
                        .into_iter()
                        .find(|slot| slot.id.as_str() == motion_slot)
                        .map(|slot| slot.display_name)
                })
                .unwrap_or_else(|| "Unassigned".to_owned());
            compact_canvas_text(&format!("Motion: {slot_name}"))
        }
        node_type => node_type_label(node_type).to_owned(),
    }
}

fn compact_canvas_text(text: &str) -> String {
    let mut characters = text.chars();
    let compact = characters
        .by_ref()
        .take(MAX_CANVAS_LABEL_CHARACTERS)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{compact}…")
    } else {
        compact
    }
}

fn node_title(node: &Node) -> String {
    if let Some(name) = node.name.as_deref().filter(|name| !name.is_empty()) {
        return name.to_owned();
    }
    match node.node_type.as_str() {
        "anim.entry" => "Entry".to_owned(),
        "anim.state" => match &node.properties {
            Value::Object(properties) => properties
                .get("motion_name")
                .and_then(|value| match value {
                    Value::String(motion) if !motion.is_empty() => Some(motion.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "State".to_owned()),
            _ => "State".to_owned(),
        },
        _ => node.node_type.as_str().to_owned(),
    }
}

/// Returns a compact human-facing type label for the secondary node line.
fn node_type_label(node_type: &str) -> &str {
    match node_type {
        "anim.entry" => "Entry",
        "anim.state" => "State",
        _ => node_type,
    }
}

fn node_positions(session: &EditorSession) -> BTreeMap<NodeId, Vec2> {
    let mut positions = BTreeMap::new();
    for (index, node_id) in session.graph().nodes.keys().enumerate() {
        let position = session
            .graph_view()
            .and_then(|view| view.nodes.get(node_id))
            .map(|layout| layout.position)
            .unwrap_or_else(|| fallback_position_for_index(index));
        positions.insert(node_id.clone(), position);
    }
    positions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::AnimationNodeInsertKind;

    #[test]
    fn released_drag_preview_replaces_the_persisted_layout_position() {
        let drag = DragState {
            node: NodeId::generate(),
            start_position: Vec2::new(12.0, 24.0),
            accumulated_offset: EguiVec2::new(30.0, -8.0),
        };
        let mut preview_position = drag.start_position;

        let released = apply_released_preview(&mut preview_position, &drag);

        assert_eq!(released, Vec2::new(42.0, 16.0));
        assert_eq!(preview_position, released);
    }

    #[test]
    fn reciprocal_animation_transitions_use_distinct_parallel_paths() {
        let mut session = EditorSession::empty_animation_graph();
        let first = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(260.0, 0.0)))
            .expect("first State should be added");
        let second = session
            .add_animation_node(AnimationNodeInsertKind::State, Some(Vec2::new(0.0, 260.0)))
            .expect("second State should be added");
        let forward = session
            .connect_animation_transition(first.clone(), second.clone())
            .expect("forward transition should connect");
        let reverse = session
            .connect_animation_transition(second.clone(), first.clone())
            .expect("reverse transition should connect");
        let positions = node_positions(&session);
        let canvas = Rect::from_min_size(Pos2::ZERO, EguiVec2::new(800.0, 600.0));
        let view = CanvasView::new(canvas, EguiVec2::ZERO);

        let forward_path =
            edge_screen_path(view, &session, &session.graph().edges[&forward], &positions)
                .expect("forward transition should have endpoints");
        let reverse_path =
            edge_screen_path(view, &session, &session.graph().edges[&reverse], &positions)
                .expect("reverse transition should have endpoints");

        assert!(
            forward_path
                .badge_center
                .distance(reverse_path.badge_center)
                > 40.0
        );
        assert_eq!(
            closest_edge_at(
                view,
                &session,
                &positions,
                forward_path.badge_center,
                EDGE_HIT_RADIUS,
            ),
            Some(forward)
        );
        assert_eq!(
            closest_edge_at(
                view,
                &session,
                &positions,
                reverse_path.badge_center,
                EDGE_HIT_RADIUS,
            ),
            Some(reverse)
        );
        let first_rect = view.node_rect(positions[&first]);
        let second_rect = view.node_rect(positions[&second]);
        assert_eq!(forward_path.points[0], transition_handle_center(first_rect));
        assert_eq!(
            reverse_path.points[0],
            transition_handle_center(second_rect)
        );
        assert!(second_rect.contains(forward_path.points[forward_path.points.len() - 1]));
    }

    /// Reproduces the node dragged so far out that it could no longer be
    /// clicked: framing must bring it back without touching its position.
    #[test]
    fn framing_brings_offscreen_nodes_back_into_the_canvas() {
        let canvas = Rect::from_min_size(Pos2::ZERO, EguiVec2::new(800.0, 600.0));
        let mut positions = BTreeMap::new();
        positions.insert(NodeId::generate(), Vec2::new(4200.0, -3100.0));
        positions.insert(NodeId::generate(), Vec2::new(4400.0, -2900.0));
        let stranded = CanvasView::new(canvas, EguiVec2::ZERO);
        assert!(positions
            .values()
            .all(|position| !stranded.node_rect(*position).intersects(canvas)));

        let framed = CanvasView::new(canvas, frame_all_pan(canvas, &positions));

        for position in positions.values() {
            assert!(
                canvas.contains_rect(framed.node_rect(*position)),
                "node at {position:?} stayed outside the canvas after framing"
            );
        }
    }

    #[test]
    fn panning_moves_hit_testing_with_the_painted_nodes() {
        let canvas = Rect::from_min_size(Pos2::ZERO, EguiVec2::new(800.0, 600.0));
        let node = NodeId::generate();
        let mut positions = BTreeMap::new();
        positions.insert(node.clone(), Vec2::new(900.0, 0.0));
        let pointer = Pos2::new(200.0, 60.0);
        assert_eq!(
            node_at_screen_position(CanvasView::new(canvas, EguiVec2::ZERO), &positions, pointer),
            None
        );

        let panned = CanvasView::new(canvas, EguiVec2::new(-800.0, 0.0));

        assert_eq!(
            node_at_screen_position(panned, &positions, pointer),
            Some(node)
        );
    }

    #[test]
    fn empty_graph_keeps_the_view_at_the_origin() {
        let canvas = Rect::from_min_size(Pos2::ZERO, EguiVec2::new(800.0, 600.0));
        assert_eq!(frame_all_pan(canvas, &BTreeMap::new()), EguiVec2::ZERO);
    }

    #[test]
    fn long_canvas_labels_are_compacted_without_losing_short_labels() {
        assert_eq!(compact_canvas_text("Idle"), "Idle");
        let long = "condition_name_that_would_overlap_neighboring_transition_labels";
        let compact = compact_canvas_text(long);
        assert!(compact.ends_with('…'));
        assert_eq!(compact.chars().count(), MAX_CANVAS_LABEL_CHARACTERS + 1);
    }

    /// Reproduces the wide-glyph state name that used to paint past its node
    /// border and over the neighboring node.
    #[test]
    fn node_labels_are_elided_to_the_node_width() {
        let context = egui::Context::default();
        let max_width = NODE_SIZE.x - NODE_TEXT_PADDING * 2.0;
        // Fonts exist only inside a frame, so lay the label out in a headless
        // one rather than opening a window.
        let mut laid_out = None;
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let job = node_line_job(
                &compact_canvas_text(&"W".repeat(MAX_CANVAS_LABEL_CHARACTERS * 2)),
                FontId::proportional(15.0),
                Color32::WHITE,
                max_width,
            );
            laid_out = Some(ui.painter().layout_job(job));
        });
        let galley = laid_out.expect("label must be laid out");

        assert_eq!(galley.rows.len(), 1, "node labels must stay on one row");
        assert!(
            galley.size().x <= max_width,
            "label width {} exceeded the node text width {max_width}",
            galley.size().x
        );
    }

    #[test]
    fn animation_state_title_prefers_the_authored_state_name() {
        let node = Node {
            id: NodeId::generate(),
            node_type: engine_authoring::NodeTypeId::new("anim.state"),
            name: Some("Locomotion".to_owned()),
            properties: Value::Object(BTreeMap::new()),
            annotations: BTreeMap::new(),
        };

        assert_eq!(node_title(&node), "Locomotion");
    }

    #[test]
    fn graph_screen_conversion_accounts_for_pan() {
        let canvas = Rect::from_min_size(Pos2::new(10.0, 20.0), EguiVec2::new(800.0, 600.0));
        let view = CanvasView::new(canvas, EguiVec2::new(125.0, -70.0));
        let graph_position = Vec2::new(260.0, 310.0);

        assert_eq!(
            view.to_graph(view.to_screen(graph_position)),
            graph_position
        );
    }

    #[test]
    fn automatic_placement_uses_selected_node_and_skips_an_occupied_slot() {
        let mut session = EditorSession::empty_animation_graph();
        let entry = session
            .graph()
            .nodes
            .iter()
            .find(|(_, node)| node.node_type.as_str() == "anim.entry")
            .map(|(node, _)| node.clone())
            .expect("animation graph must contain Entry");
        session
            .add_animation_node(
                AnimationNodeInsertKind::State,
                Some(Vec2::new(node_horizontal_step(), 0.0)),
            )
            .expect("occupied State should be added");
        session
            .select_node(Some(entry))
            .expect("Entry should be selectable");

        assert_eq!(
            next_node_position(&session, None, None),
            Vec2::new(node_horizontal_step(), node_vertical_step())
        );
    }

    #[test]
    fn explicit_canvas_position_is_kept_when_it_is_free() {
        let session = EditorSession::empty_animation_graph();
        let requested = Vec2::new(-320.0, 480.0);

        assert_eq!(
            next_node_position(&session, Some(requested), None),
            requested
        );
    }
}
