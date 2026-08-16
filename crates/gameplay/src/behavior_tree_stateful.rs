//! Stateful runtime execution and observation for compiled Behavior Trees.
//!
//! Authoring graphs remain immutable [`engine_authoring::CompiledBehaviorTree`] artifacts. A
//! [`crate::behavior_tree::BehaviorTreeExecutor`] prepares that artifact once into compact runtime
//! indices, while each [`crate::behavior_tree::BehaviorTreeInstance`] owns mutable execution state.

use engine_authoring::{
    BehaviorTreeNodeKind, CompiledBehaviorNode, CompiledBehaviorTree, GraphId, NodeId, Value,
};
use engine_ecs::{Query, ResMut};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::error::Error;
use std::fmt;

/// Maximum compiled Behavior Tree depth accepted by the runtime executor.
pub const MAX_BEHAVIOR_TREE_RUNTIME_DEPTH: u32 = 1024;
/// Default upper bound on node visits performed by one tree tick.
pub const DEFAULT_BEHAVIOR_TREE_STEP_BUDGET: usize = 4096;
/// Maximum number of recent execution transitions retained for debugging.
pub const MAX_BEHAVIOR_TREE_DEBUG_TRANSITIONS: usize = 64;

const INVERTER_BEHAVIOR: &str = "engine.inverter";
const WAIT_BEHAVIOR_PREFIX: &str = "engine.wait:";
const TIMEOUT_BEHAVIOR_PREFIX: &str = "engine.timeout:";
const COOLDOWN_BEHAVIOR_PREFIX: &str = "engine.cooldown:";

/// Runtime result of ticking one Behavior Tree node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorStatus {
    /// The node completed successfully.
    Success,
    /// The node completed unsuccessfully.
    Failure,
    /// The node is still executing and should be ticked again later.
    Running,
}

/// Identifies which external behavior dispatch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorDispatchKind {
    /// An action dispatch failed.
    Action,
    /// A condition dispatch failed.
    Condition,
}

/// Explains why active Behavior Tree work was aborted or reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorResetReason {
    /// The runner was disabled.
    RunnerDisabled,
    /// The caller explicitly reset the runner.
    ExplicitReset,
    /// A new compiled tree replaced the current tree generation.
    TreeReplaced,
    /// A timeout decorator interrupted its running child subtree.
    Timeout,
    /// A future higher-priority interruption requested cancellation.
    Interrupted,
    /// The runner component is being removed.
    RunnerRemoved,
    /// The owning runtime entity is being despawned.
    EntityDespawned,
}

/// Kind of lifecycle transition retained by runtime debugging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorExecutionTransitionKind {
    /// A long-running action was entered.
    Enter,
    /// A node completed with success.
    Success,
    /// A node completed with failure.
    Failure,
    /// Active work was aborted before normal completion.
    Abort,
    /// The instance state was reset.
    Reset,
}

/// One bounded runtime transition used by debugger snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorExecutionTransition {
    /// Execution generation in which this transition occurred.
    pub generation: u64,
    /// Source authoring node when the transition belongs to one node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeId>,
    /// Stable behavior identifier for action transitions, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behavior_id: Option<String>,
    /// Transition category.
    pub kind: BehaviorExecutionTransitionKind,
    /// Reset or abort reason when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<BehaviorResetReason>,
}

/// One currently active node and its accumulated active time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorActiveNodeSnapshot {
    /// Stable source authoring node identifier.
    pub node: NodeId,
    /// Time accumulated while this node has remained active.
    pub elapsed_seconds: f64,
}

/// Read-only bounded execution state shared by debugger integrations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorExecutionSnapshot {
    /// Source authoring graph of the compiled tree.
    pub tree_source: GraphId,
    /// Monotonic tree generation incremented when the runner changes tree.
    pub tree_generation: u64,
    /// Monotonic execution generation incremented by explicit resets.
    pub execution_generation: u64,
    /// Most recent overall tick status, if a tick completed normally.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<BehaviorStatus>,
    /// Current root-to-running-node path using source authoring IDs.
    pub active_path: Vec<BehaviorActiveNodeSnapshot>,
    /// Deepest currently running node, which may be a stateful decorator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running_node: Option<NodeId>,
    /// Most recently completed node.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_terminal_node: Option<NodeId>,
    /// Most recently observed terminal status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_terminal_status: Option<BehaviorStatus>,
    /// Most recent reset or abort reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_reset_reason: Option<BehaviorResetReason>,
    /// Bounded recent execution transition history.
    pub recent_transitions: Vec<BehaviorExecutionTransition>,
    /// Runtime blackboard owned by the runner.
    pub blackboard: BTreeMap<String, Value>,
    /// Most recent execution error rendered for observation surfaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// External behavior dispatch surface used by [`BehaviorTreeExecutor`].
///
/// Implementors own gameplay meaning of action and condition behavior IDs.
/// Lifecycle methods default to no-ops so existing integrations remain source
/// compatible while long-running actions can opt into balanced cleanup.
pub trait BehaviorTreeContext {
    /// Error type produced by this context's behavior dispatch implementation.
    type Error: Error + Send + Sync + 'static;

    /// Called exactly once before an action begins its first runtime tick.
    fn action_enter(&mut self, _node: &NodeId, _behavior_id: &str) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Ticks an action behavior.
    fn tick_action(&mut self, behavior_id: &str) -> Result<BehaviorStatus, Self::Error>;

