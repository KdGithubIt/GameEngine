//! Gameplay-owned reusable ability, AI, combat, targeting, and player contracts.

#![warn(missing_docs)]

/// Data-driven ability timing and state machines.
pub mod ability;
/// Compiled Behavior Tree runtime execution.
pub mod behavior_tree;
/// Stateful Behavior Tree execution and runtime debugging.
pub mod behavior_tree_stateful;
/// Deterministic combat damage, hit results, and knockback processing.
pub mod combat;
/// Attack-hitbox metadata shared by collision and combat adapters.
pub mod hitbox;
/// Lock-on target markers and request state independent of camera policy.
pub mod lock_on;
/// Player configuration and captured logical movement state.
pub mod player;
