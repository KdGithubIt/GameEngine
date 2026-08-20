//! VFX Timeline adapter at the final engine composition boundary (ADR 0126).
//!
//! The neutral Timeline core reports stable VFX clip boundaries. This adapter
//! resolves those authoring bindings to live scene entities and drives the
//! ADR 0125 `VfxPlayer` interface without moving VFX simulation ownership out
//! of `engine-render-runtime`.

mod replay;
mod runtime;

#[cfg(test)]
mod tests;

pub(super) use replay::{apply_seek, mark_seek, mark_seek_applied, pending_seek};
pub(super) use runtime::apply_evaluation;

use super::{
    TimelineBindingDiagnostic, TimelineBindings, TimelineDiagnostics, TimelinePlayerComponent,
};
use crate::vfx::VfxPlayer;
use engine_authoring::TimelineTick;
use engine_ecs::World;
use engine_timeline::{AdapterTokens, CompiledTrack, TimelinePlayState, VfxAction};

const PENDING_SEEK_GENERATION: &str = "engine.timeline.vfx.pending_seek_generation";
const APPLIED_SEEK_GENERATION: &str = "engine.timeline.vfx.applied_seek_generation";
const PLAY_TOKEN_PREFIX: &str = "engine.timeline.vfx.play:";

#[derive(Debug, Clone, Copy)]
pub(super) struct PendingSeek {
    generation: u64,
    tick: TimelineTick,
    state: TimelinePlayState,
}

fn apply_action(
    track: &CompiledTrack,
    action: VfxAction,
    world: &mut World,
    bindings: &TimelineBindings,
    diagnostics: &mut TimelineDiagnostics,
) {
    let Some(authoring) = track.entity.as_ref() else {
        return;
    };
    let Some(target) = bindings.resolve(authoring) else {
        diagnostics.push(TimelineBindingDiagnostic::UnresolvedEntity {
            authoring: authoring.as_stable_id().as_str().to_owned(),
        });
        return;
    };
    let Some(player) = world.get_component_mut::<VfxPlayer>(target) else {
        diagnostics.push(TimelineBindingDiagnostic::MissingComponent {
            authoring: authoring.as_stable_id().as_str().to_owned(),
            component: "VfxPlayer",
        });
        return;
    };
    match action {
        VfxAction::Play => player.play(),
        VfxAction::Stop => player.stop(),
        VfxAction::Restart => player.restart(),
    }
}

fn stop_track(
    track: &CompiledTrack,
    world: &mut World,
    bindings: &TimelineBindings,
    diagnostics: &mut TimelineDiagnostics,
) {
    apply_action(track, VfxAction::Stop, world, bindings, diagnostics);
}

fn play_token(track: &CompiledTrack) -> String {
    format!("{PLAY_TOKEN_PREFIX}{}", track.id.as_str())
}

fn play_owned(tokens: &AdapterTokens, track: &CompiledTrack) -> bool {
    tokens.get(&play_token(track)) == Some(1)
}

fn set_play_owned(tokens: &mut AdapterTokens, track: &CompiledTrack, owned: bool) {
    tokens.set(play_token(track), u64::from(owned));
}
