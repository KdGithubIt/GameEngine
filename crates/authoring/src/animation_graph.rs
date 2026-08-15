//! Animation state machine graph domain (Phase 38, ADR 0033).
//!
//! Implements [`GraphDomain`] for animation state machines, reusing the graph
//! foundation from `crates/authoring` without duplicating the graph model.
//!
//! Graphs authored with this domain have `kind: "anim.graph"` and contain
//! `anim.State` and `anim.Entry` node types.

use crate::diagnostic::Diagnostic;
use crate::graph::{
    Edge, Graph, GraphKind, GraphSchemaRegistry, NodeSchema, NodeTypeId, PortArity, PortDirection,
    PortSchema, PortValueTypeId,
};
use crate::graph_domain::{validate_graph_with_domain, GraphDomain};
use crate::id::{MotionSlotId, NodeId, PortId, StableId};
use crate::value::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Semantic graph annotation that owns the Animation Graph motion-slot list.
pub const MOTION_SLOTS_ANNOTATION: &str = "anim.motion_slots";

/// Property key stored on Animation State nodes for their playback behavior.
pub const ANIMATION_STATE_PLAYBACK_MODE_PROPERTY: &str = "playback_mode";

/// Playback behavior selected by one compiled Animation State.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationStatePlaybackMode {
    /// Restart the active clip whenever it reaches its duration.
    #[default]
    Loop,
    /// Stop the active clip when it reaches its duration.
    Once,
}

impl AnimationStatePlaybackMode {
    /// Returns the canonical node-property string for this playback mode.
    pub const fn persisted_name(self) -> &'static str {
        match self {
            Self::Loop => "loop",
            Self::Once => "once",
        }
    }

    /// Parses one canonical Animation State playback-mode property value.
    pub fn from_persisted_name(value: &str) -> Option<Self> {
        match value {
            "loop" => Some(Self::Loop),
            "once" => Some(Self::Once),
            _ => None,
        }
    }
}

/// One stable motion slot owned by an entire Animation Graph.
///
/// States reference [`Self::id`], while editors show and rename
/// [`Self::display_name`]. Renaming therefore never invalidates an Animation
/// Set binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MotionSlot {
    /// Stable identifier persisted by states and Animation Set binding keys.
    pub id: MotionSlotId,
    /// Mutable human-readable label shown in graph and set editors.
    pub display_name: String,
}

/// Reads the graph-owned motion-slot list.
///
/// A graph with no [`MOTION_SLOTS_ANNOTATION`] owns no slots.
///
/// # Errors
///
/// Returns a description when an explicitly persisted slot list is malformed.
pub fn animation_graph_motion_slots(graph: &Graph) -> Result<Vec<MotionSlot>, String> {
    let Some(value) = graph.annotations.get(MOTION_SLOTS_ANNOTATION) else {
        return Ok(Vec::new());
    };
    let Value::Array(entries) = value else {
        return Err(format!(
            "`{MOTION_SLOTS_ANNOTATION}` must be an array of slot objects"
        ));
    };
    let mut slots = Vec::with_capacity(entries.len());
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    for entry in entries {
        let Value::Object(fields) = entry else {
            return Err("each Animation Graph motion slot must be an object".to_owned());
        };
        let Some(Value::String(id)) = fields.get("id") else {
            return Err("each Animation Graph motion slot requires a string `id`".to_owned());
        };
        let id = MotionSlotId::from_stable_id(StableId::new(id.trim())).map_err(|_| {
            format!("Animation Graph motion slot `{id}` is not a valid motion_<ULID> identifier")
        })?;
        let Some(Value::String(display_name)) = fields.get("display_name") else {
            return Err(format!(
                "Animation Graph motion slot `{}` requires a string `display_name`",
                id.as_str()
            ));
        };
        let display_name = display_name.trim().to_owned();
        if display_name.is_empty() {
            return Err(format!(
                "Animation Graph motion slot `{}` has a blank display name",
                id.as_str()
            ));
        }
        if !ids.insert(id.clone()) {
            return Err(format!(
                "Animation Graph motion slot `{}` is duplicated",
                id.as_str()
            ));
        }
        if !names.insert(display_name.to_ascii_lowercase()) {
            return Err(format!(
                "Animation Graph motion slot display name `{display_name}` is duplicated"
            ));
        }
        slots.push(MotionSlot { id, display_name });
    }
    Ok(slots)
}

