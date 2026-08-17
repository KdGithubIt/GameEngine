//! Animation state machine runtime (Phase 59, ADR 0033).
//!
//! [`AnimGraphPlayer`] wraps an [`engine_authoring::CompiledAnimGraph`] with
//! fixed-step runtime state, typed Bool/Float/Trigger parameters, and the
//! motion-slot-to-clip table supplied by scene conversion. Persisted transition
//! expressions use explicit typed syntax.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use engine_authoring::{
    compile_animation_graph, AnimState, AnimTransition, AnimationGraphDomain,
    AnimationStatePlaybackMode, AssetId, CompiledAnimGraph, Diagnostic, EdgeId, Graph, GraphId,
    MotionSlotId, MotionSourceRef, MotionSourceVariant,
};

use crate::animation::{AnimationClip, Animator};
use crate::animation_parameters::{
    AnimationParameterError, AnimationParameterKind, AnimationParameterValue, AnimationParameters,
};
use crate::asset::Handle;

/// Comparison operator used by one floating-point transition condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationFloatComparison {
    /// Parameter is less than the threshold.
    Less,
    /// Parameter is less than or equal to the threshold.
    LessOrEqual,
    /// Parameter is greater than the threshold.
    Greater,
    /// Parameter is greater than or equal to the threshold.
    GreaterOrEqual,
    /// Parameter is exactly equal to the threshold.
    Equal,
    /// Parameter is not equal to the threshold.
    NotEqual,
}

impl AnimationFloatComparison {
    #[allow(clippy::float_cmp)]
    fn evaluate(self, value: f32, threshold: f32) -> bool {
        match self {
            Self::Less => value < threshold,
            Self::LessOrEqual => value <= threshold,
            Self::Greater => value > threshold,
            Self::GreaterOrEqual => value >= threshold,
            Self::Equal => value == threshold,
            Self::NotEqual => value != threshold,
        }
    }
}

/// Parsed condition attached to one Animation Graph transition.
#[derive(Debug, Clone, PartialEq)]
pub enum AnimationTransitionCondition {
    /// Transition is taken on the next graph tick.
    Always,
    /// Persistent boolean parameter must match `expected`.
    Bool {
        /// Stable parameter name.
        parameter: String,
        /// Required value.
        expected: bool,
    },
    /// Persistent float parameter must satisfy one comparison.
    Float {
        /// Stable parameter name.
        parameter: String,
        /// Comparison operator.
        comparison: AnimationFloatComparison,
        /// Finite comparison threshold.
        threshold: f32,
    },
    /// One-shot parameter must be pending and is consumed when selected.
    Trigger {
        /// Stable parameter name.
        parameter: String,
    },
}

impl AnimationTransitionCondition {
    /// Parses a persisted transition expression.
    ///
    /// Blank is unconditional. Accepted forms are explicit Bool equality such
    /// as `grounded == false`, Float comparisons such as `speed >= 0.1`, and
    /// `trigger attack`.
    ///
    /// # Errors
    ///
    /// Returns an error for a removed shorthand, blank parameter name,
    /// malformed comparison, or non-finite float threshold.
    pub fn parse(expression: &str) -> Result<Self, AnimationTransitionConditionError> {
        let expression = expression.trim();
        if expression.is_empty() {
            return Ok(Self::Always);
        }
        if let Some(parameter) = expression.strip_prefix("trigger ") {
            return Ok(Self::Trigger {
                parameter: validated_transition_parameter(parameter)?,
            });
        }

        for (token, comparison) in [
            (">=", AnimationFloatComparison::GreaterOrEqual),
            ("<=", AnimationFloatComparison::LessOrEqual),
            ("==", AnimationFloatComparison::Equal),
            ("!=", AnimationFloatComparison::NotEqual),
            (">", AnimationFloatComparison::Greater),
            ("<", AnimationFloatComparison::Less),
        ] {
            let Some((left, right)) = expression.split_once(token) else {
                continue;
            };
            let parameter = validated_transition_parameter(left)?;
            let right = right.trim();
            if right.is_empty() {
                return Err(AnimationTransitionConditionError::MissingComparisonValue);
            }
            let bool_expected = match comparison {
                AnimationFloatComparison::Equal => parse_bool_literal(right),
                AnimationFloatComparison::NotEqual => parse_bool_literal(right).map(|value| !value),
                AnimationFloatComparison::Less
                | AnimationFloatComparison::LessOrEqual
                | AnimationFloatComparison::Greater
                | AnimationFloatComparison::GreaterOrEqual => None,
            };
            if let Some(expected) = bool_expected {
                return Ok(Self::Bool {
                    parameter,
                    expected,
                });
            }
            let threshold = right.parse::<f32>().map_err(|_| {
                AnimationTransitionConditionError::InvalidComparisonValue(right.to_owned())
            })?;
            if !threshold.is_finite() {
                return Err(AnimationTransitionConditionError::NonFiniteThreshold(
                    threshold,
                ));
            }
            return Ok(Self::Float {
                parameter,
                comparison,
                threshold,
            });
        }

        Err(AnimationTransitionConditionError::UnsupportedExpression(
            expression.to_owned(),
        ))
    }

