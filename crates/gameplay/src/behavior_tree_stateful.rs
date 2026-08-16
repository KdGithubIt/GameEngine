//! Stateful Behavior Tree execution and runtime debugging for ADR 0123.

use crate::behavior_tree::{
    BehaviorDispatchKind, BehaviorStatus, BehaviorTreeBehaviorRegistry, BehaviorTreeContext,
    BehaviorTreeRuntimeError, MAX_BEHAVIOR_TREE_RUNTIME_DEPTH,
};
use engine_authoring::{
    BehaviorTreeNodeKind, CompiledBehaviorNode, CompiledBehaviorTree, GraphId, NodeId, Value,
};
use engine_ecs::{Query, ResMut};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::fmt;

/// Default hard node-step budget for one tick.
pub const DEFAULT_BEHAVIOR_TREE_STEP_BUDGET: usize = 4096;
/// Default number of recent debug transitions retained by an instance.
pub const DEFAULT_BEHAVIOR_TREE_DEBUG_CAPACITY: usize = 128;

/// Reason an active execution was reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorAbortReason {
    /// Runner was disabled.
    RunnerDisabled,
    /// Caller explicitly requested reset.
    ExplicitReset,
    /// Compiled tree generation was replaced.
    TreeReplaced,
    /// Owning runner/entity is being removed.
    RunnerRemoved,
    /// Runtime policy interrupted the branch.
    Interrupted,
}

/// Stateful action lifecycle extension over the existing gameplay dispatch boundary.
pub trait StatefulBehaviorTreeContext: BehaviorTreeContext {
    /// Called once when an action becomes active.
    fn action_enter(&mut self, _node: &NodeId, _behavior_id: &str) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Called once when an active action becomes terminal.
    fn action_complete(
        &mut self,
        _node: &NodeId,
        _behavior_id: &str,
        _status: BehaviorStatus,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Called once when an active action is aborted.
    fn action_abort(
        &mut self,
        _node: &NodeId,
        _behavior_id: &str,
        _reason: BehaviorAbortReason,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl StatefulBehaviorTreeContext for BehaviorTreeBehaviorRegistry {}

/// Invalid compiled shape rejected while preparing a runtime plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorTreePlanError {
    /// Source node associated with the invalid shape.
    pub node: NodeId,
    /// Human-readable structural reason.
    pub reason: String,
}

impl fmt::Display for BehaviorTreePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.reason)
    }
}

impl Error for BehaviorTreePlanError {}

/// Stateful execution failure.
#[derive(Debug)]
pub enum StatefulBehaviorTreeError<E> {
    /// Invalid prepared plan.
    Plan(BehaviorTreePlanError),
    /// Existing action/condition dispatch failure.
    Runtime(BehaviorTreeRuntimeError<E>),
    /// Action lifecycle hook failed.
    Lifecycle {
        /// Source action node.
        node: NodeId,
        /// Stable action behavior identifier.
        behavior_id: String,
        /// Context-owned failure.
        source: E,
    },
    /// Hard per-tick node-step budget was exhausted.
    StepBudgetExceeded {
        /// Source node that would exceed the budget.
        node: NodeId,
        /// Configured budget.
        budget: usize,
    },
}

impl<E: fmt::Display> fmt::Display for StatefulBehaviorTreeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plan(source) => source.fmt(formatter),
            Self::Runtime(source) => source.fmt(formatter),
            Self::Lifecycle {
                behavior_id, source, ..
            } => write!(
                formatter,
                "behavior tree action `{behavior_id}` lifecycle failed: {source}"
            ),
            Self::StepBudgetExceeded { budget, .. } => {
                write!(formatter, "behavior tree tick exceeded node-step budget {budget}")
            }
        }
    }
}

impl<E: Error + 'static> Error for StatefulBehaviorTreeError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Plan(source) => Some(source),
            Self::Runtime(source) => Some(source),
            Self::Lifecycle { source, .. } => Some(source),
            Self::StepBudgetExceeded { .. } => None,
        }
    }
}

/// Immutable deterministic pre-order execution plan.
#[derive(Debug, Clone)]
pub struct PreparedBehaviorTree {
    source: GraphId,
    nodes: Vec<PreparedNode>,
}