    /// Called exactly once when an entered action completes normally.
    fn action_complete(
        &mut self,
        _node: &NodeId,
        _behavior_id: &str,
        _status: BehaviorStatus,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Called exactly once when an entered action is aborted before completion.
    fn action_abort(
        &mut self,
        _node: &NodeId,
        _behavior_id: &str,
        _reason: BehaviorResetReason,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Checks a condition behavior.
    fn check_condition(&mut self, behavior_id: &str) -> Result<BehaviorStatus, Self::Error>;

    /// Returns the deterministic time step consumed by stateful decorators.
    ///
    /// Runtime hosts SHOULD override this with their fixed or frame delta. The
    /// default preserves useful standalone behavior for existing contexts.
    fn behavior_delta_seconds(&self) -> f64 {
        1.0 / 60.0
    }
}

/// Runtime errors produced by Behavior Tree execution.
#[derive(Debug)]
pub enum BehaviorTreeRuntimeError<E> {
    /// The compiled tree root field did not contain a root node.
    InvalidRootKind {
        /// Source authoring node.
        node: NodeId,
        /// Runtime node kind found at the compiled tree root.
        kind: BehaviorTreeNodeKind,
    },
    /// A non-root node had root kind.
    NestedRoot {
        /// Source authoring node.
        node: NodeId,
    },
    /// A root node must have exactly one child.
    InvalidRootChildCount {
        /// Source authoring node.
        node: NodeId,
        /// Number of children found in the compiled tree.
        count: usize,
    },
    /// A decorator node must have exactly one child.
    InvalidDecoratorChildCount {
        /// Source authoring node.
        node: NodeId,
        /// Number of children found in the compiled tree.
        count: usize,
    },
    /// A leaf action or condition has child nodes.
    LeafHasChildren {
        /// Source authoring node.
        node: NodeId,
        /// Runtime node kind.
        kind: BehaviorTreeNodeKind,
        /// Number of children found in the compiled tree.
        count: usize,
    },
    /// An action, condition, or generic decorator is missing its behavior ID.
    MissingBehaviorId {
        /// Source authoring node.
        node: NodeId,
        /// Runtime node kind.
        kind: BehaviorTreeNodeKind,
    },
    /// A composite node has no children.
    EmptyComposite {
        /// Source authoring node.
        node: NodeId,
        /// Runtime node kind.
        kind: BehaviorTreeNodeKind,
    },
    /// The compiled tree exceeds the runtime depth limit.
    MaxDepthExceeded {
        /// Source authoring node where the limit was exceeded.
        node: NodeId,
        /// Maximum allowed depth.
        max_depth: u32,
    },
    /// A stateful decorator's compiled parameter is invalid.
    InvalidDecoratorParameter {
        /// Source authoring node.
        node: NodeId,
        /// Compiled decorator behavior identifier.
        behavior_id: String,
    },
    /// One tick exhausted the configured hard node-step budget.
    StepBudgetExceeded {
        /// Maximum node visits allowed for one tick.
        max_steps: usize,
    },
    /// A runtime context failed to dispatch behavior or lifecycle work.
    BehaviorDispatchFailed {
        /// Runtime dispatch kind.
        dispatch: BehaviorDispatchKind,
        /// Stable behavior identifier.
        behavior_id: String,
        /// Context-owned failure cause.
        source: E,
    },
}

impl<E: fmt::Display> fmt::Display for BehaviorTreeRuntimeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRootKind { kind, .. } => write!(
                formatter,
                "behavior tree compiled root must be Root, found {kind:?}"
            ),
            Self::NestedRoot { .. } => {
                formatter.write_str("behavior tree root node may only appear at tree root")
            }
            Self::InvalidRootChildCount { count, .. } => write!(
                formatter,
                "behavior tree root must have exactly one child, found {count}"
            ),
            Self::InvalidDecoratorChildCount { count, .. } => write!(
                formatter,
                "behavior tree decorator must have exactly one child, found {count}"
            ),
            Self::LeafHasChildren { kind, count, .. } => write!(
                formatter,
                "behavior tree {kind:?} leaf must not have children, found {count}"
            ),
            Self::MissingBehaviorId { kind, .. } => {
                write!(formatter, "behavior tree {kind:?} node is missing behavior id")
            }
            Self::EmptyComposite { kind, .. } => write!(
                formatter,
                "behavior tree {kind:?} composite must have at least one child"
            ),
            Self::MaxDepthExceeded { max_depth, .. } => write!(
                formatter,
                "behavior tree compiled depth exceeds runtime limit {max_depth}"
            ),
            Self::InvalidDecoratorParameter { behavior_id, .. } => write!(
                formatter,
                "behavior tree decorator `{behavior_id}` has an invalid numeric parameter"
            ),
            Self::StepBudgetExceeded { max_steps } => write!(
                formatter,
                "behavior tree tick exceeded node-step budget {max_steps}"
            ),
            Self::BehaviorDispatchFailed {
                dispatch,
                behavior_id,
                source,
            } => write!(
                formatter,
                "behavior tree {dispatch:?} behavior `{behavior_id}` failed: {source}"
            ),
        }
    }
}

impl<E: Error + 'static> Error for BehaviorTreeRuntimeError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BehaviorDispatchFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct PreparedBehaviorNode {
    source: NodeId,
    kind: BehaviorTreeNodeKind,
    behavior: Option<String>,
    children: Vec<usize>,
    parent: Option<usize>,
}

#[derive(Debug, Clone)]
struct PreparedBehaviorTree {
    source: GraphId,
    nodes: Vec<PreparedBehaviorNode>,
    max_depth_node: Option<NodeId>,
}

impl PreparedBehaviorTree {
    fn new(tree: &CompiledBehaviorTree) -> Self {
        let mut plan = Self {
            source: tree.source.clone(),
            nodes: Vec::new(),
            max_depth_node: None,
        };
        prepare_node(&tree.root, None, 0, &mut plan);
        plan
    }
}

fn prepare_node(
    node: &CompiledBehaviorNode,
    parent: Option<usize>,
    depth: u32,
    plan: &mut PreparedBehaviorTree,
) -> usize {
    let index = plan.nodes.len();
    plan.nodes.push(PreparedBehaviorNode {
        source: node.source.clone(),
        kind: node.kind,
        behavior: node.behavior.clone(),
        children: Vec::new(),
        parent,
    });
    if depth > MAX_BEHAVIOR_TREE_RUNTIME_DEPTH {
        plan.max_depth_node.get_or_insert_with(|| node.source.clone());
        return index;
    }
    let children = node
        .children
        .iter()
        .map(|child| prepare_node(child, Some(index), depth.saturating_add(1), plan))
        .collect();
    plan.nodes[index].children = children;
    index
}

#[derive(Debug, Clone, Default)]
struct NodeExecutionState {
    cursor: usize,
    action_entered: bool,
    elapsed_seconds: f64,
    cooldown_until_seconds: f64,
}

/// Mutable execution state for one prepared Behavior Tree runner.
#[derive(Debug, Clone)]
pub struct BehaviorTreeInstance {
    node_state: Vec<NodeExecutionState>,
    active_path: Vec<usize>,
    active_actions: Vec<usize>,
    recent_transitions: VecDeque<BehaviorExecutionTransition>,
    execution_generation: u64,
    total_seconds: f64,
    last_terminal_node: Option<usize>,
    last_terminal_status: Option<BehaviorStatus>,
    last_reset_reason: Option<BehaviorResetReason>,
}

impl BehaviorTreeInstance {
    fn for_plan(plan: &PreparedBehaviorTree) -> Self {
        Self {
            node_state: vec![NodeExecutionState::default(); plan.nodes.len()],
            active_path: Vec::new(),
            active_actions: Vec::new(),
            recent_transitions: VecDeque::new(),
            execution_generation: 1,
            total_seconds: 0.0,
            last_terminal_node: None,
            last_terminal_status: None,
            last_reset_reason: None,
        }
    }

    /// Returns the current monotonic execution generation.
    #[must_use]
    pub fn execution_generation(&self) -> u64 {
        self.execution_generation
    }

    fn ensure_plan(&mut self, plan: &PreparedBehaviorTree) {
        if self.node_state.len() != plan.nodes.len() {
            *self = Self::for_plan(plan);
        }
    }

    fn push_transition(&mut self, transition: BehaviorExecutionTransition) {
        if self.recent_transitions.len() == MAX_BEHAVIOR_TREE_DEBUG_TRANSITIONS {
            self.recent_transitions.pop_front();
        }
        self.recent_transitions.push_back(transition);
    }