    /// Returns whether the current parameter table satisfies this condition.
    pub fn is_satisfied(&self, parameters: &AnimationParameters) -> bool {
        match self {
            Self::Always => true,
            Self::Bool {
                parameter,
                expected,
            } => parameters.bool(parameter).is_ok_and(|value| value == *expected),
            Self::Float {
                parameter,
                comparison,
                threshold,
            } => parameters
                .float(parameter)
                .is_ok_and(|value| comparison.evaluate(value, *threshold)),
            Self::Trigger { parameter } => parameters.trigger_pending(parameter).unwrap_or(false),
        }
    }

    fn consume_selected_trigger(
        &self,
        parameters: &mut AnimationParameters,
    ) -> Result<(), AnimationParameterError> {
        if let Self::Trigger { parameter } = self {
            parameters.consume_trigger(parameter)?;
        }
        Ok(())
    }
}

/// Invalid persisted Animation Graph transition condition.
#[derive(Debug, Clone, PartialEq)]
pub enum AnimationTransitionConditionError {
    /// The expression uses a removed shorthand instead of explicit typed syntax.
    UnsupportedExpression(String),
    /// A condition selected a type but omitted its parameter name.
    BlankParameter,
    /// A comparison operator had no value on its right side.
    MissingComparisonValue,
    /// The comparison value was neither a supported boolean nor a float.
    InvalidComparisonValue(String),
    /// Float thresholds must not be NaN or infinite.
    NonFiniteThreshold(f32),
}

impl fmt::Display for AnimationTransitionConditionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedExpression(expression) => write!(
                formatter,
                "animation transition expression `{expression}` must use explicit Bool equality, a Float comparison, or `trigger NAME`"
            ),
            Self::BlankParameter => {
                write!(formatter, "animation transition parameter must not be blank")
            }
            Self::MissingComparisonValue => write!(
                formatter,
                "animation transition comparison value must not be blank"
            ),
            Self::InvalidComparisonValue(value) => write!(
                formatter,
                "animation transition comparison value `{value}` must be `true`, `false`, or a finite float"
            ),
            Self::NonFiniteThreshold(value) => write!(
                formatter,
                "animation transition float threshold must be finite, found {value}"
            ),
        }
    }
}

impl std::error::Error for AnimationTransitionConditionError {}

fn validated_transition_parameter(
    parameter: &str,
) -> Result<String, AnimationTransitionConditionError> {
    let parameter = parameter.trim();
    if parameter.is_empty() {
        Err(AnimationTransitionConditionError::BlankParameter)
    } else {
        Ok(parameter.to_owned())
    }
}

