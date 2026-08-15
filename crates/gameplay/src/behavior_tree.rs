//! Runtime executor for compiled Behavior Tree artifacts.
//!
//! The executor runs [`engine_authoring::CompiledBehaviorTree`] values produced
//! by the authoring Behavior Tree domain. It does not inspect or execute
//! authoring [`engine_authoring::Graph`] documents directly. The initial
//! traversal is stateless: a `Running` child is reported to the caller, and
//! advanced resume policy is future work.

use engine_authoring::{
    BehaviorTreeNodeKind, CompiledBehaviorNode, CompiledBehaviorTree, NodeId, Value,
};
use engine_ecs::{Query, ResMut};
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;

/// Maximum compiled Behavior Tree depth accepted by the runtime executor.
///
/// The authoring compiler enforces the same limit for graphs it compiles, but
/// runtime also checks it because compiled trees are public DTOs and can be
/// constructed or deserialized directly.
pub const MAX_BEHAVIOR_TREE_RUNTIME_DEPTH: u32 = 1024;

/// Runtime result of ticking one Behavior Tree node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorStatus {
    /// The node completed successfully.
    Success,
    /// The node completed unsuccessfully.
    Failure,
    /// The node is still executing and should be ticked again later.
    Running,
}

/// Identifies which external behavior dispatch failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorDispatchKind {
    /// An action dispatch failed.
    Action,
    /// A condition dispatch failed.
    Condition,
}

/// External behavior dispatch surface used by [`BehaviorTreeExecutor`].
///
/// Implementors own the actual gameplay meaning of action and condition
/// behavior identifiers. The executor only dispatches stable behavior IDs from
/// the compiled tree.
pub trait BehaviorTreeContext {
    /// Error type produced by this context's behavior dispatch implementation.
    type Error: Error + Send + Sync + 'static;