    fn record_node_terminal(
        &mut self,
        plan: &PreparedBehaviorTree,
        index: usize,
        status: BehaviorStatus,
    ) {
        if status == BehaviorStatus::Running {
            return;
        }
        self.last_terminal_node = Some(index);
        self.last_terminal_status = Some(status);
        let kind = match status {
            BehaviorStatus::Success => BehaviorExecutionTransitionKind::Success,
            BehaviorStatus::Failure => BehaviorExecutionTransitionKind::Failure,
            BehaviorStatus::Running => return,
        };
        self.push_transition(BehaviorExecutionTransition {
            generation: self.execution_generation,
            node: Some(plan.nodes[index].source.clone()),
            behavior_id: plan.nodes[index].behavior.clone(),
            kind,
            reason: None,
        });
    }
}

/// Runtime executor for one immutable compiled Behavior Tree.
pub struct BehaviorTreeExecutor {
    tree: CompiledBehaviorTree,
    plan: PreparedBehaviorTree,
    step_budget: usize,
}

impl BehaviorTreeExecutor {
    /// Creates an executor and prepares compact runtime indices once.
    #[must_use]
    pub fn new(tree: CompiledBehaviorTree) -> Self {
        let plan = PreparedBehaviorTree::new(&tree);
        Self {
            tree,
            plan,
            step_budget: DEFAULT_BEHAVIOR_TREE_STEP_BUDGET,
        }
    }

    /// Returns the compiled tree owned by this executor.
    #[must_use]
    pub fn tree(&self) -> &CompiledBehaviorTree {
        &self.tree
    }

    /// Returns the hard node-step budget used by each tick.
    #[must_use]
    pub fn step_budget(&self) -> usize {
        self.step_budget
    }

    /// Replaces the hard per-tick node-step budget.
    pub fn set_step_budget(&mut self, step_budget: usize) {
        self.step_budget = step_budget.max(1);
    }

    /// Creates fresh mutable state for this prepared plan.
    #[must_use]
    pub fn create_instance(&self) -> BehaviorTreeInstance {
        BehaviorTreeInstance::for_plan(&self.plan)
    }

    /// Ticks with a fresh temporary instance for source compatibility.
    ///
    /// Stateful runtime users should keep a [`BehaviorTreeInstance`] and call
    /// [`tick_instance`](Self::tick_instance). [`BehaviorTreeRunner`] does this
    /// automatically.
    pub fn tick<C: BehaviorTreeContext>(
        &self,
        context: &mut C,
    ) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
        let mut instance = self.create_instance();
        self.tick_instance(&mut instance, context)
    }

    /// Ticks one persistent instance of this prepared tree.
    pub fn tick_instance<C: BehaviorTreeContext>(
        &self,
        instance: &mut BehaviorTreeInstance,
        context: &mut C,
    ) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
        instance.ensure_plan(&self.plan);
        if let Some(node) = &self.plan.max_depth_node {
            return Err(BehaviorTreeRuntimeError::MaxDepthExceeded {
                node: node.clone(),
                max_depth: MAX_BEHAVIOR_TREE_RUNTIME_DEPTH,
            });
        }
        let root = &self.plan.nodes[0];
        if root.kind != BehaviorTreeNodeKind::Root {
            return Err(BehaviorTreeRuntimeError::InvalidRootKind {
                node: root.source.clone(),
                kind: root.kind,
            });
        }
        if root.children.len() != 1 {
            return Err(BehaviorTreeRuntimeError::InvalidRootChildCount {
                node: root.source.clone(),
                count: root.children.len(),
            });
        }

        let delta_seconds = context.behavior_delta_seconds();
        let delta_seconds = if delta_seconds.is_finite() && delta_seconds >= 0.0 {
            delta_seconds
        } else {
            0.0
        };
        instance.total_seconds += delta_seconds;
        instance.active_path.clear();
        let mut path = vec![0_usize];
        let mut steps = 0_usize;
        let status = tick_node(
            &self.plan,
            instance,
            context,
            root.children[0],
            1,
            delta_seconds,
            &mut steps,
            self.step_budget,
            &mut path,
        )?;
        if status == BehaviorStatus::Running {
            instance.active_path = path;
        } else {
            instance.active_path.clear();
            instance.record_node_terminal(&self.plan, 0, status);
        }
        Ok(status)
    }

    fn reset_instance<C: BehaviorTreeContext>(
        &self,
        instance: &mut BehaviorTreeInstance,
        context: &mut C,
        reason: BehaviorResetReason,
    ) -> Result<(), BehaviorTreeRuntimeError<C::Error>> {
        abort_actions_in_subtree(&self.plan, instance, context, 0, reason)?;
        clear_subtree_state(&self.plan, instance, 0, false);
        instance.active_path.clear();
        instance.execution_generation = instance.execution_generation.saturating_add(1);
        instance.last_reset_reason = Some(reason);
        instance.push_transition(BehaviorExecutionTransition {
            generation: instance.execution_generation,
            node: None,
            behavior_id: None,
            kind: BehaviorExecutionTransitionKind::Reset,
            reason: Some(reason),
        });
        Ok(())
    }
}

/// ECS component that ticks one compiled Behavior Tree for an entity.
pub struct BehaviorTreeRunner {
    executor: BehaviorTreeExecutor,
    instance: BehaviorTreeInstance,
    blackboard: BTreeMap<String, Value>,
    enabled: bool,
    tree_generation: u64,
    pending_reset: Option<BehaviorResetReason>,
    last_status: Option<BehaviorStatus>,
    last_error: Option<String>,
    last_dispatches: Vec<BehaviorTreeDispatchRecord>,
}

impl BehaviorTreeRunner {
    /// Creates a runner from a compiled Behavior Tree artifact.
    #[must_use]
    pub fn new(tree: CompiledBehaviorTree) -> Self {
        Self::with_blackboard(tree, BTreeMap::new())
    }

    /// Creates a runner with author-defined blackboard defaults.
    #[must_use]
    pub fn with_blackboard(
        tree: CompiledBehaviorTree,
        blackboard: BTreeMap<String, Value>,
    ) -> Self {
        let executor = BehaviorTreeExecutor::new(tree);
        let instance = executor.create_instance();
        Self {
            executor,
            instance,
            blackboard,
            enabled: true,
            tree_generation: 1,
            pending_reset: None,
            last_status: None,
            last_error: None,
            last_dispatches: Vec::new(),
        }
    }

    /// Returns the executor owned by this runner.
    #[must_use]
    pub fn executor(&self) -> &BehaviorTreeExecutor {
        &self.executor
    }

    /// Returns the runtime blackboard initialized from authoring defaults.
    #[must_use]
    pub fn blackboard(&self) -> &BTreeMap<String, Value> {
        &self.blackboard
    }

    /// Returns mutable runtime blackboard state for gameplay integrations.
    pub fn blackboard_mut(&mut self) -> &mut BTreeMap<String, Value> {
        &mut self.blackboard
    }