fn parse_bool_literal(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Read-only provenance for one resolved Animation Set motion slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationMotionDebugBinding {
    /// Stable graph-owned motion slot.
    pub motion_slot: MotionSlotId,
    /// Human-readable binding label captured from the Animation Set.
    pub display_name: String,
    /// Author-selected stable motion source.
    pub source: MotionSourceRef,
    /// Variant that scene conversion actually resolved.
    pub resolved_variant: MotionSourceVariant,
    /// Session-local concrete clip handle identity.
    pub resolved_clip_runtime_id: u64,
}

/// Stable source mapping captured when an Animation Controller is converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationGraphDebugSource {
    /// Animation Graph asset referenced by the controller.
    pub graph_asset: AssetId,
    /// Stable semantic graph identity loaded from that asset.
    pub graph_id: GraphId,
    /// Animation Set asset resolved with the graph.
    pub animation_set_asset: AssetId,
    /// Source edge IDs in the same deterministic order as compiled transitions.
    pub transition_edges: Vec<Option<EdgeId>>,
    /// Resolved motion evidence keyed by stable MotionSlotId.
    pub motion_bindings: BTreeMap<MotionSlotId, AnimationMotionDebugBinding>,
}

/// Runs a compiled Animation Graph against a sibling [`Animator`] component.
pub struct AnimGraphPlayer {
    graph: CompiledAnimGraph,
    transition_conditions:
        Vec<Result<AnimationTransitionCondition, AnimationTransitionConditionError>>,
    clips: BTreeMap<String, Handle<AnimationClip>>,
    current_state: usize,
    parameters: AnimationParameters,
    /// Default crossfade duration used when an edge has no override.
    pub fade_duration: f32,
    entered: bool,
    last_transition: Option<AnimTransition>,
    last_transition_index: Option<usize>,
    transition_sequence: u64,
    debug_source: Option<AnimationGraphDebugSource>,
}

impl AnimGraphPlayer {
    /// Creates a player at the graph entry state with an empty parameter table.
    ///
    /// The entry state's clip starts during the first [`anim_graph_system`]
    /// pass. Transition conditions are parsed once here and reused every tick.
    pub fn new(graph: CompiledAnimGraph, clips: BTreeMap<String, Handle<AnimationClip>>) -> Self {
        let current_state = graph.entry_state;
        let transition_conditions = graph
            .transitions
            .iter()
            .map(|transition| {
                let parsed = AnimationTransitionCondition::parse(&transition.condition);
                if let Err(error) = &parsed {
                    log::warn!(
                        "anim_graph: transition condition `{}` is invalid: {error}",
                        transition.condition
                    );
                }
                parsed
            })
            .collect();
        Self {
            graph,
            transition_conditions,
            clips,
            current_state,
            parameters: AnimationParameters::new(),
            fade_duration: 0.2,
            entered: false,
            last_transition: None,
            last_transition_index: None,
            transition_sequence: 0,
            debug_source: None,
        }
    }

    /// Creates a player with a predeclared typed parameter table.
    pub fn with_parameters(
        graph: CompiledAnimGraph,
        clips: BTreeMap<String, Handle<AnimationClip>>,
        parameters: AnimationParameters,
    ) -> Self {
        let mut player = Self::new(graph, clips);
        player.parameters = parameters;
        player
    }

    /// Sets or creates a persistent boolean parameter.
    pub fn set_bool_parameter(
        &mut self,
        name: impl Into<String>,
        value: bool,
    ) -> Result<(), AnimationParameterError> {
        self.parameters.set_bool(name, value)
    }

    /// Sets or creates a persistent finite float parameter.
    pub fn set_float_parameter(
        &mut self,
        name: impl Into<String>,
        value: f32,
    ) -> Result<(), AnimationParameterError> {
        self.parameters.set_float(name, value)
    }

    /// Sets a one-shot trigger which remains pending until a matching edge wins.
    pub fn trigger_parameter(
        &mut self,
        name: impl Into<String>,
    ) -> Result<(), AnimationParameterError> {
        self.parameters.trigger(name)
    }

    /// Returns one parameter's stable type when it has been declared or written.
    pub fn parameter_kind(&self, name: &str) -> Option<AnimationParameterKind> {
        self.parameters
            .iter()
            .find_map(|(candidate, value)| (candidate == name).then_some(value.kind()))
    }

