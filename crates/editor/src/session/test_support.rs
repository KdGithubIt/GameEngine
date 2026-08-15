//! Helpers shared by the session submodule test modules.

use engine_authoring::Value;
use std::collections::BTreeMap;

/// Builds an `engine.transform` component value at the given position.
pub(super) fn transform_value(x: f64, y: f64, z: f64) -> Value {
    Value::Object(BTreeMap::from([
        ("x".to_owned(), Value::F64(x)),
        ("y".to_owned(), Value::F64(y)),
        ("z".to_owned(), Value::F64(z)),
    ]))
}
