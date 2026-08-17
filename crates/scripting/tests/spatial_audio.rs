use engine_authoring::Value;
use engine_scripting::game_io::{
    GameAudioRolloffMode, GameCommand, GameEntityHandle, GameSpatialAudioOptions,
};

#[test]
fn spatial_sound_effect_request_preserves_deferred_target_and_policy() {
    let target = GameEntityHandle { id: 17, generation: 4 };
    let command = GameCommand::play_spatial_sound_effect(
        target,
        "audio.effect.test",
        GameSpatialAudioOptions {
            volume: 0.75,
            spatial_blend: 0.8,
            min_distance: 0.0,
            max_distance: 12.0,
            rolloff: GameAudioRolloffMode::Inverse,
            looping: true,
        },
    );

    assert_eq!(command.target, Some(target));
    let Value::Object(fields) = command.payload else { panic!("audio payload must be an object"); };
    assert_eq!(fields.get("operation"), Some(&Value::String("play_spatial_se".to_owned())));
    assert_eq!(fields.get("rolloff"), Some(&Value::String("inverse".to_owned())));
    assert_eq!(fields.get("looping"), Some(&Value::Bool(true)));
}