    /// Returns one parameter's current typed value.
    pub fn parameter_value(&self, name: &str) -> Option<AnimationParameterValue> {
        self.parameters
            .iter()
            .find_map(|(candidate, value)| (candidate == name).then_some(value))
    }

    /// Iterates all typed parameters in deterministic name order.
    pub fn parameters(&self) -> impl Iterator<Item = (&str, AnimationParameterValue)> {
        self.parameters.iter()
    }

    /// Returns the active state's index in the compiled graph.
    pub fn current_state(&self) -> usize {
        self.current_state
    }

    /// Returns the currently active compiled state when the graph is non-empty.
    pub fn current_state_info(&self) -> Option<&AnimState> {
        self.graph.states.get(self.current_state)
    }

    /// Restores the entry state while preserving current typed parameters.
    pub fn restart(&mut self) {
        self.current_state = self.graph.entry_state;
        self.entered = false;
        self.last_transition = None;
        self.last_transition_index = None;
        self.transition_sequence = 0;
    }

    /// Returns the latest accepted transition.
    pub fn last_transition(&self) -> Option<&AnimTransition> {
        self.last_transition.as_ref()
    }

    /// Returns the stable source edge for the latest accepted transition.
    pub fn last_transition_edge(&self) -> Option<&EdgeId> {
        let index = self.last_transition_index?;
        self.debug_source.as_ref()?.transition_edges.get(index)?.as_ref()
    }

    /// Installs read-only source and binding provenance captured by scene conversion.
    pub fn set_debug_source(&mut self, source: AnimationGraphDebugSource) {
        self.debug_source = Some(source);
    }

    /// Returns source and binding provenance for runtime observation.
    pub fn debug_source(&self) -> Option<&AnimationGraphDebugSource> {
        self.debug_source.as_ref()
    }

    /// Monotonic counter incremented for every accepted transition.
    pub fn transition_sequence(&self) -> u64 {
        self.transition_sequence
    }

    /// Starts a motion registered in this player's resolved motion table.
    pub fn play_clip(&self, animator: &mut Animator, motion_key: &str, fade_duration: f32) -> bool {
        let Some(clip) = self.clip_handle(motion_key) else {
            return false;
        };
        animator.crossfade_to(clip, fade_duration);
        true
    }

    /// Returns the clip handle registered for an authored motion key.
    pub fn clip_handle(&self, motion_key: &str) -> Option<Handle<AnimationClip>> {
        self.clips.get(motion_key).copied()
    }

    /// Iterates resolved motion bindings in deterministic key order.
    pub fn clip_bindings(&self) -> impl Iterator<Item = (&str, Handle<AnimationClip>)> {
        self.clips
            .iter()
            .map(|(motion_key, handle)| (motion_key.as_str(), *handle))
    }

    /// Replaces every binding that points at `old` with `new`.
    pub fn replace_clip_handle(&mut self, old: Handle<AnimationClip>, new: Handle<AnimationClip>) {
        for handle in self.clips.values_mut() {
            if *handle == old {
                *handle = new;
            }
        }
    }

    /// Iterates resolved motion keys in deterministic order.
    pub fn clip_ids(&self) -> impl Iterator<Item = &str> {
        self.clips.keys().map(String::as_str)
    }
}

/// Walks each graph player and starts at most one transition per fixed tick.
///
/// Transition expressions support blank/unconditional, explicit Bool equality,
/// Float comparisons, and one-shot `trigger NAME`. A selected trigger is
/// consumed only after its target state has been resolved successfully.
pub fn anim_graph_system(mut query: engine_ecs::Query<(&mut AnimGraphPlayer, &mut Animator)>) {
    for (_entity, (player, animator)) in query.iter_mut() {
        if !player.entered {
            player.entered = true;
            if let Some(state) = player.graph.states.get(player.current_state).cloned() {
                start_state_clip(&player.clips, animator, &state, 0.0);
            }
            continue;
        }

        let Some(transition_index) = find_transition_index(player) else {
            continue;
        };
        let transition = player.graph.transitions[transition_index].clone();
        let Some(target_index) = player
            .graph
            .states
            .iter()
            .position(|state| state.node_id == transition.to_node)
        else {
            log::warn!(
                "anim_graph: transition target node {:?} is not among the compiled states",
                transition.to_node
            );
            continue;
        };

        if let Ok(condition) = &player.transition_conditions[transition_index] {
            condition
                .consume_selected_trigger(&mut player.parameters)
                .expect("a satisfied trigger condition must retain its trigger type");
        }
        player.current_state = target_index;
        player.last_transition = Some(transition.clone());
        player.last_transition_index = Some(transition_index);
        player.transition_sequence = player.transition_sequence.wrapping_add(1);
        let state = player.graph.states[target_index].clone();
        let fade_duration = transition.fade_duration.unwrap_or(player.fade_duration);
        start_state_clip(&player.clips, animator, &state, fade_duration);
    }
}