    /// Enables or pauses this runner without discarding authored blackboard data.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled && !enabled {
            self.pending_reset = Some(BehaviorResetReason::RunnerDisabled);
        }
        self.enabled = enabled;
    }

    /// Returns whether the shared tick system should execute this runner.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the most recent successful tick status.
    #[must_use]
    pub fn last_status(&self) -> Option<BehaviorStatus> {
        self.last_status
    }

    /// Returns the most recent tick error message, if ticking failed.
    #[must_use]
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Returns action/condition leaf nodes visited during the latest system tick.
    #[must_use]
    pub fn last_dispatches(&self) -> &[BehaviorTreeDispatchRecord] {
        &self.last_dispatches
    }

    /// Returns a bounded read-only runtime snapshot for debugging and tools.
    #[must_use]
    pub fn snapshot(&self) -> BehaviorExecutionSnapshot {
        BehaviorExecutionSnapshot {
            tree_source: self.executor.plan.source.clone(),
            tree_generation: self.tree_generation,
            execution_generation: self.instance.execution_generation,
            status: self.last_status,
            active_path: self
                .instance
                .active_path
                .iter()
                .filter_map(|index| {
                    self.executor.plan.nodes.get(*index).map(|node| {
                        BehaviorActiveNodeSnapshot {
                            node: node.source.clone(),
                            elapsed_seconds: self.instance.node_state[*index].elapsed_seconds,
                        }
                    })
                })
                .collect(),
            running_node: self
                .instance
                .active_path
                .last()
                .and_then(|index| self.executor.plan.nodes.get(*index))
                .map(|node| node.source.clone()),
            last_terminal_node: self
                .instance
                .last_terminal_node
                .and_then(|index| self.executor.plan.nodes.get(index))
                .map(|node| node.source.clone()),
            last_terminal_status: self.instance.last_terminal_status,
            last_reset_reason: self.instance.last_reset_reason,
            recent_transitions: self.instance.recent_transitions.iter().cloned().collect(),
            blackboard: self.blackboard.clone(),
            error: self.last_error.clone(),
        }
    }

    /// Ticks this runner with a caller-owned Behavior Tree context.
    pub fn tick<C: BehaviorTreeContext>(
        &mut self,
        context: &mut C,
    ) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
        self.flush_pending_reset(context)?;
        let status = self.executor.tick_instance(&mut self.instance, context)?;
        self.last_status = Some(status);
        self.last_error = None;
        Ok(status)
    }

    /// Aborts active work and resets execution state explicitly.
    pub fn reset<C: BehaviorTreeContext>(
        &mut self,
        context: &mut C,
        reason: BehaviorResetReason,
    ) -> Result<(), BehaviorTreeRuntimeError<C::Error>> {
        self.pending_reset = None;
        self.executor
            .reset_instance(&mut self.instance, context, reason)?;
        self.last_status = None;
        self.last_error = None;
        Ok(())
    }

    /// Replaces the immutable compiled tree after aborting the current generation.
    pub fn replace_tree<C: BehaviorTreeContext>(
        &mut self,
        tree: CompiledBehaviorTree,
        context: &mut C,
    ) -> Result<(), BehaviorTreeRuntimeError<C::Error>> {
        self.reset(context, BehaviorResetReason::TreeReplaced)?;
        let previous_generation = self.instance.execution_generation;
        self.executor = BehaviorTreeExecutor::new(tree);
        self.instance = self.executor.create_instance();
        self.instance.execution_generation = previous_generation;
        self.tree_generation = self.tree_generation.saturating_add(1);
        self.last_status = None;
        self.last_error = None;
        Ok(())
    }

    fn flush_pending_reset<C: BehaviorTreeContext>(
        &mut self,
        context: &mut C,
    ) -> Result<(), BehaviorTreeRuntimeError<C::Error>> {
        if let Some(reason) = self.pending_reset.take() {
            self.executor
                .reset_instance(&mut self.instance, context, reason)?;
            self.last_status = None;
        }
        Ok(())
    }

    fn record_error(&mut self, error: impl fmt::Display) {
        self.last_status = None;
        self.last_error = Some(error.to_string());
    }
}

/// One behavior dispatch made by the ECS Behavior Tree system.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorTreeDispatchRecord {
    /// Whether the dispatch targeted an action or condition behavior.
    pub kind: BehaviorDispatchKind,
    /// Stable behavior identifier passed to the runtime context.
    pub behavior_id: String,
}

/// Lifecycle event captured by the registry-backed test/runtime context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BehaviorTreeLifecycleEvent {
    /// An action was entered.
    Enter {
        /// Source authoring node.
        node: NodeId,
        /// Stable behavior identifier.
        behavior_id: String,
    },
    /// An action completed normally.
    Complete {
        /// Source authoring node.
        node: NodeId,
        /// Stable behavior identifier.
        behavior_id: String,
        /// Terminal status returned by the action.
        status: BehaviorStatus,
    },
    /// An action was aborted.
    Abort {
        /// Source authoring node.
        node: NodeId,
        /// Stable behavior identifier.
        behavior_id: String,
        /// Cancellation cause.
        reason: BehaviorResetReason,
    },
}

/// Registry-backed Behavior Tree context used by the minimal ECS integration.
#[derive(Debug)]
pub struct BehaviorTreeBehaviorRegistry {
    conditions: HashMap<String, BehaviorStatus>,
    actions: HashMap<String, BehaviorStatus>,
    calls: Vec<BehaviorTreeDispatchRecord>,
    lifecycle_calls: Vec<BehaviorTreeLifecycleEvent>,
    delta_seconds: f64,
}

impl Default for BehaviorTreeBehaviorRegistry {
    fn default() -> Self {
        Self {
            conditions: HashMap::new(),
            actions: HashMap::new(),
            calls: Vec::new(),
            lifecycle_calls: Vec::new(),
            delta_seconds: 1.0 / 60.0,
        }
    }
}

impl BehaviorTreeBehaviorRegistry {
    /// Registers or replaces a condition behavior result.
    pub fn set_condition(
        &mut self,
        behavior_id: impl Into<String>,
        status: BehaviorStatus,
    ) -> &mut Self {
        self.conditions.insert(behavior_id.into(), status);
        self
    }

    /// Registers or replaces an action behavior result.
    pub fn set_action(
        &mut self,
        behavior_id: impl Into<String>,
        status: BehaviorStatus,
    ) -> &mut Self {
        self.actions.insert(behavior_id.into(), status);
        self
    }

    /// Sets the deterministic delta consumed by stateful decorators.
    pub fn set_delta_seconds(&mut self, delta_seconds: f64) -> &mut Self {
        self.delta_seconds = delta_seconds.max(0.0);
        self
    }

    /// Returns dispatches observed since the last call to [`clear_calls`](Self::clear_calls).
    #[must_use]
    pub fn calls(&self) -> &[BehaviorTreeDispatchRecord] {
        &self.calls
    }

    /// Returns lifecycle calls observed during the current system update.
    #[must_use]
    pub fn lifecycle_calls(&self) -> &[BehaviorTreeLifecycleEvent] {
        &self.lifecycle_calls
    }

    /// Clears current-tick dispatch and lifecycle records.
    pub fn clear_calls(&mut self) {
        self.calls.clear();
        self.lifecycle_calls.clear();
    }
}

