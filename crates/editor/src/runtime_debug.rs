//! Deterministic AI runtime-debugging control for Editor Play (ADR 0157).
//!
//! This application-layer controller composes existing Play primitives. It does
//! not create a second runtime clock, input path, replay format, or renderer.

use crate::runtime::{PlayTickError, RuntimePlayState};
use engine::{
    GamepadAxis, GamepadButton, GamepadId, InputCommand, InputReplay, InputSource, KeyCode,
    MouseButton,
};
use std::fmt;

const MAX_PLAN_INPUTS: usize = 4_096;
const MAX_PLAN_TICKS: u64 = 36_000;
const MAX_STEP_TICKS: u32 = 3_600;
const MAX_OBSERVED_ENTITIES: usize = 256;

/// One virtual-input command scheduled relative to the start of a deterministic
/// managed Play plan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RuntimeDebugScheduledInput {
    tick_offset: u64,
    command: InputCommand,
}

impl RuntimeDebugScheduledInput {
    /// Schedules one existing engine [`InputCommand`] at an exact fixed-tick offset.
    pub fn at_tick(tick_offset: u64, command: InputCommand) -> Result<Self, RuntimeDebugError> {
        validate_command(command)?;
        if tick_offset > MAX_PLAN_TICKS {
            return Err(RuntimeDebugError::BudgetExceeded(format!(
                "runtime input tick {tick_offset} exceeds the {MAX_PLAN_TICKS}-tick plan budget"
            )));
        }
        Ok(Self {
            tick_offset,
            command,
        })
    }

    /// Fixed-tick offset from the start of the managed plan.
    pub fn tick_offset(&self) -> u64 {
        self.tick_offset
    }

    /// Virtual input command that will be injected through `InputSource::AiAgent`.
    pub fn command(&self) -> InputCommand {
        self.command
    }
}

/// Frozen, bounded deterministic runtime-input plan.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeDebugPlan {
    inputs: Vec<RuntimeDebugScheduledInput>,
    end_tick: u64,
}

impl RuntimeDebugPlan {
    /// Creates a bounded plan. Inputs are stably ordered by fixed-tick offset.
    pub fn new(
        mut inputs: Vec<RuntimeDebugScheduledInput>,
        end_tick: u64,
    ) -> Result<Self, RuntimeDebugError> {
        if inputs.len() > MAX_PLAN_INPUTS {
            return Err(RuntimeDebugError::BudgetExceeded(format!(
                "runtime plan contains {} inputs; maximum is {MAX_PLAN_INPUTS}",
                inputs.len()
            )));
        }
        if end_tick > MAX_PLAN_TICKS {
            return Err(RuntimeDebugError::BudgetExceeded(format!(
                "runtime plan end tick {end_tick} exceeds the {MAX_PLAN_TICKS}-tick budget"
            )));
        }
        if let Some(last) = inputs
            .iter()
            .map(RuntimeDebugScheduledInput::tick_offset)
            .max()
            && last > end_tick
        {
            return Err(RuntimeDebugError::InvalidPlan(format!(
                "runtime input tick {last} is after plan end tick {end_tick}"
            )));
        }
        inputs.sort_by_key(RuntimeDebugScheduledInput::tick_offset);
        Ok(Self { inputs, end_tick })
    }

    /// Builds a compatibility plan where successive commands run on successive
    /// fixed ticks instead of successive UI/model frames.
    pub fn sequential(
        commands: impl IntoIterator<Item = InputCommand>,
    ) -> Result<Self, RuntimeDebugError> {
        let mut inputs = Vec::new();
        for (index, command) in commands.into_iter().enumerate() {
            let tick = u64::try_from(index).map_err(|_| {
                RuntimeDebugError::BudgetExceeded("runtime input sequence is too large".to_owned())
            })?;
            inputs.push(RuntimeDebugScheduledInput::at_tick(tick, command)?);
        }
        let end_tick = inputs
            .last()
            .map_or(0, RuntimeDebugScheduledInput::tick_offset);
        Self::new(inputs, end_tick)
    }