#[derive(Debug, Clone)]
struct PreparedNode {
    source: NodeId,
    kind: BehaviorTreeNodeKind,
    behavior: Option<String>,
    children: Vec<usize>,
    depth: u32,
}

impl PreparedBehaviorTree {
    /// Validates and flattens a compiled tree.
    pub fn prepare(tree: &CompiledBehaviorTree) -> Result<Self, BehaviorTreePlanError> {
        if tree.root.kind != BehaviorTreeNodeKind::Root {
            return Err(plan_error(
                &tree.root,
                format!(
                    "behavior tree compiled root must be Root, found {:?}",
                    tree.root.kind
                ),
            ));
        }
        let mut plan = Self {
            source: tree.source.clone(),
            nodes: Vec::new(),
        };
        plan.push(&tree.root, 0, true)?;
        Ok(plan)
    }

    /// Source graph identity.
    #[must_use]
    pub fn source(&self) -> &GraphId {
        &self.source
    }

    /// Number of runtime plan nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    fn push(
        &mut self,
        node: &CompiledBehaviorNode,
        depth: u32,
        root: bool,
    ) -> Result<usize, BehaviorTreePlanError> {
        validate_node(node, depth, root)?;
        let index = self.nodes.len();
        self.nodes.push(PreparedNode {
            source: node.source.clone(),
            kind: node.kind,
            behavior: node.behavior.clone(),
            children: Vec::new(),
            depth,
        });
        let mut children = Vec::with_capacity(node.children.len());
        for child in &node.children {
            children.push(self.push(child, depth + 1, false)?);
        }
        self.nodes[index].children = children;
        Ok(index)
    }
}

fn plan_error(node: &CompiledBehaviorNode, reason: String) -> BehaviorTreePlanError {
    BehaviorTreePlanError {
        node: node.source.clone(),
        reason,
    }
}

fn validate_node(
    node: &CompiledBehaviorNode,
    depth: u32,
    root: bool,
) -> Result<(), BehaviorTreePlanError> {
    if depth > MAX_BEHAVIOR_TREE_RUNTIME_DEPTH {
        return Err(plan_error(
            node,
            format!(
                "behavior tree compiled depth exceeds runtime limit {MAX_BEHAVIOR_TREE_RUNTIME_DEPTH}"
            ),
        ));
    }
    if !root && node.kind == BehaviorTreeNodeKind::Root {
        return Err(plan_error(
            node,
            "behavior tree root node may only appear at tree root".into(),
        ));
    }
    match node.kind {
        BehaviorTreeNodeKind::Root if node.children.len() != 1 => Err(plan_error(
            node,
            format!(
                "behavior tree root must have exactly one child, found {}",
                node.children.len()
            ),
        )),
        BehaviorTreeNodeKind::Sequence | BehaviorTreeNodeKind::Selector
            if node.children.is_empty() =>
        {
            Err(plan_error(
                node,
                format!(
                    "behavior tree {:?} composite must have at least one child",
                    node.kind
                ),
            ))
        }
        BehaviorTreeNodeKind::Condition | BehaviorTreeNodeKind::Action
            if !node.children.is_empty() =>
        {
            Err(plan_error(
                node,
                format!(
                    "behavior tree {:?} leaf must not have children, found {}",
                    node.kind,
                    node.children.len()
                ),
            ))
        }
        BehaviorTreeNodeKind::Decorator if node.children.len() != 1 => Err(plan_error(
            node,
            format!(
                "behavior tree decorator must have exactly one child, found {}",
                node.children.len()
            ),
        )),
        BehaviorTreeNodeKind::Condition
        | BehaviorTreeNodeKind::Action
        | BehaviorTreeNodeKind::Decorator
            if node.behavior.as_deref().map_or(true, str::is_empty) =>
        {
            Err(plan_error(
                node,
                format!("behavior tree {:?} node is missing behavior id", node.kind),
            ))
        }
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, Default)]
struct NodeState {
    cursor: usize,
    active: bool,
    action_entered: bool,
    elapsed_seconds: f32,
}

