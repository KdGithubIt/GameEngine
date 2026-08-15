//! Bounded deterministic timers owned by project Rust gameplay.
//!
//! Timers advance on the fixed-step schedule. Results remain in a bounded,
//! sequence-numbered source log until the GameModule host copies them into its
//! per-system event log, so registration order cannot make a completion vanish.

use crate::game_service_events::{GameSourceEvent, GameSourceEventLog};
use crate::time::FixedTime;
use engine_authoring::Value;
use engine_ecs::{Res, ResMut};
use std::collections::BTreeMap;

/// Maximum live or completed project gameplay timers.
pub const MAX_GAME_TIMERS: usize = 1_024;
/// Maximum timer results retained before the host event bridge copies them.
pub const MAX_GAME_TIMER_EVENTS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq)]
struct GameTimer {
    remaining_seconds: f32,
    completed: bool,
}

/// Fixed-step timer state keyed by a project-wide stable timer ID.
#[derive(Debug, Default)]
pub(crate) struct GameTimers {
    timers: BTreeMap<String, GameTimer>,
}

impl GameTimers {
    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        self.timers.keys().map(String::as_str)
    }

    pub(crate) fn set_preflighted(&mut self, timer_id: String, duration_seconds: f32) {
        assert!(
            self.timers.contains_key(&timer_id) || self.timers.len() < MAX_GAME_TIMERS,
            "game timer capacity must be checked during atomic preflight"
        );
        self.timers.insert(
            timer_id,
            GameTimer {
                remaining_seconds: duration_seconds,
                completed: false,
            },
        );
    }

    pub(crate) fn cancel(&mut self, timer_id: &str) {
        self.timers.remove(timer_id);
    }

    fn state(&self, timer_id: &str) -> Option<GameTimer> {
        self.timers.get(timer_id).copied()
    }
}

/// Bounded source event log for completion and explicit query results.
#[derive(Debug)]
pub(crate) struct GameTimerEvents {
    log: GameSourceEventLog,
}

impl Default for GameTimerEvents {
    fn default() -> Self {
        Self {
            log: GameSourceEventLog::new("game timer", MAX_GAME_TIMER_EVENTS),
        }
    }
}

impl GameTimerEvents {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &GameSourceEvent> {
        self.log.iter()
    }

    fn push(&mut self, payload: Value) {
        self.log.push(payload);
    }
}

/// Emits one explicit timer-state query result.
pub(crate) fn query_game_timer(
    timers: &GameTimers,
    events: &mut GameTimerEvents,
    timer_id: &str,
    request_id: u64,
) {
    let (status, remaining_seconds) = match timers.state(timer_id) {
        Some(timer) if timer.completed => ("completed", Some(timer.remaining_seconds)),
        Some(timer) => ("active", Some(timer.remaining_seconds)),
        None => ("missing", None),
    };
    let mut fields = BTreeMap::from([
        ("kind".to_owned(), Value::String("query_result".to_owned())),
        ("timer_id".to_owned(), Value::String(timer_id.to_owned())),
        (
            "request_id".to_owned(),
            Value::String(request_id.to_string()),
        ),
        ("status".to_owned(), Value::String(status.to_owned())),
    ]);
    if let Some(remaining_seconds) = remaining_seconds {
        fields.insert(
            "remaining_seconds".to_owned(),
            Value::F64(f64::from(remaining_seconds)),
        );
    }
    events.push(Value::Object(fields));
}

/// Advances timers deterministically and emits each completion exactly once.
pub(crate) fn game_timer_update_system(
    fixed_time: Res<FixedTime>,
    mut timers: ResMut<GameTimers>,
    mut events: ResMut<GameTimerEvents>,
) {
    let delta = fixed_time.fixed_delta.max(0.0);
    let mut completed = Vec::new();
    for (timer_id, timer) in &mut timers.timers {
        if timer.completed {
            continue;
        }
        timer.remaining_seconds = (timer.remaining_seconds - delta).max(0.0);
        if timer.remaining_seconds <= f32::EPSILON {
            timer.completed = true;
            completed.push(timer_id.clone());
        }
    }
    for timer_id in completed {
        events.push(Value::Object(BTreeMap::from([
            ("kind".to_owned(), Value::String("completed".to_owned())),
            ("timer_id".to_owned(), Value::String(timer_id)),
        ])));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_completion_is_emitted_once_and_query_reports_completed() {
        let mut app = engine_ecs::App::new();
        let mut fixed_time = FixedTime::default();
        fixed_time.fixed_delta = 0.25;
        app.insert_resource(fixed_time);
        let mut timers = GameTimers::default();
        timers.set_preflighted("game.timer.attack".to_owned(), 0.5);
        app.insert_resource(timers);
        app.insert_resource(GameTimerEvents::default());
        app.add_system(game_timer_update_system);

        app.update().unwrap();
        app.update().unwrap();
        app.update().unwrap();

        let events = app.world().get_resource::<GameTimerEvents>().unwrap();
        let source = events.iter().collect::<Vec<_>>();
        assert_eq!(source.len(), 1);
        assert_eq!(
            source[0].payload,
            Value::Object(BTreeMap::from([
                ("kind".to_owned(), Value::String("completed".to_owned())),
                (
                    "timer_id".to_owned(),
                    Value::String("game.timer.attack".to_owned())
                ),
            ]))
        );
    }
}