    /// Ordered scheduled inputs in this frozen plan.
    pub fn inputs(&self) -> &[RuntimeDebugScheduledInput] {
        &self.inputs
    }

    /// Final fixed-tick offset that belongs to the plan.
    pub fn end_tick(&self) -> u64 {
        self.end_tick
    }
}

/// One allowlisted, typed host-side runtime condition.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeDebugPredicate {
    kind: RuntimeDebugPredicateKind,
}

#[derive(Debug, Clone, PartialEq)]
enum RuntimeDebugPredicateKind {
    FixedTickAtLeast(u64),
    AuthoringEntityExists(String),
    AuthoringPositionNear {
        authoring_id: String,
        expected: [f32; 3],
        tolerance: f32,
    },
    InputActionPressed(String),
}

impl RuntimeDebugPredicate {
    /// Waits/asserts on the fixed simulation tick.
    pub fn fixed_tick_at_least(tick: u64) -> Self {
        Self {
            kind: RuntimeDebugPredicateKind::FixedTickAtLeast(tick),
        }
    }

    /// Waits/asserts until a stable authoring entity is present in the Play world.
    pub fn authoring_entity_exists(authoring_id: impl Into<String>) -> Self {
        Self {
            kind: RuntimeDebugPredicateKind::AuthoringEntityExists(authoring_id.into()),
        }
    }

    /// Waits/asserts until an authoring entity's local position is within
    /// `tolerance` of `expected`.
    pub fn authoring_position_near(
        authoring_id: impl Into<String>,
        expected: [f32; 3],
        tolerance: f32,
    ) -> Result<Self, RuntimeDebugError> {
        if expected.iter().any(|value| !value.is_finite())
            || !tolerance.is_finite()
            || tolerance < 0.0
        {
            return Err(RuntimeDebugError::InvalidPredicate(
                "position predicate values and tolerance must be finite and tolerance must be non-negative"
                    .to_owned(),
            ));
        }
        Ok(Self {
            kind: RuntimeDebugPredicateKind::AuthoringPositionNear {
                authoring_id: authoring_id.into(),
                expected,
                tolerance,
            },
        })
    }

    /// Waits/asserts on one existing typed Input Action by name.
    pub fn input_action_pressed(action: impl Into<String>) -> Self {
        Self {
            kind: RuntimeDebugPredicateKind::InputActionPressed(action.into()),
        }
    }
}

/// Bounded, read-only runtime evidence captured by the Editor host.
#[derive(Debug, Clone)]
pub struct RuntimeDebugObservation {
    fixed_tick: u64,
    paused: bool,
    entity_count: usize,
    entities: Vec<RuntimeDebugEntityObservation>,
    keyboard: Vec<String>,
    mouse_buttons: Vec<String>,
    gamepad_buttons: Vec<String>,
    gamepad_axes: Vec<String>,
    actions: Vec<(String, bool)>,
    last_tick_ms: f64,
    maximum_tick_ms: f64,
    average_tick_ms: f64,
}

#[derive(Debug, Clone)]
struct RuntimeDebugEntityObservation {
    authoring_id: Option<String>,
    name: String,
    components: Vec<String>,
    values: Vec<(String, String)>,
    transform: Option<([f32; 3], [f32; 3], [f32; 3])>,
}

impl RuntimeDebugObservation {
    /// Current fixed simulation tick.
    pub fn fixed_tick(&self) -> u64 {
        self.fixed_tick
    }

    /// Whether normal Play advancement is paused.
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Number of runtime entities in the current Play world.
    pub fn entity_count(&self) -> usize {
        self.entity_count
    }