/// Debug transition kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorTransitionKind {
    /// Node became active.
    Enter,
    /// Node became terminal.
    Exit,
    /// Active action was aborted.
    Abort,
    /// Instance state was reset.
    Reset,
}

/// Bounded recent transition.
#[derive(Debug, Clone)]
pub struct BehaviorTransition {
    /// Execution generation.
    pub generation: u64,
    /// Source node.
    pub node: NodeId,
    /// Transition kind.
    pub kind: BehaviorTransitionKind,
    /// Terminal status for exit transitions.
    pub status: Option<BehaviorStatus>,
    /// Abort/reset reason when relevant.
    pub reason: Option<BehaviorAbortReason>,
}

/// Active node elapsed-time state.
#[derive(Debug, Clone)]
pub struct BehaviorNodeExecutionState {
    /// Source node.
    pub node: NodeId,
    /// Seconds continuously active.
    pub elapsed_seconds: f32,
}

/// Observation-only snapshot shared by debugging adapters.
#[derive(Debug, Clone)]
pub struct BehaviorInstanceSnapshot {
    /// Source tree identity.
    pub tree: GraphId,
    /// Execution generation.
    pub generation: u64,
    /// Latest overall status.
    pub status: Option<BehaviorStatus>,
    /// Active source-node path in deterministic plan order.
    pub active_path: Vec<NodeId>,
    /// Current running leaf, when present.
    pub running_leaf: Option<NodeId>,
    /// Active-node elapsed state.
    pub active_nodes: Vec<BehaviorNodeExecutionState>,
    /// Most recent terminal overall status.
    pub last_terminal_status: Option<BehaviorStatus>,
    /// Most recent abort/reset reason.
    pub last_reason: Option<BehaviorAbortReason>,
    /// Bounded transition history.
    pub recent_transitions: Vec<BehaviorTransition>,
}

/// Mutable per-runner Behavior Tree execution state.
pub struct BehaviorTreeInstance {
    plan: PreparedBehaviorTree,
    states: Vec<NodeState>,
    generation: u64,
    step_budget: usize,
    debug_capacity: usize,
    transitions: VecDeque<BehaviorTransition>,
    last_status: Option<BehaviorStatus>,
    last_terminal_status: Option<BehaviorStatus>,
    last_reason: Option<BehaviorAbortReason>,
}

impl BehaviorTreeInstance {
    /// Creates an independent instance from one compiled tree.
    pub fn new(tree: CompiledBehaviorTree) -> Result<Self, BehaviorTreePlanError> {
        Ok(Self::from_plan(PreparedBehaviorTree::prepare(&tree)?))
    }

    /// Creates an independent instance from a reusable prepared plan.
    #[must_use]
    pub fn from_plan(plan: PreparedBehaviorTree) -> Self {
        let states = vec![NodeState::default(); plan.node_count()];
        Self {
            plan,
            states,
            generation: 0,
            step_budget: DEFAULT_BEHAVIOR_TREE_STEP_BUDGET,
            debug_capacity: DEFAULT_BEHAVIOR_TREE_DEBUG_CAPACITY,
            transitions: VecDeque::new(),
            last_status: None,
            last_terminal_status: None,
            last_reason: None,
        }
    }

    /// Sets the hard node-step budget. Zero is clamped to one.
    pub fn set_step_budget(&mut self, budget: usize) {
        self.step_budget = budget.max(1);
    }

    /// Sets bounded transition history capacity.
    pub fn set_debug_capacity(&mut self, capacity: usize) {
        self.debug_capacity = capacity;
        self.transitions.truncate(capacity);
    }

    /// Immutable plan used by this instance.
    #[must_use]
    pub fn plan(&self) -> &PreparedBehaviorTree {
        &self.plan
    }

    /// Resumes execution from current memory-composite cursors.
    pub fn tick<C: StatefulBehaviorTreeContext>(
        &mut self,
        context: &mut C,
        delta_seconds: f32,
    ) -> Result<BehaviorStatus, StatefulBehaviorTreeError<C::Error>> {
        let mut steps = 0;
        let status = self.tick_node(0, context, delta_seconds.max(0.0), &mut steps)?;
        self.last_status = Some(status);
        if status != BehaviorStatus::Running {
            self.last_terminal_status = Some(status);
        }
        Ok(status)
    }

