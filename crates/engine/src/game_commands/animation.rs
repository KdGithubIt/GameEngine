//! Animation-family command preparation and application.
//!
//! Asset lookup and component-specific mutation stay in this module so the
//! top-level command pipeline can preserve atomic preflight.

use super::{bool_field, number_field, object, runtime_id_field, string_field, GameCommandError};
use crate::anim_graph::AnimGraphPlayer;
use crate::animation::{AnimationClip, Animator};
use crate::animation_parameters::AnimationParameterKind;
use crate::asset::{Assets, Handle};
use crate::game_io::GameEntityHandle;
use engine_authoring::Value;
use engine_ecs::{Entity, World};

pub(crate) enum PreparedAnimationCommand {
    Play {
        entity: Entity,
        looping: bool,
    },
    Crossfade {
        entity: Entity,
        clip: Handle<AnimationClip>,
        duration_seconds: f32,
        looping: bool,
    },
    Stop {
        entity: Entity,
    },
    SetBool {
        entity: Entity,
        name: String,
        value: bool,
    },
    SetFloat {
        entity: Entity,
        name: String,
        value: f32,
    },
    Trigger {
        entity: Entity,
        name: String,
    },
}

pub(super) fn prepare(
    world: &World,
    index: usize,
    target: GameEntityHandle,
    entity: Entity,
    payload: &Value,
) -> Result<PreparedAnimationCommand, GameCommandError> {
    let fields = object(payload, index, "animation payload")?;
    match string_field(fields, "operation", index)? {
        "play" => {
            require_animator(world, index, target, entity)?;
            Ok(PreparedAnimationCommand::Play {
                entity,
                looping: bool_field(fields, "looping", index)?,
            })
        }
        "crossfade" => {
            require_animator(world, index, target, entity)?;
            let clip_id = runtime_id_field(fields, "clip_runtime_id", index)?;
            let clip = world
                .get_resource::<Assets<AnimationClip>>()
                .and_then(|assets| {
                    assets
                        .iter()
                        .find(|(id, _)| id.value() == clip_id)
                        .and_then(|(id, _)| assets.handle(id))
                })
                .ok_or(GameCommandError::MissingAnimationClip { index, clip_id })?;
            let duration_seconds = number_field(fields, "duration_seconds", index)?;
            if duration_seconds < 0.0 {
                return Err(GameCommandError::InvalidPayload {
                    index,
                    message: "field `duration_seconds` must be zero or positive".to_owned(),
                });
            }
            Ok(PreparedAnimationCommand::Crossfade {
                entity,
                clip,
                duration_seconds,
                looping: bool_field(fields, "looping", index)?,
            })
        }
        "stop" => {
            require_animator(world, index, target, entity)?;
            Ok(PreparedAnimationCommand::Stop { entity })
        }
        "set_bool" => {
            let player = require_graph_player(world, index, target, entity)?;
            prepare_bool_parameter(
                player,
                index,
                entity,
                string_field(fields, "name", index)?,
                bool_field(fields, "value", index)?,
            )
        }
        "set_float" => {
            let player = require_graph_player(world, index, target, entity)?;
            prepare_float_parameter(
                player,
                index,
                entity,
                string_field(fields, "name", index)?,
                number_field(fields, "value", index)?,
            )
        }
        "trigger" => {
            let player = require_graph_player(world, index, target, entity)?;
            prepare_trigger_parameter(
                player,
                index,
                entity,
                string_field(fields, "name", index)?,
            )
        }
        other => Err(GameCommandError::InvalidPayload {
            index,
            message: format!("unknown animation operation `{other}`"),
        }),
    }
}