    /// Produces a bounded provider-safe summary without exposing raw runtime memory.
    pub fn summary(&self) -> String {
        let entity_rows = self
            .entities
            .iter()
            .take(16)
            .map(|entity| {
                format!(
                    "{} authoring={:?} components={:?} values={:?} transform={:?}",
                    entity.name,
                    entity.authoring_id,
                    entity.components,
                    entity.values,
                    entity.transform
                )
            })
            .collect::<Vec<_>>()
            .join(" | ");
        format!(
            "fixed_tick={} paused={} entities={} keyboard={:?} mouse={:?} gamepad_buttons={:?} gamepad_axes={:?} actions={:?} timing_ms(last={:.3}, max={:.3}, avg={:.3}) rows=[{}]",
            self.fixed_tick,
            self.paused,
            self.entity_count,
            self.keyboard,
            self.mouse_buttons,
            self.gamepad_buttons,
            self.gamepad_axes,
            self.actions,
            self.last_tick_ms,
            self.maximum_tick_ms,
            self.average_tick_ms,
            entity_rows
        )
    }
}

/// Host-owned result of one deterministic input plan or replay.
#[derive(Debug, Clone)]
pub struct RuntimeDebugExecutionReport {
    start_fixed_tick: u64,
    end_fixed_tick: u64,
    scheduled_inputs: usize,
    cleanup_inputs: usize,
    replay_recorded: bool,
    observation: RuntimeDebugObservation,
}

impl RuntimeDebugExecutionReport {
    /// Fixed tick at which execution began.
    pub fn start_fixed_tick(&self) -> u64 {
        self.start_fixed_tick
    }

    /// Fixed tick observed after execution and cleanup.
    pub fn end_fixed_tick(&self) -> u64 {
        self.end_fixed_tick
    }

    /// Number of provider-planned virtual input commands executed.
    pub fn scheduled_inputs(&self) -> usize {
        self.scheduled_inputs
    }

    /// Number of host-generated release commands used to prevent stuck controls.
    pub fn cleanup_inputs(&self) -> usize {
        self.cleanup_inputs
    }

    /// Whether the execution produced an ADR 0064 replay artifact.
    pub fn replay_recorded(&self) -> bool {
        self.replay_recorded
    }

    /// Final structured runtime observation.
    pub fn observation(&self) -> &RuntimeDebugObservation {
        &self.observation
    }

    /// Compact audit description for Agent Host evidence.
    pub fn summary(&self) -> String {
        format!(
            "deterministic runtime plan fixed_ticks={}..{} scheduled_inputs={} cleanup_inputs={} replay_recorded={}; {}",
            self.start_fixed_tick,
            self.end_fixed_tick,
            self.scheduled_inputs,
            self.cleanup_inputs,
            self.replay_recorded,
            self.observation.summary()
        )
    }
}

/// Outcome of a bounded host-side wait.
#[derive(Debug, Clone)]
pub struct RuntimeDebugWaitResult {
    matched: bool,
    unavailable: Option<String>,
    advanced_ticks: u32,
    observation: RuntimeDebugObservation,
}

impl RuntimeDebugWaitResult {
    /// Whether the allowlisted predicate matched before the tick budget expired.
    pub fn matched(&self) -> bool {
        self.matched
    }

    /// Explicit reason that evidence was unavailable, if the requested typed
    /// datum does not exist in the current runtime.
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// Number of fixed ticks advanced while waiting.
    pub fn advanced_ticks(&self) -> u32 {
        self.advanced_ticks
    }

    /// Final structured runtime observation.
    pub fn observation(&self) -> &RuntimeDebugObservation {
        &self.observation
    }

    /// Compact host-evaluated result description.
    pub fn summary(&self) -> String {
        match &self.unavailable {
            Some(reason) => format!(
                "runtime wait unavailable after {} tick(s): {reason}; {}",
                self.advanced_ticks,
                self.observation.summary()
            ),
            None => format!(
                "runtime wait matched={} after {} tick(s); {}",
                self.matched,
                self.advanced_ticks,
                self.observation.summary()
            ),
        }
    }
}