    /// Aborts active actions deepest-first, then clears mutable cursor state.
    pub fn reset<C: StatefulBehaviorTreeContext>(
        &mut self,
        context: &mut C,
        reason: BehaviorAbortReason,
    ) -> Result<(), StatefulBehaviorTreeError<C::Error>> {
        let mut active: Vec<_> = self
            .plan
            .nodes
            .iter()
            .enumerate()
            .filter(|(index, node)| {
                node.kind == BehaviorTreeNodeKind::Action && self.states[*index].action_entered
            })
            .map(|(index, node)| (index, node.depth))
            .collect();
        active.sort_by(|left, right| right.1.cmp(&left.1).then(right.0.cmp(&left.0)));
        for (index, _) in active {
            let node = self.plan.nodes[index].clone();
            let behavior_id = node.behavior.as_deref().expect("validated action behavior");
            context
                .action_abort(&node.source, behavior_id, reason)
                .map_err(|source| StatefulBehaviorTreeError::Lifecycle {
                    node: node.source.clone(),
                    behavior_id: behavior_id.to_owned(),
                    source,
                })?;
            self.record(node.source, BehaviorTransitionKind::Abort, None, Some(reason));
        }
        self.states.fill(NodeState::default());
        self.generation = self.generation.wrapping_add(1);
        self.last_status = None;
        self.last_reason = Some(reason);
        let root = self.plan.nodes[0].source.clone();
        self.record(root, BehaviorTransitionKind::Reset, None, Some(reason));
        Ok(())
    }

    /// Validates a replacement first, then aborts the old generation and swaps plans.
    pub fn replace_tree<C: StatefulBehaviorTreeContext>(
        &mut self,
        context: &mut C,
        tree: CompiledBehaviorTree,
    ) -> Result<(), StatefulBehaviorTreeError<C::Error>> {
        let plan = PreparedBehaviorTree::prepare(&tree).map_err(StatefulBehaviorTreeError::Plan)?;
        self.reset(context, BehaviorAbortReason::TreeReplaced)?;
        self.plan = plan;
        self.states = vec![NodeState::default(); self.plan.node_count()];
        Ok(())
    }

    /// Returns current read-only runtime state.
    #[must_use]
    pub fn snapshot(&self) -> BehaviorInstanceSnapshot {
        let mut active_path = Vec::new();
        let mut active_nodes = Vec::new();
        let mut running_leaf = None;
        for (index, node) in self.plan.nodes.iter().enumerate() {
            let state = &self.states[index];
            if state.active {
                active_path.push(node.source.clone());
                active_nodes.push(BehaviorNodeExecutionState {
                    node: node.source.clone(),
                    elapsed_seconds: state.elapsed_seconds,
                });
                if node.children.is_empty() {
                    running_leaf = Some(node.source.clone());
                }
            }
        }
        BehaviorInstanceSnapshot {
            tree: self.plan.source.clone(),
            generation: self.generation,
            status: self.last_status,
            active_path,
            running_leaf,
            active_nodes,
            last_terminal_status: self.last_terminal_status,
            last_reason: self.last_reason,
            recent_transitions: self.transitions.iter().cloned().collect(),
        }
    }

