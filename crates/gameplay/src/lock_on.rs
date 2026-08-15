//! Lock-on target markers and request state independent of camera selection policy.

use engine_ecs::Entity;

/// Marks an entity as a valid lock-on target for [`TargetLock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LockOnTarget {
    /// Team identifier used by the active lock-on selection policy.
    pub team: u32,
}

/// A queued [`TargetLock`] mutation applied once by the composition-level selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum LockRequest {
    /// Lock onto the nearest valid target.
    Acquire,
    /// Advance to the next valid target.
    Cycle,
    /// Clear the current lock.
    Release,
}

/// Tracks the currently locked-on entity and queues target-selection requests.
///
/// Only the most recent request made within a frame is retained. Camera,
/// distance, team, and line-of-sight policy remain composition-level concerns.
#[derive(Debug, Default)]
pub struct TargetLock {
    /// Current target exposed only for the composition-level selection adapter.
    #[doc(hidden)]
    pub current: Option<Entity>,
    /// Pending request exposed only for the composition-level selection adapter.
    #[doc(hidden)]
    pub pending: Option<LockRequest>,
}

impl TargetLock {
    /// Returns the currently locked-on entity, if any.
    pub fn current(&self) -> Option<Entity> {
        self.current
    }

    /// Queues a request to lock onto the nearest valid target.
    pub fn request_acquire(&mut self) {
        self.pending = Some(LockRequest::Acquire);
    }

    /// Queues a request to advance to the next valid target.
    pub fn request_cycle(&mut self) {
        self.pending = Some(LockRequest::Cycle);
    }

    /// Queues a request to clear the current lock.
    pub fn request_release(&mut self) {
        self.pending = Some(LockRequest::Release);
    }

    /// Takes the pending request for the composition-level selection system.
    #[doc(hidden)]
    pub fn take_pending_request(&mut self) -> Option<LockRequest> {
        self.pending.take()
    }

    /// Replaces the selected entity after validation or request processing.
    #[doc(hidden)]
    pub fn set_current(&mut self, current: Option<Entity>) {
        self.current = current;
    }

    /// Reports whether a request is waiting to be processed.
    #[doc(hidden)]
    pub fn has_pending_request(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_request_wins_and_take_clears_it() {
        let mut lock = TargetLock::default();
        lock.request_acquire();
        lock.request_release();

        assert_eq!(lock.take_pending_request(), Some(LockRequest::Release));
        assert!(!lock.has_pending_request());
    }

    #[test]
    fn adapter_can_replace_current_without_changing_pending_request() {
        let mut lock = TargetLock::default();
        let target = Entity::from_raw(7, 0);
        lock.request_cycle();

        lock.set_current(Some(target));

        assert_eq!(lock.current(), Some(target));
        assert!(lock.has_pending_request());
    }
}