/// Deterministic runtime-debugging failure.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeDebugError {
    /// A frozen plan or command violates deterministic-plan constraints.
    InvalidPlan(String),
    /// A typed predicate contains invalid values.
    InvalidPredicate(String),
    /// A bounded operation exceeded its declared host budget.
    BudgetExceeded(String),
    /// An operation requiring Pause was attempted while Play was running freely.
    NotPaused,
    /// Replay recording/playback could not start or complete.
    Replay(String),
    /// A normal runtime schedule failed while the host was stepping it.
    Tick(String),
}

impl fmt::Display for RuntimeDebugError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlan(message) => write!(formatter, "invalid runtime debug plan: {message}"),
            Self::InvalidPredicate(message) => {
                write!(formatter, "invalid runtime debug predicate: {message}")
            }
            Self::BudgetExceeded(message) => {
                write!(formatter, "runtime debug budget exceeded: {message}")
            }
            Self::NotPaused => write!(formatter, "runtime debug step/wait requires paused Play"),
            Self::Replay(message) => write!(formatter, "runtime replay failed: {message}"),
            Self::Tick(message) => write!(formatter, "runtime debug tick failed: {message}"),
        }
    }
}

impl std::error::Error for RuntimeDebugError {}

pub(crate) struct RuntimeDebugPlanOutcome {
    pub(crate) report: RuntimeDebugExecutionReport,
    pub(crate) replay: Option<InputReplay>,
}

pub(crate) fn capture_observation(runtime: &RuntimePlayState) -> RuntimeDebugObservation {
    let performance = runtime.performance_snapshot();
    let input = runtime.input_debug_snapshot();
    let entities = runtime
        .entity_debug_snapshot()
        .into_iter()
        .take(MAX_OBSERVED_ENTITIES)
        .map(|entity| {
            let values = runtime.entity_component_values(entity.entity);
            RuntimeDebugEntityObservation {
                authoring_id: entity.authoring_id,
                name: entity.name,
                components: entity.components.into_iter().map(str::to_owned).collect(),
                values,
                transform: entity.transform,
            }
        })
        .collect();
    RuntimeDebugObservation {
        fixed_tick: runtime.fixed_step_count(),
        paused: runtime.is_paused(),
        entity_count: performance.entity_count,
        entities,
        keyboard: input.keyboard,
        mouse_buttons: input.mouse_buttons,
        gamepad_buttons: input.gamepad_buttons,
        gamepad_axes: input.gamepad_axes,
        actions: input
            .actions
            .into_iter()
            .map(|(name, state)| (name, state.pressed))
            .collect(),
        last_tick_ms: performance.last_tick_ms,
        maximum_tick_ms: performance.maximum_tick_ms,
        average_tick_ms: performance.average_tick_ms,
    }
}

pub(crate) fn execute_plan(
    runtime: &mut RuntimePlayState,
    plan: &RuntimeDebugPlan,
) -> Result<RuntimeDebugPlanOutcome, RuntimeDebugError> {
    if runtime.is_replaying() || runtime.is_replay_recording() {
        return Err(RuntimeDebugError::InvalidPlan(
            "managed deterministic input cannot replace an active replay/recording".to_owned(),
        ));
    }
    runtime.set_paused(true);
    runtime
        .start_replay_recording()
        .map_err(|error| RuntimeDebugError::Replay(error.to_string()))?;
    let start_fixed_tick = runtime.fixed_step_count();
    let mut held = Vec::new();
    let result = (|| {
        // Synchronous managed execution blocks new human window events. This
        // explicit boundary also neutralizes any pre-existing human state.
        runtime.queue_input(InputSource::AiAgent, InputCommand::ReleaseAll);
        for relative_tick in 0..=plan.end_tick {
            for scheduled in plan
                .inputs
                .iter()
                .filter(|scheduled| scheduled.tick_offset == relative_tick)
            {
                update_held_inputs(&mut held, scheduled.command);
                runtime.queue_input(InputSource::AiAgent, scheduled.command);
            }
            runtime.tick_fixed_debug_step().map_err(tick_error)?;
        }
        let cleanup = cleanup_commands(&held);
        for command in &cleanup {
            runtime.queue_input(InputSource::AiAgent, *command);
        }
        if !cleanup.is_empty() {
            runtime.tick_fixed_debug_step().map_err(tick_error)?;
        }
        Ok::<_, RuntimeDebugError>(cleanup.len())
    })();

    let replay = runtime.stop_replay_recording();
    runtime.set_paused(true);
    match result {
        Ok(cleanup_inputs) => Ok(RuntimeDebugPlanOutcome {
            report: RuntimeDebugExecutionReport {
                start_fixed_tick,
                end_fixed_tick: runtime.fixed_step_count(),
                scheduled_inputs: plan.inputs.len(),
                cleanup_inputs,
                replay_recorded: replay.is_some(),
                observation: capture_observation(runtime),
            },
            replay,
        }),
        Err(error) => {
            runtime.queue_input(InputSource::AiAgent, InputCommand::ReleaseAll);
            Err(error)
        }
    }
}