    fn tick_node<C: StatefulBehaviorTreeContext>(
        &mut self,
        index: usize,
        context: &mut C,
        delta_seconds: f32,
        steps: &mut usize,
    ) -> Result<BehaviorStatus, StatefulBehaviorTreeError<C::Error>> {
        let node = self.plan.nodes[index].clone();
        if *steps >= self.step_budget {
            return Err(StatefulBehaviorTreeError::StepBudgetExceeded {
                node: node.source,
                budget: self.step_budget,
            });
        }
        *steps += 1;
        self.enter(index, delta_seconds);
        match node.kind {
            BehaviorTreeNodeKind::Root | BehaviorTreeNodeKind::Decorator => {
                let status = self.tick_node(node.children[0], context, delta_seconds, steps)?;
                if status != BehaviorStatus::Running {
                    self.finish(index, status);
                }
                Ok(status)
            }
            BehaviorTreeNodeKind::Sequence => {
                self.tick_sequence(index, &node, context, delta_seconds, steps)
            }
            BehaviorTreeNodeKind::Selector => {
                self.tick_selector(index, &node, context, delta_seconds, steps)
            }
            BehaviorTreeNodeKind::Condition => {
                let behavior_id = node.behavior.as_deref().expect("validated condition behavior");
                let status = context.check_condition(behavior_id).map_err(|source| {
                    StatefulBehaviorTreeError::Runtime(
                        BehaviorTreeRuntimeError::BehaviorDispatchFailed {
                            dispatch: BehaviorDispatchKind::Condition,
                            behavior_id: behavior_id.to_owned(),
                            source,
                        },
                    )
                })?;
                if status != BehaviorStatus::Running {
                    self.finish(index, status);
                }
                Ok(status)
            }
            BehaviorTreeNodeKind::Action => self.tick_action(index, &node, context),
        }
    }

    fn tick_sequence<C: StatefulBehaviorTreeContext>(
        &mut self,
        index: usize,
        node: &PreparedNode,
        context: &mut C,
        delta: f32,
        steps: &mut usize,
    ) -> Result<BehaviorStatus, StatefulBehaviorTreeError<C::Error>> {
        while self.states[index].cursor < node.children.len() {
            let cursor = self.states[index].cursor;
            match self.tick_node(node.children[cursor], context, delta, steps)? {
                BehaviorStatus::Success => self.states[index].cursor += 1,
                BehaviorStatus::Running => return Ok(BehaviorStatus::Running),
                BehaviorStatus::Failure => {
                    self.states[index].cursor = 0;
                    self.finish(index, BehaviorStatus::Failure);
                    return Ok(BehaviorStatus::Failure);
                }
            }
        }
        self.states[index].cursor = 0;
        self.finish(index, BehaviorStatus::Success);
        Ok(BehaviorStatus::Success)
    }

    fn tick_selector<C: StatefulBehaviorTreeContext>(
        &mut self,
        index: usize,
        node: &PreparedNode,
        context: &mut C,
        delta: f32,
        steps: &mut usize,
    ) -> Result<BehaviorStatus, StatefulBehaviorTreeError<C::Error>> {
        while self.states[index].cursor < node.children.len() {
            let cursor = self.states[index].cursor;
            match self.tick_node(node.children[cursor], context, delta, steps)? {
                BehaviorStatus::Failure => self.states[index].cursor += 1,
                BehaviorStatus::Running => return Ok(BehaviorStatus::Running),
                BehaviorStatus::Success => {
                    self.states[index].cursor = 0;
                    self.finish(index, BehaviorStatus::Success);
                    return Ok(BehaviorStatus::Success);
                }
            }
        }
        self.states[index].cursor = 0;
        self.finish(index, BehaviorStatus::Failure);
        Ok(BehaviorStatus::Failure)
    }

    fn tick_action<C: StatefulBehaviorTreeContext>(
        &mut self,
        index: usize,
        node: &PreparedNode,
        context: &mut C,
    ) -> Result<BehaviorStatus, StatefulBehaviorTreeError<C::Error>> {
        let behavior_id = node.behavior.as_deref().expect("validated action behavior");
        if !self.states[index].action_entered {
            context
                .action_enter(&node.source, behavior_id)
                .map_err(|source| StatefulBehaviorTreeError::Lifecycle {
                    node: node.source.clone(),
                    behavior_id: behavior_id.to_owned(),
                    source,
                })?;
            self.states[index].action_entered = true;
        }
        let status = context.tick_action(behavior_id).map_err(|source| {
            StatefulBehaviorTreeError::Runtime(BehaviorTreeRuntimeError::BehaviorDispatchFailed {
                dispatch: BehaviorDispatchKind::Action,
                behavior_id: behavior_id.to_owned(),
                source,
            })
        })?;
        if status != BehaviorStatus::Running {
            context
                .action_complete(&node.source, behavior_id, status)
                .map_err(|source| StatefulBehaviorTreeError::Lifecycle {
                    node: node.source.clone(),
                    behavior_id: behavior_id.to_owned(),
                    source,
                })?;
            self.states[index].action_entered = false;
            self.finish(index, status);
        }
        Ok(status)
    }