impl BehaviorTreeContext for BehaviorTreeBehaviorRegistry {
    type Error = BehaviorTreeRegistryError;

    fn action_enter(&mut self, node: &NodeId, behavior_id: &str) -> Result<(), Self::Error> {
        self.lifecycle_calls.push(BehaviorTreeLifecycleEvent::Enter {
            node: node.clone(),
            behavior_id: behavior_id.to_owned(),
        });
        Ok(())
    }

    fn tick_action(&mut self, behavior_id: &str) -> Result<BehaviorStatus, Self::Error> {
        self.calls.push(BehaviorTreeDispatchRecord {
            kind: BehaviorDispatchKind::Action,
            behavior_id: behavior_id.to_owned(),
        });
        self.actions.get(behavior_id).copied().ok_or_else(|| {
            BehaviorTreeRegistryError::MissingAction {
                behavior_id: behavior_id.to_owned(),
            }
        })
    }

    fn action_complete(
        &mut self,
        node: &NodeId,
        behavior_id: &str,
        status: BehaviorStatus,
    ) -> Result<(), Self::Error> {
        self.lifecycle_calls
            .push(BehaviorTreeLifecycleEvent::Complete {
                node: node.clone(),
                behavior_id: behavior_id.to_owned(),
                status,
            });
        Ok(())
    }

    fn action_abort(
        &mut self,
        node: &NodeId,
        behavior_id: &str,
        reason: BehaviorResetReason,
    ) -> Result<(), Self::Error> {
        self.lifecycle_calls.push(BehaviorTreeLifecycleEvent::Abort {
            node: node.clone(),
            behavior_id: behavior_id.to_owned(),
            reason,
        });
        Ok(())
    }

    fn check_condition(&mut self, behavior_id: &str) -> Result<BehaviorStatus, Self::Error> {
        self.calls.push(BehaviorTreeDispatchRecord {
            kind: BehaviorDispatchKind::Condition,
            behavior_id: behavior_id.to_owned(),
        });
        self.conditions.get(behavior_id).copied().ok_or_else(|| {
            BehaviorTreeRegistryError::MissingCondition {
                behavior_id: behavior_id.to_owned(),
            }
        })
    }

    fn behavior_delta_seconds(&self) -> f64 {
        self.delta_seconds
    }
}

/// Registry dispatch errors produced by [`BehaviorTreeBehaviorRegistry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BehaviorTreeRegistryError {
    /// No action behavior is registered for the behavior ID.
    MissingAction {
        /// Stable behavior identifier.
        behavior_id: String,
    },
    /// No condition behavior is registered for the behavior ID.
    MissingCondition {
        /// Stable behavior identifier.
        behavior_id: String,
    },
}

impl fmt::Display for BehaviorTreeRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAction { behavior_id } => {
                write!(formatter, "missing action behavior `{behavior_id}`")
            }
            Self::MissingCondition { behavior_id } => {
                write!(formatter, "missing condition behavior `{behavior_id}`")
            }
        }
    }
}

impl Error for BehaviorTreeRegistryError {}

/// ECS system that ticks every [`BehaviorTreeRunner`] once.
pub fn behavior_tree_tick_system(
    mut runners: Query<&mut BehaviorTreeRunner>,
    mut registry: ResMut<BehaviorTreeBehaviorRegistry>,
) {
    registry.clear_calls();
    for (_, runner) in &mut runners {
        let first_call = registry.calls.len();
        if !runner.is_enabled() {
            if let Err(error) = runner.flush_pending_reset(&mut *registry) {
                runner.record_error(error);
            }
            runner.last_dispatches = registry.calls[first_call..].to_vec();
            continue;
        }
        if let Err(error) = runner.tick(&mut *registry) {
            runner.record_error(error);
        }
        runner.last_dispatches = registry.calls[first_call..].to_vec();
    }
}

/// Adds the Behavior Tree ECS resource and tick system to an ECS app.
pub fn register_behavior_tree_system(
    app: &mut engine_ecs::App,
) -> Result<&mut engine_ecs::App, engine_ecs::SystemBuildError> {
    if app
        .world()
        .get_resource::<BehaviorTreeBehaviorRegistry>()
        .is_none()
    {
        app.insert_resource(BehaviorTreeBehaviorRegistry::default());
    }
    app.try_add_system(behavior_tree_tick_system)
}

// Recursive evaluation keeps bounded tick state explicit instead of boxing it.
#[allow(clippy::too_many_arguments)]
fn tick_node<C: BehaviorTreeContext>(
    plan: &PreparedBehaviorTree,
    instance: &mut BehaviorTreeInstance,
    context: &mut C,
    index: usize,
    depth: u32,
    delta_seconds: f64,
    steps: &mut usize,
    max_steps: usize,
    path: &mut Vec<usize>,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    if depth > MAX_BEHAVIOR_TREE_RUNTIME_DEPTH {
        return Err(BehaviorTreeRuntimeError::MaxDepthExceeded {
            node: plan.nodes[index].source.clone(),
            max_depth: MAX_BEHAVIOR_TREE_RUNTIME_DEPTH,
        });
    }
    *steps = steps.saturating_add(1);
    if *steps > max_steps {
        return Err(BehaviorTreeRuntimeError::StepBudgetExceeded { max_steps });
    }
    path.push(index);
    let status = match plan.nodes[index].kind {
        BehaviorTreeNodeKind::Root => Err(BehaviorTreeRuntimeError::NestedRoot {
            node: plan.nodes[index].source.clone(),
        }),
        BehaviorTreeNodeKind::Sequence => tick_sequence(
            plan,
            instance,
            context,
            index,
            depth,
            delta_seconds,
            steps,
            max_steps,
            path,
        ),
        BehaviorTreeNodeKind::Selector => tick_selector(
            plan,
            instance,
            context,
            index,
            depth,
            delta_seconds,
            steps,
            max_steps,
            path,
        ),
        BehaviorTreeNodeKind::Condition => tick_condition(plan, instance, context, index),
        BehaviorTreeNodeKind::Action => tick_action(plan, instance, context, index),
        BehaviorTreeNodeKind::Decorator => tick_decorator(
            plan,
            instance,
            context,
            index,
            depth,
            delta_seconds,
            steps,
            max_steps,
            path,
        ),
    }?;
    if status == BehaviorStatus::Running {
        instance.node_state[index].elapsed_seconds += delta_seconds;
    } else {
        if plan.nodes[index].kind != BehaviorTreeNodeKind::Decorator
            || !matches!(
                parse_decorator::<C::Error>(plan.nodes[index].behavior.as_deref()),
                Ok(PreparedDecorator::Cooldown(_))
            )
        {
            instance.node_state[index].elapsed_seconds = 0.0;
        }
        instance.record_node_terminal(plan, index, status);
        path.pop();
    }
    Ok(status)
}