pub(crate) fn step_paused(
    runtime: &mut RuntimePlayState,
    steps: u32,
) -> Result<RuntimeDebugObservation, RuntimeDebugError> {
    if !runtime.is_paused() {
        return Err(RuntimeDebugError::NotPaused);
    }
    if steps == 0 || steps > MAX_STEP_TICKS {
        return Err(RuntimeDebugError::BudgetExceeded(format!(
            "step count must be between 1 and {MAX_STEP_TICKS}"
        )));
    }
    for _ in 0..steps {
        runtime.tick_fixed_debug_step().map_err(tick_error)?;
    }
    Ok(capture_observation(runtime))
}

pub(crate) fn wait_until(
    runtime: &mut RuntimePlayState,
    predicate: &RuntimeDebugPredicate,
    max_ticks: u32,
) -> Result<RuntimeDebugWaitResult, RuntimeDebugError> {
    if !runtime.is_paused() {
        return Err(RuntimeDebugError::NotPaused);
    }
    if max_ticks > MAX_STEP_TICKS {
        return Err(RuntimeDebugError::BudgetExceeded(format!(
            "wait tick budget {max_ticks} exceeds {MAX_STEP_TICKS}"
        )));
    }

    for advanced_ticks in 0..=max_ticks {
        let observation = capture_observation(runtime);
        match evaluate_predicate(&observation, predicate) {
            PredicateEvaluation::Matched => {
                return Ok(RuntimeDebugWaitResult {
                    matched: true,
                    unavailable: None,
                    advanced_ticks,
                    observation,
                });
            }
            PredicateEvaluation::Unavailable(reason) => {
                return Ok(RuntimeDebugWaitResult {
                    matched: false,
                    unavailable: Some(reason),
                    advanced_ticks,
                    observation,
                });
            }
            PredicateEvaluation::NotMatched if advanced_ticks == max_ticks => {
                return Ok(RuntimeDebugWaitResult {
                    matched: false,
                    unavailable: None,
                    advanced_ticks,
                    observation,
                });
            }
            PredicateEvaluation::NotMatched => {
                runtime.tick_fixed_debug_step().map_err(tick_error)?;
            }
        }
    }
    unreachable!("bounded runtime wait loop always returns");
}

pub(crate) fn assert_predicate(
    runtime: &RuntimePlayState,
    predicate: &RuntimeDebugPredicate,
) -> RuntimeDebugWaitResult {
    let observation = capture_observation(runtime);
    match evaluate_predicate(&observation, predicate) {
        PredicateEvaluation::Matched => RuntimeDebugWaitResult {
            matched: true,
            unavailable: None,
            advanced_ticks: 0,
            observation,
        },
        PredicateEvaluation::NotMatched => RuntimeDebugWaitResult {
            matched: false,
            unavailable: None,
            advanced_ticks: 0,
            observation,
        },
        PredicateEvaluation::Unavailable(reason) => RuntimeDebugWaitResult {
            matched: false,
            unavailable: Some(reason),
            advanced_ticks: 0,
            observation,
        },
    }
}

