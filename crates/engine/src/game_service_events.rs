//! Shared bounded source log for asynchronous project gameplay services.
//!
//! Service logs are intentionally separate from `GameHostRuntime`'s
//! per-subscriber event history. A producer can run before or after a project
//! callback without losing its result; the host copies each source sequence
//! once and then applies normal per-system consumption cursors.

use engine_authoring::Value;
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GameSourceEvent {
    pub(crate) source_sequence: u64,
    pub(crate) payload: Value,
}

#[derive(Debug)]
pub(crate) struct GameSourceEventLog {
    label: &'static str,
    capacity: usize,
    next_sequence: u64,
    events: VecDeque<GameSourceEvent>,
}

impl GameSourceEventLog {
    pub(crate) fn new(label: &'static str, capacity: usize) -> Self {
        assert!(capacity > 0, "game source event capacity must be positive");
        Self {
            label,
            capacity,
            next_sequence: 0,
            events: VecDeque::new(),
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &GameSourceEvent> {
        self.events.iter()
    }

    pub(crate) fn push(&mut self, payload: Value) {
        if self.events.len() >= self.capacity {
            let dropped = self
                .events
                .pop_front()
                .expect("full source log has a front");
            log::warn!(
                "{} source log is full ({}); dropping source sequence {}",
                self.label,
                self.capacity,
                dropped.source_sequence
            );
        }
        self.next_sequence = self.next_sequence.saturating_add(1).max(1);
        self.events.push_back(GameSourceEvent {
            source_sequence: self.next_sequence,
            payload,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_log_drops_oldest_and_keeps_monotonic_sequences() {
        let mut log = GameSourceEventLog::new("test", 2);
        log.push(Value::I64(1));
        log.push(Value::I64(2));
        log.push(Value::I64(3));

        let events = log.iter().cloned().collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].source_sequence, 2);
        assert_eq!(events[1].source_sequence, 3);
        assert_eq!(events[1].payload, Value::I64(3));
    }
}