// Recursive evaluation keeps bounded tick state explicit instead of boxing it.
#[allow(clippy::too_many_arguments)]
fn tick_sequence<C: BehaviorTreeContext>(
    plan: &PreparedBehaviorTree,
    instance: &mut BehaviorTreeInstance,
    context: &mut C,
    index: usize,
    depth: u32,
    delta_seconds: f64,
    steps: &mut usize,
    max_steps: usize,
    path: &mut Vec<usize>,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    let children = &plan.nodes[index].children;
    if children.is_empty() {
        return Err(BehaviorTreeRuntimeError::EmptyComposite {
            node: plan.nodes[index].source.clone(),
            kind: BehaviorTreeNodeKind::Sequence,
        });
    }
    let mut cursor = instance.node_state[index].cursor.min(children.len() - 1);
    while cursor < children.len() {
        match tick_node(
            plan,
            instance,
            context,
            children[cursor],
            depth.saturating_add(1),
            delta_seconds,
            steps,
            max_steps,
            path,
        )? {
            BehaviorStatus::Success => {
                cursor += 1;
                instance.node_state[index].cursor = cursor;
            }
            BehaviorStatus::Failure => {
                instance.node_state[index].cursor = 0;
                return Ok(BehaviorStatus::Failure);
            }
            BehaviorStatus::Running => {
                instance.node_state[index].cursor = cursor;
                return Ok(BehaviorStatus::Running);
            }
        }
    }
    instance.node_state[index].cursor = 0;
    Ok(BehaviorStatus::Success)
}

// Recursive evaluation keeps bounded tick state explicit instead of boxing it.
#[allow(clippy::too_many_arguments)]
fn tick_selector<C: BehaviorTreeContext>(
    plan: &PreparedBehaviorTree,
    instance: &mut BehaviorTreeInstance,
    context: &mut C,
    index: usize,
    depth: u32,
    delta_seconds: f64,
    steps: &mut usize,
    max_steps: usize,
    path: &mut Vec<usize>,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    let children = &plan.nodes[index].children;
    if children.is_empty() {
        return Err(BehaviorTreeRuntimeError::EmptyComposite {
            node: plan.nodes[index].source.clone(),
            kind: BehaviorTreeNodeKind::Selector,
        });
    }
    let mut cursor = instance.node_state[index].cursor.min(children.len() - 1);
    while cursor < children.len() {
        match tick_node(
            plan,
            instance,
            context,
            children[cursor],
            depth.saturating_add(1),
            delta_seconds,
            steps,
            max_steps,
            path,
        )? {
            BehaviorStatus::Success => {
                instance.node_state[index].cursor = 0;
                return Ok(BehaviorStatus::Success);
            }
            BehaviorStatus::Failure => {
                cursor += 1;
                instance.node_state[index].cursor = cursor;
            }
            BehaviorStatus::Running => {
                instance.node_state[index].cursor = cursor;
                return Ok(BehaviorStatus::Running);
            }
        }
    }
    instance.node_state[index].cursor = 0;
    Ok(BehaviorStatus::Failure)
}

fn tick_condition<C: BehaviorTreeContext>(
    plan: &PreparedBehaviorTree,
    _instance: &mut BehaviorTreeInstance,
    context: &mut C,
    index: usize,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    ensure_leaf(plan, index)?;
    let behavior_id = behavior_id(plan, index)?;
    context.check_condition(behavior_id).map_err(|source| {
        BehaviorTreeRuntimeError::BehaviorDispatchFailed {
            dispatch: BehaviorDispatchKind::Condition,
            behavior_id: behavior_id.to_owned(),
            source,
        }
    })
}