    fn enter(&mut self, index: usize, delta: f32) {
        if self.states[index].active {
            self.states[index].elapsed_seconds += delta;
        } else {
            self.states[index].active = true;
            self.states[index].elapsed_seconds = 0.0;
            let node = self.plan.nodes[index].source.clone();
            self.record(node, BehaviorTransitionKind::Enter, None, None);
        }
    }

    fn finish(&mut self, index: usize, status: BehaviorStatus) {
        self.states[index].active = false;
        self.states[index].elapsed_seconds = 0.0;
        let node = self.plan.nodes[index].source.clone();
        self.record(node, BehaviorTransitionKind::Exit, Some(status), None);
    }

    fn record(
        &mut self,
        node: NodeId,
        kind: BehaviorTransitionKind,
        status: Option<BehaviorStatus>,
        reason: Option<BehaviorAbortReason>,
    ) {
        if self.debug_capacity == 0 {
            return;
        }
        while self.transitions.len() >= self.debug_capacity {
            self.transitions.pop_front();
        }
        self.transitions.push_back(BehaviorTransition {
            generation: self.generation,
            node,
            kind,
            status,
            reason,
        });
    }
}

/// Shared observation shape for a stateful ECS runner.
#[derive(Debug, Clone)]
pub struct BehaviorExecutionSnapshot {
    /// Stateful executor state.
    pub execution: BehaviorInstanceSnapshot,
    /// Runtime blackboard.
    pub blackboard: BTreeMap<String, Value>,
    /// Latest runtime error.
    pub error: Option<String>,
}

/// ECS component owning one independent stateful Behavior Tree instance.
pub struct StatefulBehaviorTreeRunner {
    instance: BehaviorTreeInstance,
    blackboard: BTreeMap<String, Value>,
    enabled: bool,
    pending_abort: Option<BehaviorAbortReason>,
    last_error: Option<String>,
}

impl StatefulBehaviorTreeRunner {
    /// Creates a runner.
    pub fn new(tree: CompiledBehaviorTree) -> Result<Self, BehaviorTreePlanError> {
        Self::with_blackboard(tree, BTreeMap::new())
    }

    /// Creates a runner with blackboard defaults.
    pub fn with_blackboard(
        tree: CompiledBehaviorTree,
        blackboard: BTreeMap<String, Value>,
    ) -> Result<Self, BehaviorTreePlanError> {
        Ok(Self {
            instance: BehaviorTreeInstance::new(tree)?,
            blackboard,
            enabled: true,
            pending_abort: None,
            last_error: None,
        })
    }

    /// Current execution instance.
    #[must_use]
    pub fn instance(&self) -> &BehaviorTreeInstance {
        &self.instance
    }

    /// Mutable execution instance.
    pub fn instance_mut(&mut self) -> &mut BehaviorTreeInstance {
        &mut self.instance
    }

    /// Runtime blackboard.
    #[must_use]
    pub fn blackboard(&self) -> &BTreeMap<String, Value> {
        &self.blackboard
    }

    /// Mutable runtime blackboard.
    pub fn blackboard_mut(&mut self) -> &mut BTreeMap<String, Value> {
        &mut self.blackboard
    }