pub(super) fn apply(world: &mut World, command: PreparedAnimationCommand) {
    match command {
        PreparedAnimationCommand::Play { entity, looping } => {
            let animator = world
                .get_component_mut::<Animator>(entity)
                .expect("preflighted animator must remain live during exclusive apply");
            animator.set_looping(looping);
            animator.play();
        }
        PreparedAnimationCommand::Crossfade {
            entity,
            clip,
            duration_seconds,
            looping,
        } => {
            let animator = world
                .get_component_mut::<Animator>(entity)
                .expect("preflighted animator must remain live during exclusive apply");
            animator.set_looping(looping);
            animator.crossfade_to(clip, duration_seconds);
        }
        PreparedAnimationCommand::Stop { entity } => {
            world
                .get_component_mut::<Animator>(entity)
                .expect("preflighted animator must remain live during exclusive apply")
                .stop();
        }
        PreparedAnimationCommand::SetBool {
            entity,
            name,
            value,
        } => {
            world
                .get_component_mut::<AnimGraphPlayer>(entity)
                .expect("preflighted animation graph must remain live during exclusive apply")
                .set_bool_parameter(name, value)
                .expect("preflighted boolean parameter type must remain stable");
        }
        PreparedAnimationCommand::SetFloat {
            entity,
            name,
            value,
        } => {
            world
                .get_component_mut::<AnimGraphPlayer>(entity)
                .expect("preflighted animation graph must remain live during exclusive apply")
                .set_float_parameter(name, value)
                .expect("preflighted float parameter type and value must remain valid");
        }
        PreparedAnimationCommand::Trigger { entity, name } => {
            world
                .get_component_mut::<AnimGraphPlayer>(entity)
                .expect("preflighted animation graph must remain live during exclusive apply")
                .trigger_parameter(name)
                .expect("preflighted trigger parameter type must remain stable");
        }
    }
}

fn prepare_bool_parameter(
    player: &AnimGraphPlayer,
    index: usize,
    entity: Entity,
    name: &str,
    value: bool,
) -> Result<PreparedAnimationCommand, GameCommandError> {
    let name = validated_parameter_name(index, name)?;
    require_parameter_kind(player, index, &name, AnimationParameterKind::Bool)?;
    Ok(PreparedAnimationCommand::SetBool {
        entity,
        name,
        value,
    })
}

fn prepare_float_parameter(
    player: &AnimGraphPlayer,
    index: usize,
    entity: Entity,
    name: &str,
    value: f32,
) -> Result<PreparedAnimationCommand, GameCommandError> {
    let name = validated_parameter_name(index, name)?;
    if !value.is_finite() {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: "animation float parameter value must be finite".to_owned(),
        });
    }
    require_parameter_kind(player, index, &name, AnimationParameterKind::Float)?;
    Ok(PreparedAnimationCommand::SetFloat {
        entity,
        name,
        value,
    })
}

fn prepare_trigger_parameter(
    player: &AnimGraphPlayer,
    index: usize,
    entity: Entity,
    name: &str,
) -> Result<PreparedAnimationCommand, GameCommandError> {
    let name = validated_parameter_name(index, name)?;
    require_parameter_kind(player, index, &name, AnimationParameterKind::Trigger)?;
    Ok(PreparedAnimationCommand::Trigger { entity, name })
}

fn validated_parameter_name(index: usize, name: &str) -> Result<String, GameCommandError> {
    let name = name.trim();
    if name.is_empty() {
        Err(GameCommandError::InvalidPayload {
            index,
            message: "field `name` must not be empty".to_owned(),
        })
    } else {
        Ok(name.to_owned())
    }
}

fn require_parameter_kind(
    player: &AnimGraphPlayer,
    index: usize,
    name: &str,
    requested: AnimationParameterKind,
) -> Result<(), GameCommandError> {
    if let Some(stored) = player.parameter_kind(name)
        && stored != requested
    {
        return Err(GameCommandError::InvalidPayload {
            index,
            message: format!("animation parameter `{name}` is {stored:?}, not {requested:?}"),
        });
    }
    Ok(())
}

fn require_graph_player(
    world: &World,
    index: usize,
    target: GameEntityHandle,
    entity: Entity,
) -> Result<&AnimGraphPlayer, GameCommandError> {
    world
        .get_component::<AnimGraphPlayer>(entity)
        .ok_or(GameCommandError::MissingAnimationGraph { index, target })
}

fn require_animator(
    world: &World,
    index: usize,
    target: GameEntityHandle,
    entity: Entity,
) -> Result<(), GameCommandError> {
    if world.get_component::<Animator>(entity).is_none() {
        Err(GameCommandError::MissingAnimator { index, target })
    } else {
        Ok(())
    }
}
