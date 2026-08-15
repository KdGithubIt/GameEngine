//! Toolkit-independent editor geometry helpers.

use engine_authoring::{Graph, NodeId, Vec2};

/// Returns the deterministic prototype canvas position for a zero-based graph
/// node index.
pub fn fallback_position_for_index(index: usize) -> Vec2 {
    let column = (index % 4) as f64;
    let row = (index / 4) as f64;
    Vec2::new(column * 220.0, row * 140.0)
}

/// Returns the deterministic prototype canvas position for `node`.
pub fn fallback_position_for_node(graph: &Graph, node: &NodeId) -> Vec2 {
    let index = graph
        .nodes
        .keys()
        .position(|candidate| candidate == node)
        .unwrap_or(0);
    fallback_position_for_index(index)
}