    /// Enables/disables the runner; disable queues an abort.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled && !enabled {
            self.pending_abort = Some(BehaviorAbortReason::RunnerDisabled);
        }
        self.enabled = enabled;
    }

    /// Whether normal execution is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Applies queued lifecycle work even when the runner is disabled.
    pub fn synchronize_lifecycle<C: StatefulBehaviorTreeContext>(
        &mut self,
        context: &mut C,
    ) -> Result<(), StatefulBehaviorTreeError<C::Error>> {
        if let Some(reason) = self.pending_abort.take() {
            self.instance.reset(context, reason)?;
        }
        Ok(())
    }

    /// Resumes this runner for one tick.
    pub fn tick<C: StatefulBehaviorTreeContext>(
        &mut self,
        context: &mut C,
        delta_seconds: f32,
    ) -> Result<BehaviorStatus, StatefulBehaviorTreeError<C::Error>> {
        self.synchronize_lifecycle(context)?;
        let result = self.instance.tick(context, delta_seconds);
        match &result {
            Ok(_) => self.last_error = None,
            Err(error) => self.last_error = Some(error.to_string()),
        }
        result
    }

    /// Explicitly resets the runner.
    pub fn reset<C: StatefulBehaviorTreeContext>(
        &mut self,
        context: &mut C,
    ) -> Result<(), StatefulBehaviorTreeError<C::Error>> {
        self.instance
            .reset(context, BehaviorAbortReason::ExplicitReset)
    }

    /// Replaces the compiled tree generation.
    pub fn replace_tree<C: StatefulBehaviorTreeContext>(
        &mut self,
        context: &mut C,
        tree: CompiledBehaviorTree,
    ) -> Result<(), StatefulBehaviorTreeError<C::Error>> {
        self.instance.replace_tree(context, tree)
    }

    /// Current observation-only snapshot.
    #[must_use]
    pub fn snapshot(&self) -> BehaviorExecutionSnapshot {
        BehaviorExecutionSnapshot {
            execution: self.instance.snapshot(),
            blackboard: self.blackboard.clone(),
            error: self.last_error.clone(),
        }
    }
}

/// Minimal ECS system for stateful runners.
///
/// The registry has no authoritative frame-delta source, so this adapter uses
/// zero elapsed time. Production scheduling can call [`BehaviorTreeInstance::tick`]
/// with its frame delta without changing gameplay dispatch semantics.
pub fn stateful_behavior_tree_tick_system(
    mut runners: Query<&mut StatefulBehaviorTreeRunner>,
    mut registry: ResMut<BehaviorTreeBehaviorRegistry>,
) {
    registry.clear_calls();
    for (_, runner) in &mut runners {
        if runner.synchronize_lifecycle(&mut *registry).is_err() || !runner.is_enabled() {
            continue;
        }
        let _ = runner.tick(&mut *registry, 0.0);
    }
}