pub(crate) fn replay_to_completion(
    runtime: &mut RuntimePlayState,
    replay: InputReplay,
) -> Result<RuntimeDebugExecutionReport, RuntimeDebugError> {
    runtime.set_paused(true);
    runtime
        .start_replay(replay)
        .map_err(|error| RuntimeDebugError::Replay(error.to_string()))?;
    let start_fixed_tick = runtime.fixed_step_count();
    let mut steps = 0_u64;
    let replay_budget = MAX_PLAN_TICKS.saturating_add(2);
    while runtime.is_replaying() {
        if steps >= replay_budget {
            return Err(RuntimeDebugError::BudgetExceeded(format!(
                "replay exceeded the {replay_budget}-tick host budget"
            )));
        }
        runtime.tick_fixed_debug_step().map_err(tick_error)?;
        steps = steps.saturating_add(1);
    }
    runtime.set_paused(true);
    Ok(RuntimeDebugExecutionReport {
        start_fixed_tick,
        end_fixed_tick: runtime.fixed_step_count(),
        scheduled_inputs: 0,
        cleanup_inputs: 0,
        replay_recorded: false,
        observation: capture_observation(runtime),
    })
}

fn validate_command(command: InputCommand) -> Result<(), RuntimeDebugError> {
    let finite = match command {
        InputCommand::MouseMove { position } => position.0.is_finite() && position.1.is_finite(),
        InputCommand::MouseDelta { delta } => delta.0.is_finite() && delta.1.is_finite(),
        InputCommand::MouseScroll { amount } => amount.is_finite(),
        InputCommand::GamepadAxis { value, .. } => value.is_finite(),
        _ => true,
    };
    if finite {
        Ok(())
    } else {
        Err(RuntimeDebugError::InvalidPlan(
            "runtime input numeric values must be finite".to_owned(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldInput {
    Key(KeyCode),
    Mouse(MouseButton),
    GamepadButton(GamepadId, GamepadButton),
    GamepadAxis(GamepadId, GamepadAxis),
}

fn update_held_inputs(held: &mut Vec<HeldInput>, command: InputCommand) {
    match command {
        InputCommand::Key { key, pressed } => set_held(held, HeldInput::Key(key), pressed),
        InputCommand::MouseButton { button, pressed } => {
            set_held(held, HeldInput::Mouse(button), pressed)
        }
        InputCommand::GamepadButton {
            gamepad,
            button,
            pressed,
        } => set_held(held, HeldInput::GamepadButton(gamepad, button), pressed),
        InputCommand::GamepadAxis {
            gamepad,
            axis,
            value,
        } => set_held(
            held,
            HeldInput::GamepadAxis(gamepad, axis),
            value.abs() > f32::EPSILON,
        ),
        InputCommand::GamepadDisconnected { gamepad } => {
            held.retain(|value| match value {
                HeldInput::GamepadButton(id, _) | HeldInput::GamepadAxis(id, _) => *id != gamepad,
                HeldInput::Key(_) | HeldInput::Mouse(_) => true,
            });
        }
        InputCommand::ReleaseAll => held.clear(),
        InputCommand::GamepadConnected { .. }
        | InputCommand::MouseMove { .. }
        | InputCommand::MouseDelta { .. }
        | InputCommand::MouseScroll { .. } => {}
        _ => {}
    }
}

fn set_held(held: &mut Vec<HeldInput>, value: HeldInput, active: bool) {
    if active {
        if !held.contains(&value) {
            held.push(value);
        }
    } else {
        held.retain(|held| *held != value);
    }
}

fn cleanup_commands(held: &[HeldInput]) -> Vec<InputCommand> {
    held.iter()
        .map(|held| match *held {
            HeldInput::Key(key) => InputCommand::Key {
                key,
                pressed: false,
            },
            HeldInput::Mouse(button) => InputCommand::MouseButton {
                button,
                pressed: false,
            },
            HeldInput::GamepadButton(gamepad, button) => InputCommand::GamepadButton {
                gamepad,
                button,
                pressed: false,
            },
            HeldInput::GamepadAxis(gamepad, axis) => InputCommand::GamepadAxis {
                gamepad,
                axis,
                value: 0.0,
            },
        })
        .collect()
}

enum PredicateEvaluation {
    Matched,
    NotMatched,
    Unavailable(String),
}

fn evaluate_predicate(
    observation: &RuntimeDebugObservation,
    predicate: &RuntimeDebugPredicate,
) -> PredicateEvaluation {
    match &predicate.kind {
        RuntimeDebugPredicateKind::FixedTickAtLeast(expected) => {
            if observation.fixed_tick >= *expected {
                PredicateEvaluation::Matched
            } else {
                PredicateEvaluation::NotMatched
            }
        }
        RuntimeDebugPredicateKind::AuthoringEntityExists(authoring_id) => {
            if observation
                .entities
                .iter()
                .any(|entity| entity.authoring_id.as_deref() == Some(authoring_id.as_str()))
            {
                PredicateEvaluation::Matched
            } else {
                PredicateEvaluation::NotMatched
            }
        }
        RuntimeDebugPredicateKind::AuthoringPositionNear {
            authoring_id,
            expected,
            tolerance,
        } => {
            let Some(entity) = observation
                .entities
                .iter()
                .find(|entity| entity.authoring_id.as_deref() == Some(authoring_id.as_str()))
            else {
                return PredicateEvaluation::Unavailable(format!(
                    "authoring entity `{authoring_id}` is not present in bounded runtime observation"
                ));
            };
            let Some((position, _, _)) = entity.transform else {
                return PredicateEvaluation::Unavailable(format!(
                    "authoring entity `{authoring_id}` has no runtime Transform"
                ));
            };
            let within = position
                .iter()
                .zip(expected)
                .all(|(actual, expected)| (*actual - *expected).abs() <= *tolerance);
            if within {
                PredicateEvaluation::Matched
            } else {
                PredicateEvaluation::NotMatched
            }
        }
        RuntimeDebugPredicateKind::InputActionPressed(action) => {
            let Some((_, pressed)) = observation.actions.iter().find(|(name, _)| name == action)
            else {
                return PredicateEvaluation::Unavailable(format!(
                    "input action `{action}` is not available in the current project action map"
                ));
            };
            if *pressed {
                PredicateEvaluation::Matched
            } else {
                PredicateEvaluation::NotMatched
            }
        }
    }
}

fn tick_error(error: PlayTickError) -> RuntimeDebugError {
    RuntimeDebugError::Tick(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_orders_same_tick_inputs_without_using_wall_clock() {
        let plan = RuntimeDebugPlan::new(
            vec![
                RuntimeDebugScheduledInput::at_tick(
                    48,
                    InputCommand::Key {
                        key: KeyCode::KeyW,
                        pressed: false,
                    },
                )
                .unwrap(),
                RuntimeDebugScheduledInput::at_tick(
                    0,
                    InputCommand::Key {
                        key: KeyCode::KeyW,
                        pressed: true,
                    },
                )
                .unwrap(),
            ],
            48,
        )
        .unwrap();
        assert_eq!(plan.inputs()[0].tick_offset(), 0);
        assert_eq!(plan.inputs()[1].tick_offset(), 48);
    }

    #[test]
    fn held_inputs_generate_source_scoped_release_commands() {
        let mut held = Vec::new();
        update_held_inputs(
            &mut held,
            InputCommand::Key {
                key: KeyCode::KeyW,
                pressed: true,
            },
        );
        update_held_inputs(
            &mut held,
            InputCommand::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
        );
        let cleanup = cleanup_commands(&held);
        assert_eq!(
            cleanup,
            vec![
                InputCommand::Key {
                    key: KeyCode::KeyW,
                    pressed: false,
                },
                InputCommand::MouseButton {
                    button: MouseButton::Left,
                    pressed: false,
                },
            ],
        );
    }

    #[test]
    fn invalid_numeric_input_is_rejected_before_play() {
        let error = RuntimeDebugScheduledInput::at_tick(
            0,
            InputCommand::MouseScroll { amount: f32::NAN },
        )
        .unwrap_err();
        assert!(matches!(error, RuntimeDebugError::InvalidPlan(_)));
    }

    fn runtime_fixture() -> RuntimePlayState {
        const MINIMAL_SCENE: &str =
            include_str!("../../engine/assets/scenes/minimal.scene.json");
        let scene = engine_authoring::load_scene_from_json(MINIMAL_SCENE)
            .expect("minimal runtime-debug fixture must load");
        RuntimePlayState::start(&scene, None)
            .expect("runtime-debug fixture must start Play")
            .state
    }

    #[test]
    fn fixed_tick_plan_executes_and_releases_ai_owned_input() {
        let mut runtime = runtime_fixture();
        runtime.set_paused(true);
        let plan = RuntimeDebugPlan::new(
            vec![
                RuntimeDebugScheduledInput::at_tick(
                    0,
                    InputCommand::Key {
                        key: KeyCode::KeyW,
                        pressed: true,
                    },
                )
                .unwrap(),
                RuntimeDebugScheduledInput::at_tick(
                    2,
                    InputCommand::Key {
                        key: KeyCode::KeyW,
                        pressed: false,
                    },
                )
                .unwrap(),
            ],
            2,
        )
        .unwrap();

        let outcome = execute_plan(&mut runtime, &plan).expect("fixed-tick plan must execute");

        assert_eq!(outcome.report.start_fixed_tick(), 0);
        assert_eq!(outcome.report.end_fixed_tick(), 3);
        assert_eq!(outcome.report.scheduled_inputs(), 2);
        assert_eq!(outcome.report.cleanup_inputs(), 0);
        assert!(outcome.report.replay_recorded());
        assert!(runtime.is_paused());
        assert!(!runtime
            .input_debug_snapshot()
            .keyboard
            .iter()
            .any(|key| key == "KeyW"));
    }

    #[test]
    fn unfinished_hold_gets_host_cleanup_and_replay_is_reproducible() {
        let mut runtime = runtime_fixture();
        runtime.set_paused(true);
        let plan = RuntimeDebugPlan::new(
            vec![RuntimeDebugScheduledInput::at_tick(
                0,
                InputCommand::Key {
                    key: KeyCode::KeyW,
                    pressed: true,
                },
            )
            .unwrap()],
            1,
        )
        .unwrap();

        let outcome = execute_plan(&mut runtime, &plan).expect("hold plan must execute");
        assert_eq!(outcome.report.cleanup_inputs(), 1);
        assert!(!runtime
            .input_debug_snapshot()
            .keyboard
            .iter()
            .any(|key| key == "KeyW"));

        let replay = outcome.replay.expect("managed plan must retain ADR 0064 replay");
        let mut reproduced = runtime_fixture();
        let report = replay_to_completion(&mut reproduced, replay)
            .expect("recorded fixed-tick plan must replay");
        assert!(reproduced.is_paused());
        assert!(report.end_fixed_tick() >= report.start_fixed_tick());
        assert!(!reproduced
            .input_debug_snapshot()
            .keyboard
            .iter()
            .any(|key| key == "KeyW"));
    }

    #[test]
    fn paused_step_and_wait_use_host_fixed_tick_budget() {
        let mut runtime = runtime_fixture();
        runtime.set_paused(true);
        let before = runtime.fixed_step_count();

        let observation = step_paused(&mut runtime, 1).expect("single step must succeed");
        assert!(observation.paused());
        assert_eq!(observation.fixed_tick(), before + 1);

        let wait = wait_until(
            &mut runtime,
            &RuntimeDebugPredicate::fixed_tick_at_least(before + 3),
            3,
        )
        .expect("bounded fixed-tick wait must succeed");
        assert!(wait.matched());
        assert!(wait.unavailable().is_none());
        assert_eq!(wait.observation().fixed_tick(), before + 3);
    }
}
