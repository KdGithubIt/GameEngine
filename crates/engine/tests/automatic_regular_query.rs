use engine::game_module::{ComponentSchema, ComponentTypeId, GameComponent, Value};
use engine::prelude::*;

macro_rules! test_component {
    ($name:ident, $id:literal) => {
        #[derive(Debug, Default)]
        struct $name;

        impl GameComponent for $name {
            const TYPE_ID: &'static str = $id;
            const DISPLAY_NAME: &'static str = stringify!($name);

            fn schema() -> ComponentSchema {
                ComponentSchema {
                    type_id: ComponentTypeId::new(Self::TYPE_ID),
                    display_name: Self::DISPLAY_NAME.to_owned(),
                    description: "Automatic query integration-test component.".to_owned(),
                    category: "Test".to_owned(),
                    version: 1,
                    fields: Vec::new(),
                    component_default: Some(Value::Object(Default::default())),
                }
            }

            fn from_authoring_value(value: &Value) -> Result<Self, String> {
                match value {
                    Value::Object(_) => Ok(Self),
                    _ => Err("test component value must be an object".to_owned()),
                }
            }

            fn to_authoring_value(&self) -> Value {
                Value::Object(Default::default())
            }
        }
    };
}

test_component!(MoveRule, "test.automatic.move_rule");
test_component!(Target, "test.automatic.target");
test_component!(Dead, "test.automatic.dead");
test_component!(Health, "test.automatic.health");
test_component!(Buff, "test.automatic.buff");

struct LegacyQuery;

impl QuerySpec for LegacyQuery {
    const ID: &'static str = "test.automatic.legacy";

    fn access() -> engine::game_io::GameQueryAccess {
        QueryAccessBuilder::new(Self::ID).read::<Target>().build()
    }
}

#[engine::game_system(id = "test.automatic_regular_query")]
fn automatic_regular_query(
    moving: Query<(&MoveRule, &mut Transform)>,
    targets: Query<(&Target, &Transform), Without<Dead>>,
    writers: Query<(&mut Health, Option<&mut Buff>, &mut Transform), With<MoveRule>>,
    legacy: Query<LegacyQuery>,
) -> Result<(), GameApiError> {
    for (rule, transform) in moving.iter() {
        let _ = rule;
        transform.translation.y += 1.0;
    }
    let _ = targets.iter().count();
    for (health, buff, transform) in writers.iter() {
        let _ = health;
        let _ = buff;
        transform.translation.x += 1.0;
    }
    let _ = legacy.rows().len();
    Ok(())
}

#[test]
fn automatic_query_access_uses_stable_per_parameter_ids() {
    let access = __iroha_access_automatic_regular_query();
    let query_ids = access
        .queries
        .iter()
        .map(|query| query.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        query_ids,
        vec![
            "test.automatic_regular_query.query.0",
            "test.automatic_regular_query.query.1",
            "test.automatic_regular_query.query.2",
            LegacyQuery::ID,
        ]
    );
}