/// Registers the minimal stateful runner ECS system.
pub fn register_stateful_behavior_tree_system(
    app: &mut engine_ecs::App,
) -> Result<&mut engine_ecs::App, engine_ecs::SystemBuildError> {
    if app
        .world()
        .get_resource::<BehaviorTreeBehaviorRegistry>()
        .is_none()
    {
        app.insert_resource(BehaviorTreeBehaviorRegistry::default());
    }
    app.try_add_system(stateful_behavior_tree_tick_system)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Debug)]
    struct TestError;

    impl fmt::Display for TestError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("test")
        }
    }

    impl Error for TestError {}

    #[derive(Default)]
    struct Context {
        actions: BTreeMap<String, VecDeque<BehaviorStatus>>,
        calls: Vec<String>,
        lifecycle: Vec<String>,
    }

    impl BehaviorTreeContext for Context {
        type Error = TestError;

        fn tick_action(&mut self, id: &str) -> Result<BehaviorStatus, Self::Error> {
            self.calls.push(id.into());
            Ok(self
                .actions
                .get_mut(id)
                .and_then(VecDeque::pop_front)
                .unwrap_or(BehaviorStatus::Success))
        }

        fn check_condition(&mut self, id: &str) -> Result<BehaviorStatus, Self::Error> {
            self.calls.push(id.into());
            Ok(BehaviorStatus::Success)
        }
    }

    impl StatefulBehaviorTreeContext for Context {
        fn action_enter(&mut self, _: &NodeId, id: &str) -> Result<(), Self::Error> {
            self.lifecycle.push(format!("enter:{id}"));
            Ok(())
        }

        fn action_complete(
            &mut self,
            _: &NodeId,
            id: &str,
            status: BehaviorStatus,
        ) -> Result<(), Self::Error> {
            self.lifecycle.push(format!("complete:{id}:{status:?}"));
            Ok(())
        }

        fn action_abort(
            &mut self,
            _: &NodeId,
            id: &str,
            reason: BehaviorAbortReason,
        ) -> Result<(), Self::Error> {
            self.lifecycle.push(format!("abort:{id}:{reason:?}"));
            Ok(())
        }
    }

    fn node(
        kind: BehaviorTreeNodeKind,
        behavior: Option<&str>,
        children: Vec<CompiledBehaviorNode>,
    ) -> CompiledBehaviorNode {
        CompiledBehaviorNode {
            source: NodeId::generate(),
            kind,
            behavior: behavior.map(str::to_owned),
            children,
        }
    }

    fn tree(child: CompiledBehaviorNode) -> CompiledBehaviorTree {
        CompiledBehaviorTree {
            source: GraphId::generate(),
            root: node(BehaviorTreeNodeKind::Root, None, vec![child]),
        }
    }

    #[test]
    fn sequence_resumes_running_child() {
        let mut instance = BehaviorTreeInstance::new(tree(node(
            BehaviorTreeNodeKind::Sequence,
            None,
            vec![
                node(BehaviorTreeNodeKind::Action, Some("a"), vec![]),
                node(BehaviorTreeNodeKind::Action, Some("b"), vec![]),
            ],
        )))
        .unwrap();
        let mut context = Context::default();
        context.actions.insert(
            "b".into(),
            VecDeque::from([BehaviorStatus::Running, BehaviorStatus::Success]),
        );
        assert_eq!(
            instance.tick(&mut context, 0.0).unwrap(),
            BehaviorStatus::Running
        );
        assert_eq!(
            instance.tick(&mut context, 0.0).unwrap(),
            BehaviorStatus::Success
        );
        assert_eq!(context.calls, vec!["a", "b", "b"]);
    }

    #[test]
    fn lifecycle_enter_complete_and_abort_are_exact() {
        let mut instance = BehaviorTreeInstance::new(tree(node(
            BehaviorTreeNodeKind::Action,
            Some("run"),
            vec![],
        )))
        .unwrap();
        let mut context = Context::default();
        context.actions.insert(
            "run".into(),
            VecDeque::from([BehaviorStatus::Running, BehaviorStatus::Success]),
        );
        assert_eq!(
            instance.tick(&mut context, 0.0).unwrap(),
            BehaviorStatus::Running
        );
        assert_eq!(
            instance.tick(&mut context, 0.0).unwrap(),
            BehaviorStatus::Success
        );
        context.actions.insert(
            "run".into(),
            VecDeque::from([BehaviorStatus::Running]),
        );
        assert_eq!(
            instance.tick(&mut context, 0.0).unwrap(),
            BehaviorStatus::Running
        );
        instance
            .reset(&mut context, BehaviorAbortReason::ExplicitReset)
            .unwrap();
        assert_eq!(
            context.lifecycle,
            vec![
                "enter:run",
                "complete:run:Success",
                "enter:run",
                "abort:run:ExplicitReset"
            ]
        );
    }

    #[test]
    fn prepared_plan_can_back_independent_instances() {
        let compiled = tree(node(
            BehaviorTreeNodeKind::Sequence,
            None,
            vec![
                node(BehaviorTreeNodeKind::Action, Some("a"), vec![]),
                node(BehaviorTreeNodeKind::Action, Some("b"), vec![]),
            ],
        ));
        let plan = PreparedBehaviorTree::prepare(&compiled).unwrap();
        let left = BehaviorTreeInstance::from_plan(plan.clone());
        let right = BehaviorTreeInstance::from_plan(plan);
        assert_eq!(left.plan().source(), right.plan().source());
        assert_eq!(left.snapshot().generation, 0);
        assert_eq!(right.snapshot().generation, 0);
    }

    #[test]
    fn step_budget_is_hard_and_source_mapped() {
        let mut instance = BehaviorTreeInstance::new(tree(node(
            BehaviorTreeNodeKind::Action,
            Some("a"),
            vec![],
        )))
        .unwrap();
        instance.set_step_budget(1);
        let mut context = Context::default();
        assert!(matches!(
            instance.tick(&mut context, 0.0),
            Err(StatefulBehaviorTreeError::StepBudgetExceeded { budget: 1, .. })
        ));
    }
}