fn tick_action<C: BehaviorTreeContext>(
    plan: &PreparedBehaviorTree,
    instance: &mut BehaviorTreeInstance,
    context: &mut C,
    index: usize,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    ensure_leaf(plan, index)?;
    let behavior_id = behavior_id(plan, index)?;
    if !instance.node_state[index].action_entered {
        context
            .action_enter(&plan.nodes[index].source, behavior_id)
            .map_err(|source| BehaviorTreeRuntimeError::BehaviorDispatchFailed {
                dispatch: BehaviorDispatchKind::Action,
                behavior_id: behavior_id.to_owned(),
                source,
            })?;
        instance.node_state[index].action_entered = true;
        if !instance.active_actions.contains(&index) {
            instance.active_actions.push(index);
        }
        instance.push_transition(BehaviorExecutionTransition {
            generation: instance.execution_generation,
            node: Some(plan.nodes[index].source.clone()),
            behavior_id: Some(behavior_id.to_owned()),
            kind: BehaviorExecutionTransitionKind::Enter,
            reason: None,
        });
    }

    let status = match context.tick_action(behavior_id) {
        Ok(status) => status,
        Err(source) => {
            let _ = context.action_abort(
                &plan.nodes[index].source,
                behavior_id,
                BehaviorResetReason::Interrupted,
            );
            instance.node_state[index].action_entered = false;
            instance.active_actions.retain(|active| *active != index);
            instance.push_transition(BehaviorExecutionTransition {
                generation: instance.execution_generation,
                node: Some(plan.nodes[index].source.clone()),
                behavior_id: Some(behavior_id.to_owned()),
                kind: BehaviorExecutionTransitionKind::Abort,
                reason: Some(BehaviorResetReason::Interrupted),
            });
            return Err(BehaviorTreeRuntimeError::BehaviorDispatchFailed {
                dispatch: BehaviorDispatchKind::Action,
                behavior_id: behavior_id.to_owned(),
                source,
            });
        }
    };
    if status != BehaviorStatus::Running {
        instance.node_state[index].action_entered = false;
        instance.active_actions.retain(|active| *active != index);
        context
            .action_complete(&plan.nodes[index].source, behavior_id, status)
            .map_err(|source| BehaviorTreeRuntimeError::BehaviorDispatchFailed {
                dispatch: BehaviorDispatchKind::Action,
                behavior_id: behavior_id.to_owned(),
                source,
            })?;
    }
    Ok(status)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PreparedDecorator {
    Generic,
    Inverter,
    Wait(f64),
    Timeout(f64),
    Cooldown(f64),
}

fn parse_decorator<E>(
    behavior: Option<&str>,
) -> Result<PreparedDecorator, BehaviorTreeRuntimeError<E>> {
    let Some(behavior_id) = behavior.filter(|behavior| !behavior.is_empty()) else {
        return Ok(PreparedDecorator::Generic);
    };
    if behavior_id == INVERTER_BEHAVIOR {
        return Ok(PreparedDecorator::Inverter);
    }
    for (prefix, make) in [
        (WAIT_BEHAVIOR_PREFIX, PreparedDecorator::Wait as fn(f64) -> PreparedDecorator),
        (TIMEOUT_BEHAVIOR_PREFIX, PreparedDecorator::Timeout),
        (COOLDOWN_BEHAVIOR_PREFIX, PreparedDecorator::Cooldown),
    ] {
        if let Some(raw) = behavior_id.strip_prefix(prefix) {
            let duration = raw.parse::<f64>().ok().filter(|value| value.is_finite());
            if let Some(duration) = duration.filter(|value| *value >= 0.0) {
                return Ok(make(duration));
            }
        }
    }
    Ok(PreparedDecorator::Generic)
}

// Recursive evaluation keeps bounded tick state explicit instead of boxing it.
#[allow(clippy::too_many_arguments)]
fn tick_decorator<C: BehaviorTreeContext>(
    plan: &PreparedBehaviorTree,
    instance: &mut BehaviorTreeInstance,
    context: &mut C,
    index: usize,
    depth: u32,
    delta_seconds: f64,
    steps: &mut usize,
    max_steps: usize,
    path: &mut Vec<usize>,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    if plan.nodes[index].children.len() != 1 {
        return Err(BehaviorTreeRuntimeError::InvalidDecoratorChildCount {
            node: plan.nodes[index].source.clone(),
            count: plan.nodes[index].children.len(),
        });
    }
    let behavior_id = behavior_id(plan, index)?;
    let decorator = parse_decorator::<C::Error>(Some(behavior_id))?;
    let child = plan.nodes[index].children[0];
    match decorator {
        PreparedDecorator::Generic => tick_node(
            plan,
            instance,
            context,
            child,
            depth.saturating_add(1),
            delta_seconds,
            steps,
            max_steps,
            path,
        ),
        PreparedDecorator::Inverter => match tick_node(
            plan,
            instance,
            context,
            child,
            depth.saturating_add(1),
            delta_seconds,
            steps,
            max_steps,
            path,
        )? {
            BehaviorStatus::Success => Ok(BehaviorStatus::Failure),
            BehaviorStatus::Failure => Ok(BehaviorStatus::Success),
            BehaviorStatus::Running => Ok(BehaviorStatus::Running),
        },
        PreparedDecorator::Wait(duration) => {
            let elapsed = instance.node_state[index].elapsed_seconds + delta_seconds;
            if elapsed < duration {
                instance.node_state[index].elapsed_seconds = elapsed - delta_seconds;
                return Ok(BehaviorStatus::Running);
            }
            instance.node_state[index].elapsed_seconds = 0.0;
            tick_node(
                plan,
                instance,
                context,
                child,
                depth.saturating_add(1),
                delta_seconds,
                steps,
                max_steps,
                path,
            )
        }
        PreparedDecorator::Timeout(duration) => {
            let status = tick_node(
                plan,
                instance,
                context,
                child,
                depth.saturating_add(1),
                delta_seconds,
                steps,
                max_steps,
                path,
            )?;
            if status == BehaviorStatus::Running
                && instance.node_state[index].elapsed_seconds + delta_seconds >= duration
            {
                abort_actions_in_subtree(
                    plan,
                    instance,
                    context,
                    child,
                    BehaviorResetReason::Timeout,
                )?;
                clear_subtree_state(plan, instance, child, false);
                instance.node_state[index].elapsed_seconds = 0.0;
                instance.last_reset_reason = Some(BehaviorResetReason::Timeout);
                while path.last().is_some_and(|path_index| *path_index != index) {
                    path.pop();
                }
                return Ok(BehaviorStatus::Failure);
            }
            Ok(status)
        }
        PreparedDecorator::Cooldown(duration) => {
            if instance.node_state[index].cooldown_until_seconds > instance.total_seconds {
                return Ok(BehaviorStatus::Failure);
            }
            let status = tick_node(
                plan,
                instance,
                context,
                child,
                depth.saturating_add(1),
                delta_seconds,
                steps,
                max_steps,
                path,
            )?;
            if status == BehaviorStatus::Success {
                instance.node_state[index].cooldown_until_seconds =
                    instance.total_seconds + duration;
            }
            Ok(status)
        }
    }
}

fn ensure_leaf<E>(
    plan: &PreparedBehaviorTree,
    index: usize,
) -> Result<(), BehaviorTreeRuntimeError<E>> {
    if plan.nodes[index].children.is_empty() {
        Ok(())
    } else {
        Err(BehaviorTreeRuntimeError::LeafHasChildren {
            node: plan.nodes[index].source.clone(),
            kind: plan.nodes[index].kind,
            count: plan.nodes[index].children.len(),
        })
    }
}

fn behavior_id<E>(
    plan: &PreparedBehaviorTree,
    index: usize,
) -> Result<&str, BehaviorTreeRuntimeError<E>> {
    plan.nodes[index]
        .behavior
        .as_deref()
        .filter(|behavior| !behavior.is_empty())
        .ok_or_else(|| BehaviorTreeRuntimeError::MissingBehaviorId {
            node: plan.nodes[index].source.clone(),
            kind: plan.nodes[index].kind,
        })
}

fn is_descendant(plan: &PreparedBehaviorTree, mut node: usize, ancestor: usize) -> bool {
    loop {
        if node == ancestor {
            return true;
        }
        let Some(parent) = plan.nodes[node].parent else {
            return false;
        };
        node = parent;
    }
}

fn clear_subtree_state(
    plan: &PreparedBehaviorTree,
    instance: &mut BehaviorTreeInstance,
    root: usize,
    preserve_cooldown: bool,
) {
    for index in 0..plan.nodes.len() {
        if !is_descendant(plan, index, root) {
            continue;
        }
        let cooldown = instance.node_state[index].cooldown_until_seconds;
        instance.node_state[index] = NodeExecutionState::default();
        if preserve_cooldown {
            instance.node_state[index].cooldown_until_seconds = cooldown;
        }
    }
    instance
        .active_actions
        .retain(|index| !is_descendant(plan, *index, root));
}

fn abort_actions_in_subtree<C: BehaviorTreeContext>(
    plan: &PreparedBehaviorTree,
    instance: &mut BehaviorTreeInstance,
    context: &mut C,
    root: usize,
    reason: BehaviorResetReason,
) -> Result<(), BehaviorTreeRuntimeError<C::Error>> {
    let targets = instance
        .active_actions
        .iter()
        .copied()
        .filter(|index| is_descendant(plan, *index, root))
        .rev()
        .collect::<Vec<_>>();
    let mut first_error = None;
    for index in targets {
        let Some(behavior_id) = plan.nodes[index].behavior.as_deref() else {
            continue;
        };
        if let Err(source) = context.action_abort(&plan.nodes[index].source, behavior_id, reason)
            && first_error.is_none()
        {
            first_error = Some((behavior_id.to_owned(), source));
        }
        instance.node_state[index].action_entered = false;
        instance.push_transition(BehaviorExecutionTransition {
            generation: instance.execution_generation,
            node: Some(plan.nodes[index].source.clone()),
            behavior_id: Some(behavior_id.to_owned()),
            kind: BehaviorExecutionTransitionKind::Abort,
            reason: Some(reason),
        });
    }
    instance
        .active_actions
        .retain(|index| !is_descendant(plan, *index, root));
    if let Some((behavior_id, source)) = first_error {
        return Err(BehaviorTreeRuntimeError::BehaviorDispatchFailed {
            dispatch: BehaviorDispatchKind::Action,
            behavior_id,
            source,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{BehaviorTreeDomain, EdgeId, Graph, GraphCommand, GraphDomain, GraphTransaction};

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

    fn action(behavior: &str) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Action, Some(behavior), vec![])
    }

    fn condition(behavior: &str) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Condition, Some(behavior), vec![])
    }

    fn sequence(children: Vec<CompiledBehaviorNode>) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Sequence, None, children)
    }

    fn selector(children: Vec<CompiledBehaviorNode>) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Selector, None, children)
    }

    fn decorator(behavior: &str, child: CompiledBehaviorNode) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Decorator, Some(behavior), vec![child])
    }

    fn tree(child: CompiledBehaviorNode) -> CompiledBehaviorTree {
        CompiledBehaviorTree {
            source: GraphId::generate(),
            root: node(BehaviorTreeNodeKind::Root, None, vec![child]),
        }
    }

    #[test]
    fn memory_sequence_resumes_running_child_without_restarting_prefix() {
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry
            .set_condition("ready", BehaviorStatus::Success)
            .set_action("work", BehaviorStatus::Running);
        let mut runner = BehaviorTreeRunner::new(tree(sequence(vec![
            condition("ready"),
            action("work"),
        ])));

        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Running);
        registry.clear_calls();
        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Running);

        assert_eq!(
            registry.calls(),
            [BehaviorTreeDispatchRecord {
                kind: BehaviorDispatchKind::Action,
                behavior_id: "work".into(),
            }]
        );
    }

    #[test]
    fn memory_selector_resumes_running_child_without_restarting_failures() {
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry
            .set_condition("visible", BehaviorStatus::Failure)
            .set_action("search", BehaviorStatus::Running);
        let mut runner = BehaviorTreeRunner::new(tree(selector(vec![
            condition("visible"),
            action("search"),
        ])));

        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Running);
        registry.clear_calls();
        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Running);

        assert_eq!(registry.calls().len(), 1);
        assert_eq!(registry.calls()[0].behavior_id, "search");
    }

    #[test]
    fn action_lifecycle_is_balanced_across_running_and_completion() {
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry.set_action("work", BehaviorStatus::Running);
        let mut runner = BehaviorTreeRunner::new(tree(action("work")));

        runner.tick(&mut registry).unwrap();
        assert!(matches!(
            registry.lifecycle_calls(),
            [BehaviorTreeLifecycleEvent::Enter { .. }]
        ));
        registry.clear_calls();
        registry.set_action("work", BehaviorStatus::Success);
        runner.tick(&mut registry).unwrap();

        assert!(matches!(
            registry.lifecycle_calls(),
            [BehaviorTreeLifecycleEvent::Complete {
                status: BehaviorStatus::Success,
                ..
            }]
        ));
    }

    #[test]
    fn disabling_runner_aborts_running_action_once() {
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry.set_action("work", BehaviorStatus::Running);
        let mut runner = BehaviorTreeRunner::new(tree(action("work")));
        runner.tick(&mut registry).unwrap();
        registry.clear_calls();

        runner.set_enabled(false);
        runner.flush_pending_reset(&mut registry).unwrap();
        runner.flush_pending_reset(&mut registry).unwrap();

        assert_eq!(registry.lifecycle_calls().len(), 1);
        assert!(matches!(
            registry.lifecycle_calls()[0],
            BehaviorTreeLifecycleEvent::Abort {
                reason: BehaviorResetReason::RunnerDisabled,
                ..
            }
        ));
    }

    #[test]
    fn inverter_flips_terminal_status() {
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry.set_condition("ready", BehaviorStatus::Success);
        let mut runner = BehaviorTreeRunner::new(tree(decorator(
            INVERTER_BEHAVIOR,
            condition("ready"),
        )));

        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Failure);
    }

    #[test]
    fn wait_delays_child_until_duration_elapses() {
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry
            .set_delta_seconds(0.25)
            .set_action("work", BehaviorStatus::Success);
        let mut runner = BehaviorTreeRunner::new(tree(decorator(
            "engine.wait:0.5",
            action("work"),
        )));

        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Running);
        assert!(registry.calls().is_empty());
        registry.clear_calls();
        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Success);
        assert_eq!(registry.calls().len(), 1);
    }

    #[test]
    fn timeout_aborts_running_child_and_fails() {
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry
            .set_delta_seconds(0.25)
            .set_action("work", BehaviorStatus::Running);
        let mut runner = BehaviorTreeRunner::new(tree(decorator(
            "engine.timeout:0.5",
            action("work"),
        )));

        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Running);
        registry.clear_calls();
        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Failure);
        assert!(registry.lifecycle_calls().iter().any(|event| matches!(
            event,
            BehaviorTreeLifecycleEvent::Abort {
                reason: BehaviorResetReason::Timeout,
                ..
            }
        )));
    }

    #[test]
    fn cooldown_blocks_successful_child_until_window_expires() {
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry
            .set_delta_seconds(0.25)
            .set_action("fire", BehaviorStatus::Success);
        let mut runner = BehaviorTreeRunner::new(tree(decorator(
            "engine.cooldown:0.5",
            action("fire"),
        )));

        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Success);
        registry.clear_calls();
        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Failure);
        assert!(registry.calls().is_empty());
        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Success);
    }

    #[test]
    fn step_budget_stops_pathological_tick() {
        let compiled = tree(sequence(vec![condition("a"), condition("b")]));
        let mut executor = BehaviorTreeExecutor::new(compiled);
        executor.set_step_budget(1);
        let mut instance = executor.create_instance();
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry
            .set_condition("a", BehaviorStatus::Success)
            .set_condition("b", BehaviorStatus::Success);

        assert!(matches!(
            executor.tick_instance(&mut instance, &mut registry),
            Err(BehaviorTreeRuntimeError::StepBudgetExceeded { max_steps: 1 })
        ));
    }

    #[test]
    fn snapshot_maps_runtime_indices_back_to_source_ids() {
        let running = action("work");
        let running_id = running.source.clone();
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry.set_action("work", BehaviorStatus::Running);
        let mut runner = BehaviorTreeRunner::new(tree(running));

        runner.tick(&mut registry).unwrap();
        let snapshot = runner.snapshot();

        assert_eq!(snapshot.status, Some(BehaviorStatus::Running));
        assert_eq!(snapshot.running_node, Some(running_id));
        assert!(!snapshot.active_path.is_empty());
        assert!(snapshot.recent_transitions.len() <= MAX_BEHAVIOR_TREE_DEBUG_TRANSITIONS);
    }

    #[test]
    fn compiled_authoring_tree_executes_with_persistent_runner() {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "runtime_smoke",
        );
        let root = NodeId::generate();
        let action = NodeId::generate();
        let mut transaction = GraphTransaction::begin(&graph);
        transaction.apply(GraphCommand::AddNode {
            node: domain.root_node(root.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.action_node(action.clone(), "idle"),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), root, action, 0),
        });
        transaction
            .commit(&mut graph, domain.schema_registry())
            .expect("test graph must commit");
        let compiled = domain.compile(&graph).expect("test graph must compile");
        let mut registry = BehaviorTreeBehaviorRegistry::default();
        registry.set_action("idle", BehaviorStatus::Success);
        let mut runner = BehaviorTreeRunner::new(compiled);

        assert_eq!(runner.tick(&mut registry).unwrap(), BehaviorStatus::Success);
    }
}
