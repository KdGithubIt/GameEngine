//! Compile-time and value-shape coverage for project runtime resources.

use engine::game_module::{GameResource as _, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, engine::GameResource)]
#[game_resource(id = "game.mission", display_name = "Mission State")]
struct MissionState {
    /// Current mission phase shared by multiple project systems.
    phase: i64,
    /// Whether the boss encounter is currently active.
    boss_active: bool,
}

#[test]
fn derive_exports_schema_default_and_typed_roundtrip() {
    let schema = MissionState::schema();
    assert_eq!(schema.id, "game.mission");
    assert_eq!(schema.display_name, "Mission State");
    assert_eq!(schema.fields.len(), 2);
    assert_eq!(
        schema.default_value,
        Value::Object(BTreeMap::from([
            ("boss_active".to_owned(), Value::Bool(false)),
            ("phase".to_owned(), Value::I64(0)),
        ]))
    );

    let state = MissionState {
        phase: 3,
        boss_active: true,
    };
    assert_eq!(MissionState::from_value(&state.to_value()).unwrap(), state);
}