fn find_transition_index(player: &AnimGraphPlayer) -> Option<usize> {
    let current_node = &player.graph.states.get(player.current_state)?.node_id;
    player
        .graph
        .transitions
        .iter()
        .enumerate()
        .find_map(|(index, transition)| {
            (&transition.from_node == current_node
                && player
                    .transition_conditions
                    .get(index)
                    .and_then(|result| result.as_ref().ok())
                    .is_some_and(|condition| condition.is_satisfied(&player.parameters)))
            .then_some(index)
        })
}

fn start_state_clip(
    clips: &BTreeMap<String, Handle<AnimationClip>>,
    animator: &mut Animator,
    state: &AnimState,
    fade_duration: f32,
) {
    let Some(motion_key) = state.motion_key() else {
        log::warn!(
            "anim_graph: state {:?} has no motion slot; playing nothing",
            state.node_id
        );
        return;
    };
    let Some(&handle) = clips.get(motion_key) else {
        log::warn!(
            "anim_graph: state {:?} motion key '{motion_key}' is not present in the resolved motion table; playing nothing",
            state.node_id
        );
        return;
    };
    animator.crossfade_to(handle, fade_duration);
    match state.playback_mode {
        AnimationStatePlaybackMode::Loop => animator.set_looping(true),
        AnimationStatePlaybackMode::Once => animator.set_looping(false),
    }
}

/// Describes why [`load_animation_graph`] failed.
#[derive(Debug)]
pub enum AnimGraphLoadError {
    /// Reading the graph file failed.
    Io(std::io::Error),
    /// The file was not valid graph JSON.
    Parse(serde_json::Error),
    /// Structural, domain, or typed-condition validation failed.
    Compile(Vec<Diagnostic>),
}

impl fmt::Display for AnimGraphLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "failed to read animation graph file: {error}"),
            Self::Parse(error) => write!(
                formatter,
                "animation graph file is not valid graph JSON: {error}"
            ),
            Self::Compile(diagnostics) => {
                write!(
                    formatter,
                    "animation graph failed to compile ({} diagnostic(s)):",
                    diagnostics.len()
                )?;
                for diagnostic in diagnostics {
                    write!(formatter, " [{}] {}", diagnostic.code, diagnostic.message)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for AnimGraphLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Parse(error) => Some(error),
            Self::Compile(_) => None,
        }
    }
}

/// Loads, compiles, and validates one `anim.graph` document.
///
/// # Errors
///
/// Returns an I/O, JSON parse, graph compilation, or typed transition-condition
/// diagnostic when the asset cannot become a runnable state machine.
pub fn load_animation_graph(path: &Path) -> Result<CompiledAnimGraph, AnimGraphLoadError> {
    load_animation_graph_document(path).map(|(_, compiled)| compiled)
}

/// Compiles and validates one in-memory `anim.graph` JSON snapshot.
///
/// Editor hosts use this for unsaved working copies so runtime composition does
/// not need a temporary file and cannot silently fall back to an older saved graph.
///
/// # Errors
///
/// Returns a JSON parse, graph compilation, or typed transition-condition diagnostic.
pub fn load_animation_graph_json(json: &str) -> Result<CompiledAnimGraph, AnimGraphLoadError> {
    load_animation_graph_document_json(json).map(|(_, compiled)| compiled)
}