/// Encodes a graph-owned motion-slot list into its semantic annotation value.
pub fn motion_slots_annotation_value(slots: &[MotionSlot]) -> Value {
    Value::Array(
        slots
            .iter()
            .map(|slot| {
                Value::Object(BTreeMap::from([
                    (
                        "display_name".to_owned(),
                        Value::String(slot.display_name.clone()),
                    ),
                    ("id".to_owned(), Value::String(slot.id.as_str().to_owned())),
                ]))
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Stable port IDs (hardcoded Crockford base32 suffixes, see ADR 0033)
// ---------------------------------------------------------------------------

fn fixed_port(suffix: &str) -> PortId {
    PortId::from_stable_id(StableId::new(format!("port_{suffix}")))
        .expect("animation graph port ID must use a valid stable ID")
}

const ENTRY_OUT_SUFFIX: &str = "0000000000000000ANENTRPRT0";
const STATE_IN_SUFFIX: &str = "0000000000000000ANSTATEPRT";
const STATE_OUT_SUFFIX: &str = "0000000000000000ANSTATEPTR";

// ---------------------------------------------------------------------------
// Domain
// ---------------------------------------------------------------------------

/// Animation state machine graph domain (ADR 0033).
///
/// Provides the schema registry and semantic validation for graphs with
/// `kind: "anim.graph"`.  Reuses the `GraphDomain` trait from the graph
/// foundation; does not duplicate any Behavior Tree domain code.
pub struct AnimationGraphDomain {
    graph_kind: GraphKind,
    entry_type: NodeTypeId,
    state_type: NodeTypeId,
    entry_out: PortId,
    state_in: PortId,
    state_out: PortId,
    schemas: BTreeMap<NodeTypeId, NodeSchema>,
}

impl AnimationGraphDomain {
    /// Creates the animation graph domain with its node and port schemas.
    pub fn new() -> Self {
        let graph_kind = GraphKind::new("anim.graph");
        let entry_type = NodeTypeId::new("anim.entry");
        let state_type = NodeTypeId::new("anim.state");
        let transition_value = PortValueTypeId::new("anim.transition");
        let compatible = BTreeSet::from([graph_kind.clone()]);

        let entry_out = fixed_port(ENTRY_OUT_SUFFIX);
        let state_in = fixed_port(STATE_IN_SUFFIX);
        let state_out = fixed_port(STATE_OUT_SUFFIX);

        let mut schemas = BTreeMap::new();

        schemas.insert(
            entry_type.clone(),
            NodeSchema {
                node_type: entry_type.clone(),
                compatible_graph_kinds: compatible.clone(),
                display_name: "Entry".into(),
                description: "Marks the default initial state of the animation graph.".into(),
                category: "Control".into(),
                search_tags: vec!["entry".into(), "start".into(), "initial".into()],
                property_schema: Value::Null,
                ports: BTreeMap::from([(
                    entry_out.clone(),
                    PortSchema {
                        id: entry_out.clone(),
                        name: "out.default".into(),
                        display_name: "To initial state".into(),
                        description: "Connects to the first active state.".into(),
                        direction: PortDirection::Output,
                        value_type: transition_value.clone(),
                        arity: PortArity {
                            min_connections: 0,
                            max_connections: Some(1),
                        },
                    },
                )]),
                version: 1,
            },
        );

        schemas.insert(
            state_type.clone(),
            NodeSchema {
                node_type: state_type.clone(),
                compatible_graph_kinds: compatible,
                display_name: "State".into(),
                description: "An animation state that plays a clip when active.".into(),
                category: "State".into(),
                search_tags: vec!["state".into(), "clip".into(), "animation".into()],
                property_schema: Value::Null,
                ports: BTreeMap::from([
                    (
                        state_in.clone(),
                        PortSchema {
                            id: state_in.clone(),
                            name: "in.default".into(),
                            display_name: "Transitions in".into(),
                            description: "Receives transitions from other states.".into(),
                            direction: PortDirection::Input,
                            value_type: transition_value.clone(),
                            arity: PortArity {
                                min_connections: 0,
                                max_connections: None,
                            },
                        },
                    ),
                    (
                        state_out.clone(),
                        PortSchema {
                            id: state_out.clone(),
                            name: "out.default".into(),
                            display_name: "Transitions out".into(),
                            description: "Sends transitions to other states.".into(),
                            direction: PortDirection::Output,
                            value_type: transition_value,
                            arity: PortArity {
                                min_connections: 0,
                                max_connections: None,
                            },
                        },
                    ),
                ]),
                version: 1,
            },
        );

        Self {
            graph_kind,
            entry_type,
            state_type,
            entry_out,
            state_in,
            state_out,
            schemas,
        }
    }

    /// Returns the `anim.Entry` node type identifier.
    pub fn entry_type(&self) -> &NodeTypeId {
        &self.entry_type
    }

    /// Returns the `anim.State` node type identifier.
    pub fn state_type(&self) -> &NodeTypeId {
        &self.state_type
    }

    /// Returns the stable output port ID for `anim.Entry` nodes.
    pub fn entry_out_port(&self) -> &PortId {
        &self.entry_out
    }

    /// Returns the stable input port ID for `anim.State` nodes.
    pub fn state_in_port(&self) -> &PortId {
        &self.state_in
    }

    /// Returns the stable output port ID for `anim.State` nodes.
    pub fn state_out_port(&self) -> &PortId {
        &self.state_out
    }
}

impl Default for AnimationGraphDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphSchemaRegistry for AnimationGraphDomain {
    fn node_schema(&self, node_type: &NodeTypeId) -> Option<&NodeSchema> {
        self.schemas.get(node_type)
    }
}

impl GraphDomain for AnimationGraphDomain {
    fn graph_kind(&self) -> &GraphKind {
        &self.graph_kind
    }

    fn schema_registry(&self) -> &dyn GraphSchemaRegistry {
        self
    }

    fn validate_domain(&self, graph: &Graph) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        let motion_slots = match animation_graph_motion_slots(graph) {
            Ok(slots) => slots,
            Err(message) => {
                diags.push(Diagnostic::error("anim.invalid_motion_slots", message));
                Vec::new()
            }
        };
        let motion_slot_ids = motion_slots
            .iter()
            .map(|slot| slot.id.clone())
            .collect::<BTreeSet<_>>();
        let has_explicit_motion_slots = graph.annotations.contains_key(MOTION_SLOTS_ANNOTATION);

        let entry_count = graph
            .nodes
            .values()
            .filter(|n| n.node_type == self.entry_type)
            .count();

        match entry_count {
            0 => diags.push(Diagnostic::error(
                "anim.no_entry_node",
                "animation graph must contain exactly one Entry node",
            )),
            1 => {}
            _ => diags.push(Diagnostic::error(
                "anim.multiple_entry_nodes",
                format!(
                    "animation graph contains {entry_count} Entry nodes; exactly one is required"
                ),
            )),
        }

        for node in graph.nodes.values() {
            if node.node_type != self.state_type {
                continue;
            }
            let (motion_slot, playback_mode) = if let Value::Object(ref props) = node.properties {
                (
                    props.get("motion_slot"),
                    props.get(ANIMATION_STATE_PLAYBACK_MODE_PROPERTY),
                )
            } else {
                (None, None)
            };
            let parsed_motion_slot = match motion_slot {
                Some(Value::String(id)) if !id.trim().is_empty() => {
                    match MotionSlotId::from_stable_id(StableId::new(id.trim())) {
                        Ok(id) => Some(id),
                        Err(_) => {
                            diags.push(Diagnostic::error(
                                "anim.invalid_motion_slot",
                                format!(
                                    "animation State motion_slot `{id}` is not a valid motion_<ULID> identifier"
                                ),
                            ));
                            None
                        }
                    }
                }
                Some(Value::String(_)) | None => None,
                Some(_) => {
                    diags.push(Diagnostic::error(
                        "anim.invalid_motion_slot",
                        "animation State motion_slot must be a stable ID string",
                    ));
                    None
                }
            };
            if has_explicit_motion_slots
                && parsed_motion_slot
                    .as_ref()
                    .is_some_and(|id| !motion_slot_ids.contains(id))
            {
                diags.push(Diagnostic::warning(
                    "anim.state_motion_slot_missing",
                    "an animation State references a motion slot that is not owned by this graph",
                ));
            }
            if parsed_motion_slot.is_none() {
                diags.push(Diagnostic::warning(
                    "anim.state_no_motion",
                    "an animation State has no motion_slot; it will play nothing when active",
                ));
            }
            if playback_mode.is_some_and(|value| {
                !matches!(
                    value,
                    Value::String(name)
                        if AnimationStatePlaybackMode::from_persisted_name(name).is_some()
                )
            }) {
                diags.push(Diagnostic::error(
                    "anim.invalid_state_playback_mode",
                    "animation State playback_mode must be `loop` or `once`",
                ));
            }
        }

        for edge in graph.edges.values() {
            if let Some(value) = edge.annotations.get("fade_duration") {
                let valid = match value {
                    Value::F64(value) => value.is_finite() && *value >= 0.0,
                    Value::I64(value) => *value >= 0,
                    Value::U64(_) => true,
                    _ => false,
                };
                if !valid {
                    diags.push(Diagnostic::error(
                        "anim.invalid_transition_fade",
                        "animation transition fade_duration must be a finite non-negative number",
                    ));
                }
            }
        }

        diags
    }
}

// ---------------------------------------------------------------------------
// Compiled output
// ---------------------------------------------------------------------------

/// A single state in a compiled animation graph.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimState {
    /// Graph node that this state originated from.
    pub node_id: NodeId,
    /// Stable motion slot resolved through the selected Animation Set.
    pub motion_slot: Option<MotionSlotId>,
    /// Clip completion behavior for this State.
    ///
    /// A graph that persists no explicit property uses
    /// [`AnimationStatePlaybackMode::Loop`].
    pub playback_mode: AnimationStatePlaybackMode,
}

impl AnimState {
    /// Returns the runtime motion-table key used by this state.
    pub fn motion_key(&self) -> Option<&str> {
        self.motion_slot.as_ref().map(MotionSlotId::as_str)
    }
}

/// A transition edge in a compiled animation graph.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimTransition {
    /// Source state node ID.
    pub from_node: NodeId,
    /// Destination state node ID.
    pub to_node: NodeId,
    /// Condition label.  Empty string means the transition is unconditional.
    pub condition: String,
    /// Optional transition-specific crossfade duration in seconds.
    pub fade_duration: Option<f32>,
}

/// The result of compiling an animation graph (ADR 0033).
#[derive(Debug, Clone)]
pub struct CompiledAnimGraph {
    /// All animation states in the graph.
    pub states: Vec<AnimState>,
    /// All transitions between states.
    pub transitions: Vec<AnimTransition>,
    /// Index into `states` for the entry (default initial) state.
    pub entry_state: usize,
    /// Non-blocking warnings produced during compilation (e.g. disconnected Entry node).
    pub compile_warnings: Vec<Diagnostic>,
}

/// Validates `graph` against `domain` and, if valid, compiles it into a
/// [`CompiledAnimGraph`].
///
/// # Errors
///
/// Returns the blocking diagnostics when validation fails.
pub fn compile_animation_graph(
    domain: &AnimationGraphDomain,
    graph: &Graph,
) -> Result<CompiledAnimGraph, Vec<Diagnostic>> {
    let diags = validate_graph_with_domain(graph, domain);
    if diags.iter().any(Diagnostic::is_blocking) {
        return Err(diags);
    }

    let states: Vec<AnimState> = graph
        .nodes
        .iter()
        .filter(|(_, n)| n.node_type == *domain.state_type())
        .map(|(id, n)| {
            let (motion_slot, playback_mode) = if let Value::Object(ref props) = n.properties {
                let motion_slot = props.get("motion_slot").and_then(|value| match value {
                    Value::String(value) if !value.trim().is_empty() => {
                        MotionSlotId::from_stable_id(StableId::new(value.trim())).ok()
                    }
                    _ => None,
                });
                let playback_mode = props
                    .get(ANIMATION_STATE_PLAYBACK_MODE_PROPERTY)
                    .and_then(|value| match value {
                        Value::String(value) => {
                            AnimationStatePlaybackMode::from_persisted_name(value)
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                (motion_slot, playback_mode)
            } else {
                (None, AnimationStatePlaybackMode::default())
            };
            AnimState {
                node_id: id.clone(),
                motion_slot,
                playback_mode,
            }
        })
        .collect();

    let mut compile_warnings = Vec::new();
    let entry_state = find_entry_state(domain, graph, &states).unwrap_or_else(|| {
        if !states.is_empty() {
            compile_warnings.push(Diagnostic::warning(
                "anim.entry_disconnected",
                "Entry node is not connected to any State; falling back to first state",
            ));
        }
        0
    });

    let transitions = collect_transitions(domain, graph, &states);

    Ok(CompiledAnimGraph {
        states,
        transitions,
        entry_state,
        compile_warnings,
    })
}

fn find_entry_state(
    domain: &AnimationGraphDomain,
    graph: &Graph,
    states: &[AnimState],
) -> Option<usize> {
    let entry_node = graph
        .nodes
        .iter()
        .find(|(_, n)| n.node_type == *domain.entry_type())?;
    let entry_node_id = entry_node.0;

    let connected_state_id = graph.edges.values().find_map(|e: &Edge| {
        if &e.from.node == entry_node_id {
            Some(&e.to.node)
        } else {
            None
        }
    })?;

    states.iter().position(|s| &s.node_id == connected_state_id)
}

fn collect_transitions(
    domain: &AnimationGraphDomain,
    graph: &Graph,
    states: &[AnimState],
) -> Vec<AnimTransition> {
    let state_node_ids: BTreeSet<&NodeId> = states.iter().map(|s| &s.node_id).collect();

    graph
        .edges
        .values()
        .filter(|e| {
            state_node_ids.contains(&e.from.node)
                && state_node_ids.contains(&e.to.node)
                && e.from.port == *domain.state_out_port()
                && e.to.port == *domain.state_in_port()
        })
        .map(|e| {
            let condition = match e.annotations.get("condition") {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            };
            let fade_duration = e
                .annotations
                .get("fade_duration")
                .and_then(|value| match value {
                    Value::F64(value) => Some(*value as f32),
                    Value::I64(value) => Some(*value as f32),
                    Value::U64(value) => Some(*value as f32),
                    _ => None,
                });
            AnimTransition {
                from_node: e.from.node.clone(),
                to_node: e.to.node.clone(),
                condition,
                fade_duration,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Graph, Node, PortRef};
    use crate::id::{EdgeId, GraphId, NodeId};

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

    fn connect(
        graph: &mut Graph,
        from: &NodeId,
        from_port: &PortId,
        to: &NodeId,
        to_port: &PortId,
    ) {
        let eid = EdgeId::generate();
        graph.edges.insert(
            eid.clone(),
            Edge::new(
                eid,
                PortRef::new(from.clone(), from_port.clone()),
                PortRef::new(to.clone(), to_port.clone()),
            ),
        );
    }

    #[test]
    fn validate_no_entry_node_returns_blocking_error() {
        let domain = AnimationGraphDomain::new();
        let graph = empty_graph(&domain);
        let diags = validate_graph_with_domain(&graph, &domain);
        assert!(
            diags.iter().any(Diagnostic::is_blocking),
            "missing Entry node must be a blocking error"
        );
    }

    #[test]
    fn validate_multiple_entry_nodes_returns_error() {
        let domain = AnimationGraphDomain::new();
        let mut graph = empty_graph(&domain);
        add_entry(&mut graph, &domain);
        add_entry(&mut graph, &domain);
        let diags = validate_graph_with_domain(&graph, &domain);
        assert!(diags.iter().any(|d| d.code == "anim.multiple_entry_nodes"));
    }

    #[test]
    fn validate_state_without_motion_emits_warning() {
        let domain = AnimationGraphDomain::new();
        let mut graph = empty_graph(&domain);
        add_entry(&mut graph, &domain);
        add_state(&mut graph, &domain);
        let diags = validate_graph_with_domain(&graph, &domain);
        assert!(
            diags.iter().any(|d| d.code == "anim.state_no_motion"),
            "state without a motion slot must produce a warning"
        );
    }

    #[test]
    fn explicit_slot_list_reports_state_reference_to_missing_slot() {
        let domain = AnimationGraphDomain::new();
        let mut graph = empty_graph(&domain);
        add_entry(&mut graph, &domain);
        let state = add_state(&mut graph, &domain);
        graph.annotations.insert(
            MOTION_SLOTS_ANNOTATION.to_owned(),
            motion_slots_annotation_value(&[]),
        );
        graph.nodes.get_mut(&state).unwrap().properties = Value::Object(BTreeMap::from([(
            "motion_slot".to_owned(),
            Value::String(MotionSlotId::generate().as_str().to_owned()),
        )]));

        let diagnostics = validate_graph_with_domain(&graph, &domain);

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "anim.state_motion_slot_missing"));
    }

    #[test]
    fn compile_valid_graph_returns_compiled() {
        let domain = AnimationGraphDomain::new();
        let mut graph = empty_graph(&domain);
        let entry_id = add_entry(&mut graph, &domain);
        let state_id = add_state(&mut graph, &domain);
        connect(
            &mut graph,
            &entry_id,
            domain.entry_out_port(),
            &state_id,
            domain.state_in_port(),
        );
        let result = compile_animation_graph(&domain, &graph);
        assert!(result.is_ok(), "valid graph must compile without error");
        let compiled = result.unwrap();
        assert_eq!(compiled.states.len(), 1);
        assert_eq!(compiled.entry_state, 0);
        assert_eq!(
            compiled.states[0].playback_mode,
            AnimationStatePlaybackMode::Loop
        );
    }

    #[test]
    fn compile_state_with_once_playback_mode() {
        let domain = AnimationGraphDomain::new();
        let mut graph = empty_graph(&domain);
        let entry_id = add_entry(&mut graph, &domain);
        let state_id = add_state(&mut graph, &domain);
        let Value::Object(properties) = &mut graph
            .nodes
            .get_mut(&state_id)
            .expect("State must exist")
            .properties
        else {
            panic!("Animation State properties must be an object");
        };
        properties.insert(
            ANIMATION_STATE_PLAYBACK_MODE_PROPERTY.to_owned(),
            Value::String("once".to_owned()),
        );
        connect(
            &mut graph,
            &entry_id,
            domain.entry_out_port(),
            &state_id,
            domain.state_in_port(),
        );

        let compiled = compile_animation_graph(&domain, &graph).expect("graph must compile");

        assert_eq!(
            compiled.states[0].playback_mode,
            AnimationStatePlaybackMode::Once
        );
    }

    #[test]
    fn invalid_state_playback_mode_is_blocking() {
        let domain = AnimationGraphDomain::new();
        let mut graph = empty_graph(&domain);
        add_entry(&mut graph, &domain);
        let state_id = add_state(&mut graph, &domain);
        let Value::Object(properties) = &mut graph
            .nodes
            .get_mut(&state_id)
            .expect("State must exist")
            .properties
        else {
            panic!("Animation State properties must be an object");
        };
        properties.insert(
            ANIMATION_STATE_PLAYBACK_MODE_PROPERTY.to_owned(),
            Value::String("repeat_forever".to_owned()),
        );

        let diagnostics = validate_graph_with_domain(&graph, &domain);

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "anim.invalid_state_playback_mode"));
    }

    #[test]
    fn compile_missing_entry_returns_err() {
        let domain = AnimationGraphDomain::new();
        let graph = empty_graph(&domain);
        assert!(
            compile_animation_graph(&domain, &graph).is_err(),
            "graph without Entry node must not compile"
        );
    }

    #[test]
    fn transitions_collected_between_states() {
        let domain = AnimationGraphDomain::new();
        let mut graph = empty_graph(&domain);
        let entry_id = add_entry(&mut graph, &domain);
        let state_a = add_state(&mut graph, &domain);
        let state_b = add_state(&mut graph, &domain);
        connect(
            &mut graph,
            &entry_id,
            domain.entry_out_port(),
            &state_a,
            domain.state_in_port(),
        );
        connect(
            &mut graph,
            &state_a,
            domain.state_out_port(),
            &state_b,
            domain.state_in_port(),
        );
        graph
            .edges
            .values_mut()
            .find(|edge| edge.from.node == state_a)
            .expect("state transition edge")
            .annotations
            .insert("fade_duration".into(), Value::F64(0.35));
        let compiled = compile_animation_graph(&domain, &graph).unwrap();
        assert_eq!(compiled.transitions.len(), 1);
        assert_eq!(compiled.transitions[0].from_node, state_a);
        assert_eq!(compiled.transitions[0].to_node, state_b);
        assert_eq!(compiled.transitions[0].fade_duration, Some(0.35));
    }

    #[test]
    fn invalid_transition_fade_duration_is_blocking() {
        let domain = AnimationGraphDomain::new();
        let mut graph = empty_graph(&domain);
        let entry = add_entry(&mut graph, &domain);
        let state_a = add_state(&mut graph, &domain);
        let state_b = add_state(&mut graph, &domain);
        connect(
            &mut graph,
            &entry,
            domain.entry_out_port(),
            &state_a,
            domain.state_in_port(),
        );
        connect(
            &mut graph,
            &state_a,
            domain.state_out_port(),
            &state_b,
            domain.state_in_port(),
        );
        graph
            .edges
            .values_mut()
            .find(|edge| edge.from.node == state_a)
            .expect("state transition edge")
            .annotations
            .insert("fade_duration".into(), Value::F64(-0.1));

        let diagnostics = validate_graph_with_domain(&graph, &domain);

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "anim.invalid_transition_fade"));
    }
}