    /// Ticks an action behavior.
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` when the behavior cannot be executed. The executor
    /// wraps it in [`BehaviorTreeRuntimeError::BehaviorDispatchFailed`].
    fn tick_action(&mut self, behavior_id: &str) -> Result<BehaviorStatus, Self::Error>;

    /// Checks a condition behavior.
    ///
    /// Synchronous conditions can return [`BehaviorStatus::Success`] or
    /// [`BehaviorStatus::Failure`]. Polled or asynchronous conditions may
    /// return [`BehaviorStatus::Running`].
    ///
    /// # Errors
    ///
    /// Returns `Self::Error` when the behavior cannot be evaluated. The
    /// executor wraps it in [`BehaviorTreeRuntimeError::BehaviorDispatchFailed`].
    fn check_condition(&mut self, behavior_id: &str) -> Result<BehaviorStatus, Self::Error>;
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
    /// An action or condition node is missing its behavior identifier.
    MissingBehaviorId {
        /// Source authoring node.
        node: NodeId,
        /// Runtime node kind.
        kind: BehaviorTreeNodeKind,
    },
    /// A composite node (Sequence or Selector) has no children.
    ///
    /// Authoring rejects this shape; the runtime guards it too because compiled
    /// trees are public DTOs and can be deserialized directly.
    EmptyComposite {
        /// Source authoring node.
        node: NodeId,
        /// Runtime node kind (Sequence or Selector).
        kind: BehaviorTreeNodeKind,
    },
    /// The compiled tree exceeds the runtime depth limit.
    MaxDepthExceeded {
        /// Source authoring node where the limit was exceeded.
        node: NodeId,
        /// Maximum allowed depth.
        max_depth: u32,
    },
    /// A runtime context failed to dispatch an action or condition behavior.
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
            Self::InvalidRootKind { kind, .. } => {
                write!(
                    formatter,
                    "behavior tree compiled root must be Root, found {kind:?}"
                )
            }
            Self::NestedRoot { .. } => {
                formatter.write_str("behavior tree root node may only appear at tree root")
            }
            Self::InvalidRootChildCount { count, .. } => {
                write!(
                    formatter,
                    "behavior tree root must have exactly one child, found {count}"
                )
            }
            Self::InvalidDecoratorChildCount { count, .. } => {
                write!(
                    formatter,
                    "behavior tree decorator must have exactly one child, found {count}"
                )
            }
            Self::LeafHasChildren { kind, count, .. } => {
                write!(
                    formatter,
                    "behavior tree {kind:?} leaf must not have children, found {count}"
                )
            }
            Self::MissingBehaviorId { kind, .. } => {
                write!(
                    formatter,
                    "behavior tree {kind:?} node is missing behavior id"
                )
            }
            Self::EmptyComposite { kind, .. } => {
                write!(
                    formatter,
                    "behavior tree {kind:?} composite must have at least one child"
                )
            }
            Self::MaxDepthExceeded { max_depth, .. } => {
                write!(
                    formatter,
                    "behavior tree compiled depth exceeds runtime limit {max_depth}"
                )
            }
            Self::BehaviorDispatchFailed {
                dispatch,
                behavior_id,
                source,
            } => {
                write!(
                    formatter,
                    "behavior tree {dispatch:?} behavior `{behavior_id}` failed: {source}"
                )
            }
        }
    }
}

impl<E: Error + 'static> Error for BehaviorTreeRuntimeError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::BehaviorDispatchFailed { source, .. } => Some(source),
            Self::InvalidRootKind { .. }
            | Self::NestedRoot { .. }
            | Self::InvalidRootChildCount { .. }
            | Self::InvalidDecoratorChildCount { .. }
            | Self::LeafHasChildren { .. }
            | Self::MissingBehaviorId { .. }
            | Self::EmptyComposite { .. }
            | Self::MaxDepthExceeded { .. } => None,
        }
    }
}

/// Runtime executor for one compiled Behavior Tree.
pub struct BehaviorTreeExecutor {
    tree: CompiledBehaviorTree,
}

impl BehaviorTreeExecutor {
    /// Creates an executor that owns `tree`.
    #[must_use]
    pub fn new(tree: CompiledBehaviorTree) -> Self {
        Self { tree }
    }

    /// Returns the compiled tree owned by this executor.
    #[must_use]
    pub fn tree(&self) -> &CompiledBehaviorTree {
        &self.tree
    }

    /// Ticks the compiled tree once from the root.
    ///
    /// Phase 6 traversal is intentionally stateless. If a child returns
    /// [`BehaviorStatus::Running`], the executor returns `Running` immediately;
    /// the next tick starts from the root again.
    ///
    /// # Errors
    ///
    /// Returns [`BehaviorTreeRuntimeError`] when the compiled tree shape is
    /// invalid or when `context` cannot dispatch a behavior identifier.
    pub fn tick<C: BehaviorTreeContext>(
        &self,
        context: &mut C,
    ) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
        if self.tree.root.kind != BehaviorTreeNodeKind::Root {
            return Err(BehaviorTreeRuntimeError::InvalidRootKind {
                node: self.tree.root.source.clone(),
                kind: self.tree.root.kind,
            });
        }
        tick_root(&self.tree.root, context)
    }
}

/// ECS component that ticks one compiled Behavior Tree for an entity.
pub struct BehaviorTreeRunner {
    executor: BehaviorTreeExecutor,
    blackboard: BTreeMap<String, Value>,
    enabled: bool,
    last_status: Option<BehaviorStatus>,
    last_error: Option<String>,
    last_dispatches: Vec<BehaviorTreeDispatchRecord>,
}

impl BehaviorTreeRunner {
    /// Creates a runner from a compiled Behavior Tree artifact.
    pub fn new(tree: CompiledBehaviorTree) -> Self {
        Self::with_blackboard(tree, BTreeMap::new())
    }

    /// Creates a runner with author-defined blackboard defaults.
    pub fn with_blackboard(
        tree: CompiledBehaviorTree,
        blackboard: BTreeMap<String, Value>,
    ) -> Self {
        Self {
            executor: BehaviorTreeExecutor::new(tree),
            blackboard,
            enabled: true,
            last_status: None,
            last_error: None,
            last_dispatches: Vec::new(),
        }
    }

    /// Returns the executor owned by this runner.
    pub fn executor(&self) -> &BehaviorTreeExecutor {
        &self.executor
    }

    /// Returns the runtime blackboard initialized from authoring defaults.
    pub fn blackboard(&self) -> &BTreeMap<String, Value> {
        &self.blackboard
    }

    /// Returns mutable runtime blackboard state for gameplay integrations.
    pub fn blackboard_mut(&mut self) -> &mut BTreeMap<String, Value> {
        &mut self.blackboard
    }

    /// Enables or pauses this runner without discarding its state.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Returns whether the shared tick system should execute this runner.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the most recent successful tick status.
    ///
    /// Returns `None` before the first tick and after a tick error. Use
    /// [`last_error`] to distinguish a failed tick from a runner that has not
    /// been ticked yet.
    ///
    /// [`last_error`]: Self::last_error
    pub fn last_status(&self) -> Option<BehaviorStatus> {
        self.last_status
    }

    /// Returns the most recent tick error message, if ticking failed.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Returns action/condition leaf nodes visited during the latest tick.
    pub fn last_dispatches(&self) -> &[BehaviorTreeDispatchRecord] {
        &self.last_dispatches
    }

    /// Ticks this runner with a caller-owned Behavior Tree context.
    ///
    /// # Errors
    ///
    /// Returns [`BehaviorTreeRuntimeError`] when the compiled tree shape is
    /// invalid or the context rejects a behavior dispatch.
    pub fn tick<C: BehaviorTreeContext>(
        &mut self,
        context: &mut C,
    ) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
        let status = self.executor.tick(context)?;
        self.last_status = Some(status);
        self.last_error = None;
        Ok(status)
    }

    fn record_error(&mut self, error: impl fmt::Display) {
        self.last_status = None;
        self.last_error = Some(error.to_string());
    }
}

/// One behavior dispatch made by the ECS Behavior Tree system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorTreeDispatchRecord {
    /// Whether the dispatch targeted an action or condition behavior.
    pub kind: BehaviorDispatchKind,
    /// Stable behavior identifier passed to the runtime context.
    pub behavior_id: String,
}

/// Registry-backed Behavior Tree context used by the minimal ECS integration.
///
/// This resource is intentionally small. It proves that ECS systems can tick
/// compiled Behavior Trees without embedding gameplay behavior in authoring or
/// CLI code. Real games can provide a custom [`BehaviorTreeContext`] outside
/// this registry.
#[derive(Debug, Default)]
pub struct BehaviorTreeBehaviorRegistry {
    conditions: HashMap<String, BehaviorStatus>,
    actions: HashMap<String, BehaviorStatus>,
    calls: Vec<BehaviorTreeDispatchRecord>,
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

    /// Returns all dispatches observed since the last call to [`clear_calls`].
    ///
    /// [`clear_calls`]: Self::clear_calls
    pub fn calls(&self) -> &[BehaviorTreeDispatchRecord] {
        &self.calls
    }

    /// Clears recorded dispatch calls without changing registered behavior statuses.
    pub fn clear_calls(&mut self) {
        self.calls.clear();
    }
}

impl BehaviorTreeContext for BehaviorTreeBehaviorRegistry {
    type Error = BehaviorTreeRegistryError;

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
///
/// The system writes each runner's latest status or error. Dispatch behavior
/// comes from [`BehaviorTreeBehaviorRegistry`], which must be inserted as a
/// world resource before the system runs. Dispatch call records are cleared at
/// the start of each run so [`BehaviorTreeBehaviorRegistry::calls`] describes
/// the current system tick only.
pub fn behavior_tree_tick_system(
    mut runners: Query<&mut BehaviorTreeRunner>,
    mut registry: ResMut<BehaviorTreeBehaviorRegistry>,
) {
    registry.clear_calls();
    for (_, runner) in &mut runners {
        if !runner.is_enabled() {
            continue;
        }
        let first_call = registry.calls.len();
        if let Err(error) = runner.tick(&mut *registry) {
            runner.record_error(error);
        }
        runner.last_dispatches = registry.calls[first_call..].to_vec();
    }
}

/// Adds the minimal Behavior Tree ECS resource and tick system to an ECS app.
///
/// # Errors
///
/// Returns [`engine_ecs::SystemBuildError`] if the system access declaration is
/// rejected by the ECS scheduler.
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

fn tick_root<C: BehaviorTreeContext>(
    node: &CompiledBehaviorNode,
    context: &mut C,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    let child = single_child(node, |count| {
        BehaviorTreeRuntimeError::InvalidRootChildCount {
            node: node.source.clone(),
            count,
        }
    })?;
    tick_node(child, context, 1)
}

fn tick_node<C: BehaviorTreeContext>(
    node: &CompiledBehaviorNode,
    context: &mut C,
    depth: u32,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    if depth > MAX_BEHAVIOR_TREE_RUNTIME_DEPTH {
        return Err(BehaviorTreeRuntimeError::MaxDepthExceeded {
            node: node.source.clone(),
            max_depth: MAX_BEHAVIOR_TREE_RUNTIME_DEPTH,
        });
    }
    match node.kind {
        BehaviorTreeNodeKind::Root => Err(BehaviorTreeRuntimeError::NestedRoot {
            node: node.source.clone(),
        }),
        BehaviorTreeNodeKind::Sequence => tick_sequence(node, context, depth),
        BehaviorTreeNodeKind::Selector => tick_selector(node, context, depth),
        BehaviorTreeNodeKind::Condition => tick_condition(node, context),
        BehaviorTreeNodeKind::Action => tick_action(node, context),
        BehaviorTreeNodeKind::Decorator => tick_decorator(node, context, depth),
    }
}

fn tick_sequence<C: BehaviorTreeContext>(
    node: &CompiledBehaviorNode,
    context: &mut C,
    depth: u32,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    if node.children.is_empty() {
        return Err(BehaviorTreeRuntimeError::EmptyComposite {
            node: node.source.clone(),
            kind: node.kind,
        });
    }
    for child in &node.children {
        match tick_node(child, context, depth + 1)? {
            BehaviorStatus::Success => {}
            BehaviorStatus::Failure => return Ok(BehaviorStatus::Failure),
            BehaviorStatus::Running => return Ok(BehaviorStatus::Running),
        }
    }
    Ok(BehaviorStatus::Success)
}

fn tick_selector<C: BehaviorTreeContext>(
    node: &CompiledBehaviorNode,
    context: &mut C,
    depth: u32,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    if node.children.is_empty() {
        return Err(BehaviorTreeRuntimeError::EmptyComposite {
            node: node.source.clone(),
            kind: node.kind,
        });
    }
    for child in &node.children {
        match tick_node(child, context, depth + 1)? {
            BehaviorStatus::Success => return Ok(BehaviorStatus::Success),
            BehaviorStatus::Failure => {}
            BehaviorStatus::Running => return Ok(BehaviorStatus::Running),
        }
    }
    Ok(BehaviorStatus::Failure)
}

fn tick_condition<C: BehaviorTreeContext>(
    node: &CompiledBehaviorNode,
    context: &mut C,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    ensure_leaf(node)?;
    let behavior_id = behavior_id(node)?;
    context.check_condition(behavior_id).map_err(|source| {
        BehaviorTreeRuntimeError::BehaviorDispatchFailed {
            dispatch: BehaviorDispatchKind::Condition,
            behavior_id: behavior_id.to_owned(),
            source,
        }
    })
}

fn tick_action<C: BehaviorTreeContext>(
    node: &CompiledBehaviorNode,
    context: &mut C,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    ensure_leaf(node)?;
    let behavior_id = behavior_id(node)?;
    context.tick_action(behavior_id).map_err(|source| {
        BehaviorTreeRuntimeError::BehaviorDispatchFailed {
            dispatch: BehaviorDispatchKind::Action,
            behavior_id: behavior_id.to_owned(),
            source,
        }
    })
}

fn tick_decorator<C: BehaviorTreeContext>(
    node: &CompiledBehaviorNode,
    context: &mut C,
    depth: u32,
) -> Result<BehaviorStatus, BehaviorTreeRuntimeError<C::Error>> {
    behavior_id(node)?;
    let child = single_child(node, |count| {
        BehaviorTreeRuntimeError::InvalidDecoratorChildCount {
            node: node.source.clone(),
            count,
        }
    })?;
    tick_node(child, context, depth + 1)
}

fn single_child<E>(
    node: &CompiledBehaviorNode,
    make_error: impl FnOnce(usize) -> BehaviorTreeRuntimeError<E>,
) -> Result<&CompiledBehaviorNode, BehaviorTreeRuntimeError<E>> {
    node.children
        .first()
        .filter(|_| node.children.len() == 1)
        .ok_or_else(|| make_error(node.children.len()))
}

fn ensure_leaf<E>(node: &CompiledBehaviorNode) -> Result<(), BehaviorTreeRuntimeError<E>> {
    if node.children.is_empty() {
        Ok(())
    } else {
        Err(BehaviorTreeRuntimeError::LeafHasChildren {
            node: node.source.clone(),
            kind: node.kind,
            count: node.children.len(),
        })
    }
}

fn behavior_id<E>(node: &CompiledBehaviorNode) -> Result<&str, BehaviorTreeRuntimeError<E>> {
    node.behavior
        .as_deref()
        .filter(|behavior| !behavior.is_empty())
        .ok_or_else(|| BehaviorTreeRuntimeError::MissingBehaviorId {
            node: node.source.clone(),
            kind: node.kind,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_authoring::{
        BehaviorTreeDomain, EdgeId, Graph, GraphCommand, GraphDomain, GraphId, GraphTransaction,
        NodeId,
    };
    use std::collections::HashMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum FakeContextError {
        Unregistered(String),
        MissingTarget(String),
    }

    impl fmt::Display for FakeContextError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Unregistered(behavior_id) => {
                    write!(formatter, "unregistered behavior `{behavior_id}`")
                }
                Self::MissingTarget(behavior_id) => {
                    write!(formatter, "behavior `{behavior_id}` has no target")
                }
            }
        }
    }

    impl Error for FakeContextError {}

    #[derive(Default)]
    struct FakeContext {
        conditions: HashMap<String, BehaviorStatus>,
        actions: HashMap<String, BehaviorStatus>,
        action_errors: HashMap<String, FakeContextError>,
        calls: Vec<String>,
    }

    impl FakeContext {
        fn with_condition(mut self, behavior_id: &str, status: BehaviorStatus) -> Self {
            self.conditions.insert(behavior_id.into(), status);
            self
        }

        fn with_action(mut self, behavior_id: &str, status: BehaviorStatus) -> Self {
            self.actions.insert(behavior_id.into(), status);
            self
        }

        fn with_action_error(mut self, behavior_id: &str, error: FakeContextError) -> Self {
            self.action_errors.insert(behavior_id.into(), error);
            self
        }
    }

    impl BehaviorTreeContext for FakeContext {
        type Error = FakeContextError;

        fn tick_action(&mut self, behavior_id: &str) -> Result<BehaviorStatus, Self::Error> {
            self.calls.push(format!("action:{behavior_id}"));
            if let Some(error) = self.action_errors.get(behavior_id) {
                return Err(error.clone());
            }
            self.actions
                .get(behavior_id)
                .copied()
                .ok_or_else(|| FakeContextError::Unregistered(behavior_id.into()))
        }

        fn check_condition(&mut self, behavior_id: &str) -> Result<BehaviorStatus, Self::Error> {
            self.calls.push(format!("condition:{behavior_id}"));
            self.conditions
                .get(behavior_id)
                .copied()
                .ok_or_else(|| FakeContextError::Unregistered(behavior_id.into()))
        }
    }

    #[test]
    fn condition_true_returns_success() {
        let executor = executor(condition("ready"));
        let mut context = FakeContext::default().with_condition("ready", BehaviorStatus::Success);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Success
        );
        assert_eq!(context.calls, ["condition:ready"]);
    }

    #[test]
    fn condition_false_returns_failure() {
        let executor = executor(condition("ready"));
        let mut context = FakeContext::default().with_condition("ready", BehaviorStatus::Failure);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Failure
        );
    }

    #[test]
    fn condition_running_returns_running() {
        let executor = executor(condition("ready"));
        let mut context = FakeContext::default().with_condition("ready", BehaviorStatus::Running);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Running
        );
    }

    #[test]
    fn action_success_returns_success() {
        let executor = executor(action("idle"));
        let mut context = FakeContext::default().with_action("idle", BehaviorStatus::Success);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Success
        );
        assert_eq!(context.calls, ["action:idle"]);
    }

    #[test]
    fn action_failure_returns_failure() {
        let executor = executor(action("idle"));
        let mut context = FakeContext::default().with_action("idle", BehaviorStatus::Failure);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Failure
        );
    }

    #[test]
    fn action_running_returns_running() {
        let executor = executor(action("idle"));
        let mut context = FakeContext::default().with_action("idle", BehaviorStatus::Running);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Running
        );
    }

    #[test]
    fn sequence_all_success_returns_success() {
        let executor = executor(sequence(vec![condition("ready"), action("idle")]));
        let mut context = FakeContext::default()
            .with_condition("ready", BehaviorStatus::Success)
            .with_action("idle", BehaviorStatus::Success);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Success
        );
        assert_eq!(context.calls, ["condition:ready", "action:idle"]);
    }

    #[test]
    fn sequence_stops_on_first_failure() {
        let executor = executor(sequence(vec![condition("ready"), action("idle")]));
        let mut context = FakeContext::default()
            .with_condition("ready", BehaviorStatus::Failure)
            .with_action("idle", BehaviorStatus::Success);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Failure
        );
        assert_eq!(context.calls, ["condition:ready"]);
    }

    #[test]
    fn sequence_returns_running_when_child_running() {
        let executor = executor(sequence(vec![action("wait"), action("idle")]));
        let mut context = FakeContext::default()
            .with_action("wait", BehaviorStatus::Running)
            .with_action("idle", BehaviorStatus::Success);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Running
        );
        assert_eq!(context.calls, ["action:wait"]);
    }

    #[test]
    fn selector_returns_success_on_first_successful_child() {
        let executor = executor(selector(vec![condition("visible"), action("idle")]));
        let mut context = FakeContext::default()
            .with_condition("visible", BehaviorStatus::Success)
            .with_action("idle", BehaviorStatus::Success);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Success
        );
        assert_eq!(context.calls, ["condition:visible"]);
    }

    #[test]
    fn selector_all_failure_returns_failure() {
        let executor = executor(selector(vec![condition("visible"), action("idle")]));
        let mut context = FakeContext::default()
            .with_condition("visible", BehaviorStatus::Failure)
            .with_action("idle", BehaviorStatus::Failure);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Failure
        );
        assert_eq!(context.calls, ["condition:visible", "action:idle"]);
    }

    #[test]
    fn selector_returns_running_when_child_running() {
        let executor = executor(selector(vec![condition("visible"), action("idle")]));
        let mut context = FakeContext::default()
            .with_condition("visible", BehaviorStatus::Failure)
            .with_action("idle", BehaviorStatus::Running);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Running
        );
        assert_eq!(context.calls, ["condition:visible", "action:idle"]);
    }

    #[test]
    fn root_returns_child_status() {
        let executor = executor(action("idle"));
        let mut context = FakeContext::default().with_action("idle", BehaviorStatus::Failure);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Failure
        );
    }

    #[test]
    fn decorator_passes_through_child_status() {
        let executor = executor(decorator("invert", condition("visible")));
        let mut context = FakeContext::default().with_condition("visible", BehaviorStatus::Success);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Success
        );
        assert_eq!(context.calls, ["condition:visible"]);
    }

    #[test]
    fn context_error_preserves_actual_failure_cause() {
        let executor = executor(action("chase_player"));
        let mut context = FakeContext::default().with_action_error(
            "chase_player",
            FakeContextError::MissingTarget("chase_player".into()),
        );

        let error = executor.tick(&mut context).unwrap_err();
        assert!(matches!(
            error,
            BehaviorTreeRuntimeError::BehaviorDispatchFailed {
                dispatch: BehaviorDispatchKind::Action,
                ref behavior_id,
                source: FakeContextError::MissingTarget(_)
            } if behavior_id == "chase_player"
        ));
        assert!(error.to_string().contains("has no target"));
    }

    #[test]
    fn unregistered_behavior_returns_context_error_without_panic() {
        let executor = executor(action("missing"));
        let mut context = FakeContext::default();

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::BehaviorDispatchFailed {
                dispatch: BehaviorDispatchKind::Action,
                ref behavior_id,
                source: FakeContextError::Unregistered(_)
            } if behavior_id == "missing"
        ));
    }

    #[test]
    fn missing_behavior_id_returns_error_without_dispatch() {
        let executor = executor(node(BehaviorTreeNodeKind::Action, None, vec![]));
        let mut context = FakeContext::default();

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::MissingBehaviorId {
                kind: BehaviorTreeNodeKind::Action,
                ..
            }
        ));
        assert!(context.calls.is_empty());
    }

    #[test]
    fn invalid_compiled_tree_shape_returns_error() {
        let executor = BehaviorTreeExecutor::new(CompiledBehaviorTree {
            source: GraphId::generate(),
            root: node(BehaviorTreeNodeKind::Root, None, vec![]),
        });
        let mut context = FakeContext::default();

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::InvalidRootChildCount { count: 0, .. }
        ));
    }

    #[test]
    fn compiled_tree_root_must_be_root_kind() {
        let executor = BehaviorTreeExecutor::new(CompiledBehaviorTree {
            source: GraphId::generate(),
            root: action("idle"),
        });
        let mut context = FakeContext::default().with_action("idle", BehaviorStatus::Success);

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::InvalidRootKind { .. }
        ));
        assert!(context.calls.is_empty());
    }

    #[test]
    fn nested_root_returns_error_without_dispatch() {
        let executor = executor(sequence(vec![
            node(BehaviorTreeNodeKind::Root, None, vec![action("idle")]),
            action("after"),
        ]));
        let mut context = FakeContext::default()
            .with_action("idle", BehaviorStatus::Success)
            .with_action("after", BehaviorStatus::Success);

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::NestedRoot { .. }
        ));
        assert!(context.calls.is_empty());
    }

    #[test]
    fn excessive_runtime_depth_returns_error_without_stack_overflow() {
        let mut child = action("leaf");
        for _ in 0..MAX_BEHAVIOR_TREE_RUNTIME_DEPTH {
            child = decorator("pass", child);
        }
        let executor = executor(child);
        let mut context = FakeContext::default().with_action("leaf", BehaviorStatus::Success);

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::MaxDepthExceeded { .. }
        ));
        assert!(context.calls.is_empty());
    }

    #[test]
    fn tick_only_requires_shared_executor_access() {
        let executor = executor(action("idle"));
        let tree = executor.tree();
        let mut context = FakeContext::default().with_action("idle", BehaviorStatus::Success);

        assert_eq!(tree.root.kind, BehaviorTreeNodeKind::Root);
        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Success
        );
    }

    #[test]
    fn empty_sequence_returns_shape_error() {
        let executor = executor(sequence(vec![]));
        let mut context = FakeContext::default();

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::EmptyComposite {
                kind: BehaviorTreeNodeKind::Sequence,
                ..
            }
        ));
        assert!(context.calls.is_empty());
    }

    #[test]
    fn empty_selector_returns_shape_error() {
        let executor = executor(selector(vec![]));
        let mut context = FakeContext::default();

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::EmptyComposite {
                kind: BehaviorTreeNodeKind::Selector,
                ..
            }
        ));
        assert!(context.calls.is_empty());
    }

    #[test]
    fn decorator_without_behavior_id_returns_error_without_dispatch() {
        let executor = executor(node(
            BehaviorTreeNodeKind::Decorator,
            None,
            vec![action("idle")],
        ));
        let mut context = FakeContext::default().with_action("idle", BehaviorStatus::Success);

        assert!(matches!(
            executor.tick(&mut context).unwrap_err(),
            BehaviorTreeRuntimeError::MissingBehaviorId {
                kind: BehaviorTreeNodeKind::Decorator,
                ..
            }
        ));
        assert!(context.calls.is_empty());
    }

    #[test]
    fn deterministic_compiled_behavior_tree_can_be_executed() {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "runtime_smoke",
        );
        let root = NodeId::generate();
        let selector = NodeId::generate();
        let sequence = NodeId::generate();
        let visible = NodeId::generate();
        let chase = NodeId::generate();
        let patrol = NodeId::generate();

        let mut transaction = GraphTransaction::begin(&graph);
        transaction.apply(GraphCommand::AddNode {
            node: domain.root_node(root.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.selector_node(selector.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.sequence_node(sequence.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.condition_node(visible.clone(), "player_visible"),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.action_node(chase.clone(), "chase_player"),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.action_node(patrol.clone(), "patrol"),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), root, selector.clone(), 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), selector.clone(), sequence.clone(), 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), selector, patrol, 1),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), sequence.clone(), visible, 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), sequence, chase, 1),
        });
        transaction
            .commit(&mut graph, domain.schema_registry())
            .expect("test graph commands must commit");

        let compiled = domain.compile(&graph).expect("test graph must compile");
        let executor = BehaviorTreeExecutor::new(compiled);
        let mut context = FakeContext::default()
            .with_condition("player_visible", BehaviorStatus::Failure)
            .with_action("chase_player", BehaviorStatus::Success)
            .with_action("patrol", BehaviorStatus::Success);

        assert_eq!(
            executor.tick(&mut context).unwrap(),
            BehaviorStatus::Success
        );
        assert_eq!(context.calls, ["condition:player_visible", "action:patrol"]);
    }

    #[test]
    fn ecs_system_ticks_behavior_tree_runner_component() {
        let compiled = chase_or_patrol_tree();
        let mut app = engine_ecs::App::new();
        register_behavior_tree_system(&mut app).expect("system must register");
        {
            let registry = app
                .world_mut()
                .get_resource_mut::<BehaviorTreeBehaviorRegistry>()
                .expect("register must insert registry");
            registry
                .set_condition("player_visible", BehaviorStatus::Failure)
                .set_action("chase_player", BehaviorStatus::Success)
                .set_action("patrol", BehaviorStatus::Success);
        }
        let entity = app
            .world_mut()
            .spawn_with(BehaviorTreeRunner::new(compiled))
            .expect("runner entity must spawn");

        app.update().expect("behavior tree system must run");

        let runner = app
            .world()
            .get_component::<BehaviorTreeRunner>(entity)
            .expect("runner must remain on entity");
        assert_eq!(runner.last_status(), Some(BehaviorStatus::Success));
        assert_eq!(runner.last_error(), None);
        let registry = app
            .world()
            .get_resource::<BehaviorTreeBehaviorRegistry>()
            .expect("registry must remain available");
        assert_eq!(
            registry.calls(),
            [
                BehaviorTreeDispatchRecord {
                    kind: BehaviorDispatchKind::Condition,
                    behavior_id: "player_visible".into(),
                },
                BehaviorTreeDispatchRecord {
                    kind: BehaviorDispatchKind::Action,
                    behavior_id: "patrol".into(),
                },
            ]
        );
    }

    #[test]
    fn disabled_runner_preserves_blackboard_without_dispatching() {
        let compiled = chase_or_patrol_tree();
        let mut runner = BehaviorTreeRunner::with_blackboard(
            compiled,
            BTreeMap::from([("home".into(), Value::String("north".into()))]),
        );
        runner.set_enabled(false);
        let mut app = engine_ecs::App::new();
        register_behavior_tree_system(&mut app).expect("system must register");
        let entity = app
            .world_mut()
            .spawn_with(runner)
            .expect("runner entity must spawn");

        app.update()
            .expect("disabled runner system pass must succeed");

        let runner = app
            .world()
            .get_component::<BehaviorTreeRunner>(entity)
            .expect("runner must remain on entity");
        assert_eq!(runner.last_status(), None);
        assert_eq!(
            runner.blackboard().get("home"),
            Some(&Value::String("north".into()))
        );
        assert!(app
            .world()
            .get_resource::<BehaviorTreeBehaviorRegistry>()
            .expect("registry")
            .calls()
            .is_empty());
    }

    #[test]
    fn ecs_system_clears_dispatch_calls_each_update() {
        let compiled = chase_or_patrol_tree();
        let mut app = engine_ecs::App::new();
        register_behavior_tree_system(&mut app).expect("system must register");
        {
            let registry = app
                .world_mut()
                .get_resource_mut::<BehaviorTreeBehaviorRegistry>()
                .expect("register must insert registry");
            registry
                .set_condition("player_visible", BehaviorStatus::Failure)
                .set_action("chase_player", BehaviorStatus::Success)
                .set_action("patrol", BehaviorStatus::Success);
        }
        app.world_mut()
            .spawn_with(BehaviorTreeRunner::new(compiled))
            .expect("runner entity must spawn");

        app.update().expect("first update must run");
        app.update().expect("second update must run");

        let registry = app
            .world()
            .get_resource::<BehaviorTreeBehaviorRegistry>()
            .expect("registry must remain available");
        assert_eq!(
            registry.calls().len(),
            2,
            "dispatch calls must describe the latest update, not accumulate forever"
        );
    }

    fn chase_or_patrol_tree() -> CompiledBehaviorTree {
        let domain = BehaviorTreeDomain::new();
        let mut graph = Graph::new(
            GraphId::generate(),
            domain.graph_kind().clone(),
            "runtime_ecs_smoke",
        );
        let root = NodeId::generate();
        let selector = NodeId::generate();
        let sequence = NodeId::generate();
        let visible = NodeId::generate();
        let chase = NodeId::generate();
        let patrol = NodeId::generate();

        let mut transaction = GraphTransaction::begin(&graph);
        transaction.apply(GraphCommand::AddNode {
            node: domain.root_node(root.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.selector_node(selector.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.sequence_node(sequence.clone()),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.condition_node(visible.clone(), "player_visible"),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.action_node(chase.clone(), "chase_player"),
        });
        transaction.apply(GraphCommand::AddNode {
            node: domain.action_node(patrol.clone(), "patrol"),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), root, selector.clone(), 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), selector.clone(), sequence.clone(), 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), selector, patrol, 1),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), sequence.clone(), visible, 0),
        });
        transaction.apply(GraphCommand::AddEdge {
            edge: domain.child_edge(EdgeId::generate(), sequence, chase, 1),
        });
        transaction
            .commit(&mut graph, domain.schema_registry())
            .expect("test graph commands must commit");

        domain.compile(&graph).expect("test graph must compile")
    }

    fn executor(child: CompiledBehaviorNode) -> BehaviorTreeExecutor {
        BehaviorTreeExecutor::new(CompiledBehaviorTree {
            source: GraphId::generate(),
            root: node(BehaviorTreeNodeKind::Root, None, vec![child]),
        })
    }

    fn sequence(children: Vec<CompiledBehaviorNode>) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Sequence, None, children)
    }

    fn selector(children: Vec<CompiledBehaviorNode>) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Selector, None, children)
    }

    fn condition(behavior: &str) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Condition, Some(behavior), vec![])
    }

    fn action(behavior: &str) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Action, Some(behavior), vec![])
    }

    fn decorator(behavior: &str, child: CompiledBehaviorNode) -> CompiledBehaviorNode {
        node(BehaviorTreeNodeKind::Decorator, Some(behavior), vec![child])
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
}
