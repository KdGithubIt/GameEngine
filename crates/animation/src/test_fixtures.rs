//! Opt-in fixtures shared by cross-crate animation acceptance tests.

use std::collections::BTreeMap;

use engine_authoring::graph::{Edge, Graph, Node, PortRef};
use engine_authoring::id::{EdgeId, GraphId, MotionSlotId, NodeId};
use engine_authoring::{AnimationGraphDomain, GraphDomain, Value};

fn empty_graph(domain: &AnimationGraphDomain) -> Graph {
    Graph::new(GraphId::generate(), domain.graph_kind().clone(), "test")
}

fn add_entry(graph: &mut Graph, domain: &AnimationGraphDomain) -> NodeId {
    let id = NodeId::generate();
    graph.nodes.insert(
        id.clone(),
        Node::new(
            id.clone(),
            domain.entry_type().clone(),
            Value::Object(BTreeMap::new()),
        ),
    );
    id
}

fn add_state(graph: &mut Graph, domain: &AnimationGraphDomain) -> NodeId {
    let id = NodeId::generate();
    graph.nodes.insert(
        id.clone(),
        Node::new(
            id.clone(),
            domain.state_type().clone(),
            Value::Object(BTreeMap::new()),
        ),
    );
    id
}

fn set_motion_slot(graph: &mut Graph, state: &NodeId, slot: &MotionSlotId, name: &str) {
    let Value::Object(properties) = &mut graph
        .nodes
        .get_mut(state)
        .expect("fixture state must exist")
        .properties
    else {
        unreachable!("animation State properties are objects")
    };
    properties.insert(
        "motion_slot".to_owned(),
        Value::String(slot.as_str().to_owned()),
    );
    properties.insert("motion_name".to_owned(), Value::String(name.to_owned()));
}

fn connect(
    graph: &mut Graph,
    from: &NodeId,
    from_port: &engine_authoring::id::PortId,
    to: &NodeId,
    to_port: &engine_authoring::id::PortId,
) {
    let id = EdgeId::generate();
    graph.edges.insert(
        id.clone(),
        Edge::new(
            id,
            PortRef::new(from.clone(), from_port.clone()),
            PortRef::new(to.clone(), to_port.clone()),
        ),
    );
}

fn connect_states(
    graph: &mut Graph,
    domain: &AnimationGraphDomain,
    from: &NodeId,
    to: &NodeId,
    condition: &str,
) {
    let id = EdgeId::generate();
    let mut edge = Edge::new(
        id.clone(),
        PortRef::new(from.clone(), domain.state_out_port().clone()),
        PortRef::new(to.clone(), domain.state_in_port().clone()),
    );
    if !condition.is_empty() {
        edge.annotations.insert(
            "condition".to_owned(),
            Value::String(condition.to_owned()),
        );
    }
    graph.edges.insert(id, edge);
}

/// Builds a canonical one-state Animation Graph JSON document for `motion_slot`.
pub fn valid_graph_json_for_motion_slot(motion_slot: &MotionSlotId) -> String {
    let domain = AnimationGraphDomain::new();
    let mut graph = empty_graph(&domain);
    let entry = add_entry(&mut graph, &domain);
    let state = add_state(&mut graph, &domain);
    set_motion_slot(&mut graph, &state, motion_slot, "spin");
    connect(
        &mut graph,
        &entry,
        domain.entry_out_port(),
        &state,
        domain.state_in_port(),
    );
    graph
        .to_canonical_json(&domain)
        .expect("valid graph fixture must serialize")
}

/// Builds canonical two-state Animation Graph JSON for two motion slots.
pub fn valid_graph_json_for_motion_slots(
    first_slot: &MotionSlotId,
    second_slot: &MotionSlotId,
) -> String {
    let domain = AnimationGraphDomain::new();
    let mut graph = empty_graph(&domain);
    let entry = add_entry(&mut graph, &domain);
    let first = add_state(&mut graph, &domain);
    let second = add_state(&mut graph, &domain);
    set_motion_slot(&mut graph, &first, first_slot, "first");
    set_motion_slot(&mut graph, &second, second_slot, "second");
    connect(
        &mut graph,
        &entry,
        domain.entry_out_port(),
        &first,
        domain.state_in_port(),
    );
    connect_states(&mut graph, &domain, &first, &second, "switch == true");
    graph
        .to_canonical_json(&domain)
        .expect("valid graph fixture must serialize")
}