/// Loads the semantic source graph together with its compiled runtime artifact.
///
/// The semantic graph is returned only as read-only provenance for callers that
/// must preserve stable `GraphId`/`EdgeId` mappings while constructing runtime
/// observation metadata. Runtime execution continues to use `CompiledAnimGraph`.
pub fn load_animation_graph_document(
    path: &Path,
) -> Result<(Graph, CompiledAnimGraph), AnimGraphLoadError> {
    let json = std::fs::read_to_string(path).map_err(AnimGraphLoadError::Io)?;
    load_animation_graph_document_json(&json)
}

/// Parses, compiles, and validates an in-memory semantic Animation Graph.
///
/// # Errors
///
/// Returns a JSON parse, graph compilation, or typed transition-condition diagnostic.
pub fn load_animation_graph_document_json(
    json: &str,
) -> Result<(Graph, CompiledAnimGraph), AnimGraphLoadError> {
    let graph: Graph = serde_json::from_str(json).map_err(AnimGraphLoadError::Parse)?;
    let domain = AnimationGraphDomain::new();
    let compiled =
        compile_animation_graph(&domain, &graph).map_err(AnimGraphLoadError::Compile)?;
    let diagnostics = compiled
        .transitions
        .iter()
        .filter_map(|transition| {
            AnimationTransitionCondition::parse(&transition.condition)
                .err()
                .map(|error| {
                    Diagnostic::error(
                        "anim.invalid_transition_condition",
                        format!(
                            "animation transition condition `{}` is invalid: {error}",
                            transition.condition
                        ),
                    )
                })
        })
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        Ok((graph, compiled))
    } else {
        Err(AnimGraphLoadError::Compile(diagnostics))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::animation::{AnimChannel, AnimProperty, AnimatorState, Keyframe};
    use crate::asset::Assets;
    use engine_authoring::graph::{Edge, Graph as AuthoringGraph, Node, PortRef};
    use engine_authoring::id::{EdgeId, GraphId, MotionSlotId, NodeId};
    use engine_authoring::value::Value;
    use engine_authoring::GraphDomain;
    use engine_ecs::{App, Entity, World};
    use std::collections::BTreeMap as StdBTreeMap;

    fn domain() -> AnimationGraphDomain {
        AnimationGraphDomain::new()
    }

    fn empty_graph(domain: &AnimationGraphDomain) -> AuthoringGraph {
        AuthoringGraph::new(GraphId::generate(), domain.graph_kind().clone(), "test")
    }

    fn add_entry(graph: &mut AuthoringGraph, domain: &AnimationGraphDomain) -> NodeId {
        let id = NodeId::generate();
        graph.nodes.insert(
            id.clone(),
            Node::new(
                id.clone(),
                domain.entry_type().clone(),
                Value::Object(StdBTreeMap::new()),
            ),
        );
        id
    }

    fn motion_slot_for(name: &str) -> MotionSlotId {
        let ulid = match name {
            "idle" => "01JP0000000000000000000001",
            "run" => "01JP0000000000000000000002",
            "attack" => "01JP0000000000000000000003",
            "unmapped" => "01JP0000000000000000000004",
            other => panic!("unknown fixture motion name: {other}"),
        };
        MotionSlotId::from_stable_id(engine_authoring::id::StableId::new(format!(
            "motion_{ulid}"
        )))
        .expect("fixture motion slot must be valid")
    }

    fn add_state(
        graph: &mut AuthoringGraph,
        domain: &AnimationGraphDomain,
        motion_name: Option<&str>,
    ) -> NodeId {
        let id = NodeId::generate();
        let mut properties = StdBTreeMap::new();
        if let Some(motion_name) = motion_name {
            properties.insert(
                "motion_slot".to_owned(),
                Value::String(motion_slot_for(motion_name).as_str().to_owned()),
            );
            properties.insert(
                "motion_name".to_owned(),
                Value::String(motion_name.to_owned()),
            );
        }
        graph.nodes.insert(
            id.clone(),
            Node::new(
                id.clone(),
                domain.state_type().clone(),
                Value::Object(properties),
            ),
        );
        id
    }

    fn connect(
        graph: &mut AuthoringGraph,
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
        graph: &mut AuthoringGraph,
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

    fn one_second_clip() -> AnimationClip {
        AnimationClip {
            duration: 1.0,
            channels: vec![AnimChannel {
                property: AnimProperty::Translation,
                target_bone: None,
                keyframes: vec![Keyframe {
                    time: 0.0,
                    value: [0.0, 0.0, 0.0, 1.0],
                }],
            }],
            morph_channels: Vec::new(),
            events: vec![],
            skeleton: None,
            skeleton_identity: None,
            root_bone: None,
            contacts: Vec::new(),
        }
    }

    fn run(world: &mut World) {
        let mut app = App::new();
        std::mem::swap(app.world_mut(), world);
        app.add_system(anim_graph_system);
        app.update().expect("animation graph system must run");
        std::mem::swap(app.world_mut(), world);
    }

    fn two_state_world(condition: &str) -> (World, Entity, Handle<AnimationClip>, usize) {
        let domain = domain();
        let mut graph = empty_graph(&domain);
        let entry = add_entry(&mut graph, &domain);
        let idle_state = add_state(&mut graph, &domain, Some("idle"));
        let run_state = add_state(&mut graph, &domain, Some("run"));
        connect(
            &mut graph,
            &entry,
            domain.entry_out_port(),
            &idle_state,
            domain.state_in_port(),
        );
        connect_states(&mut graph, &domain, &idle_state, &run_state, condition);
        let compiled = compile_animation_graph(&domain, &graph).expect("graph must compile");
        let run_index = compiled
            .states
            .iter()
            .position(|state| state.node_id == run_state)
            .unwrap();

        let mut assets = Assets::<AnimationClip>::default();
        let idle = assets.add(one_second_clip());
        let run_clip = assets.add(one_second_clip());
        let mut clips = BTreeMap::new();
        clips.insert(motion_slot_for("idle").as_str().to_owned(), idle);
        clips.insert(motion_slot_for("run").as_str().to_owned(), run_clip);

        let mut world = World::new();
        let entity = world.spawn_with(Animator::playing(idle)).unwrap();
        world
            .add_component(entity, AnimGraphPlayer::new(compiled, clips))
            .unwrap();
        (world, entity, run_clip, run_index)
    }

    #[test]
    fn first_pass_starts_entry_clip_without_transitioning() {
        let (mut world, entity, _, _) = two_state_world("");
        run(&mut world);
        assert_eq!(
            world
                .get_component::<AnimGraphPlayer>(entity)
                .unwrap()
                .transition_sequence(),
            0
        );
        assert_eq!(
            world.get_component::<Animator>(entity).unwrap().state,
            AnimatorState::Playing
        );
    }

    #[test]
    fn removed_bool_shorthands_are_rejected() {
        for expression in ["moving", "!grounded"] {
            assert!(matches!(
                AnimationTransitionCondition::parse(expression),
                Err(AnimationTransitionConditionError::UnsupportedExpression(_))
            ));
        }
    }

    #[test]
    fn bool_and_float_conditions_drive_transitions() {
        let (mut bool_world, bool_entity, _, bool_target) = two_state_world("moving == true");
        run(&mut bool_world);
        run(&mut bool_world);
        assert_ne!(
            bool_world
                .get_component::<AnimGraphPlayer>(bool_entity)
                .unwrap()
                .current_state(),
            bool_target
        );
        bool_world
            .get_component_mut::<AnimGraphPlayer>(bool_entity)
            .unwrap()
            .set_bool_parameter("moving", true)
            .unwrap();
        run(&mut bool_world);
        assert_eq!(
            bool_world
                .get_component::<AnimGraphPlayer>(bool_entity)
                .unwrap()
                .current_state(),
            bool_target
        );

        let (mut float_world, float_entity, _, float_target) = two_state_world("speed > 0.1");
        float_world
            .get_component_mut::<AnimGraphPlayer>(float_entity)
            .unwrap()
            .set_float_parameter("speed", 0.5)
            .unwrap();
        run(&mut float_world);
        run(&mut float_world);
        assert_eq!(
            float_world
                .get_component::<AnimGraphPlayer>(float_entity)
                .unwrap()
                .current_state(),
            float_target
        );
    }

    #[test]
    fn false_bool_and_trigger_conditions_work() {
        let (mut bool_world, bool_entity, _, bool_target) = two_state_world("grounded == false");
        bool_world
            .get_component_mut::<AnimGraphPlayer>(bool_entity)
            .unwrap()
            .set_bool_parameter("grounded", false)
            .unwrap();
        run(&mut bool_world);
        run(&mut bool_world);
        assert_eq!(
            bool_world
                .get_component::<AnimGraphPlayer>(bool_entity)
                .unwrap()
                .current_state(),
            bool_target
        );

        let (mut trigger_world, trigger_entity, _, trigger_target) =
            two_state_world("trigger attack");
        trigger_world
            .get_component_mut::<AnimGraphPlayer>(trigger_entity)
            .unwrap()
            .trigger_parameter("attack")
            .unwrap();
        run(&mut trigger_world);
        run(&mut trigger_world);
        let player = trigger_world
            .get_component::<AnimGraphPlayer>(trigger_entity)
            .unwrap();
        assert_eq!(player.current_state(), trigger_target);
        assert_eq!(
            player.parameter_value("attack"),
            Some(AnimationParameterValue::Trigger(false))
        );
    }

    #[test]
    fn parameter_types_remain_stable() {
        let (mut world, entity, _, _) = two_state_world("speed > 0.1");
        let player = world
            .get_component_mut::<AnimGraphPlayer>(entity)
            .unwrap();
        player.set_float_parameter("speed", 1.0).unwrap();
        assert!(matches!(
            player.set_bool_parameter("speed", true),
            Err(AnimationParameterError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn positive_fade_starts_crossfade() {
        let (mut world, entity, run_clip, _) = two_state_world("");
        world
            .get_component_mut::<AnimGraphPlayer>(entity)
            .unwrap()
            .fade_duration = 0.5;
        run(&mut world);
        run(&mut world);
        let animator = world.get_component::<Animator>(entity).unwrap();
        assert_eq!(animator.clip, run_clip);
        assert!(animator.is_fading());
    }

    #[test]
    fn load_rejects_malformed_typed_condition() {
        let domain = domain();
        let mut graph = empty_graph(&domain);
        let entry = add_entry(&mut graph, &domain);
        let idle = add_state(&mut graph, &domain, Some("idle"));
        let run_state = add_state(&mut graph, &domain, Some("run"));
        connect(
            &mut graph,
            &entry,
            domain.entry_out_port(),
            &idle,
            domain.state_in_port(),
        );
        connect_states(&mut graph, &domain, &idle, &run_state, "speed > nope");
        let json = graph.to_canonical_json(&domain).unwrap();
        let path = std::env::temp_dir().join(format!(
            "typed_anim_graph_bad_condition_{}.json",
            std::process::id()
        ));
        std::fs::write(&path, json).unwrap();
        let result = load_animation_graph(&path);
        let _ = std::fs::remove_file(&path);
        assert!(matches!(result, Err(AnimGraphLoadError::Compile(_))));
    }

    #[test]
    fn typed_condition_parser_accepts_bool_float_and_trigger_forms() {
        assert_eq!(
            AnimationTransitionCondition::parse("grounded == false").unwrap(),
            AnimationTransitionCondition::Bool {
                parameter: "grounded".to_owned(),
                expected: false,
            }
        );
        assert!(matches!(
            AnimationTransitionCondition::parse("speed >= 0.1").unwrap(),
            AnimationTransitionCondition::Float {
                comparison: AnimationFloatComparison::GreaterOrEqual,
                ..
            }
        ));
        assert_eq!(
            AnimationTransitionCondition::parse("trigger attack").unwrap(),
            AnimationTransitionCondition::Trigger {
                parameter: "attack".to_owned(),
            }
        );
    }
}
