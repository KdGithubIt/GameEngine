//! Transform gizmo state and drag-delta computation for Phase 29.

use engine_authoring::id::ComponentTypeId;
use engine_authoring::Value;

/// Describes which part of the transform the user is dragging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GizmoAxis {
    /// World-space X axis.
    X,
    /// World-space Y axis.
    Y,
    /// World-space Z axis.
    Z,
}

/// Returns a copy of an entity's transform component with one field nudged.
///
/// `entity` is the source component value (must be an `Object`).
/// `axis` selects the field to modify (`x`, `y`, or `z`).
/// `delta` is the amount to add.
pub fn apply_translate_delta(
    current: &Value,
    axis: GizmoAxis,
    delta: f32,
) -> Option<(Vec<engine_authoring::PropertyPathSegment>, Value)> {
    use engine_authoring::PropertyPathSegment;
    let key = match axis {
        GizmoAxis::X => "x",
        GizmoAxis::Y => "y",
        GizmoAxis::Z => "z",
    };
    let old = if let Value::Object(fields) = current {
        match fields.get(key) {
            Some(Value::F64(f)) => *f,
            Some(Value::I64(i)) => *i as f64,
            _ => 0.0,
        }
    } else {
        return None;
    };
    let new_val = Value::F64(old + delta as f64);
    let path = vec![PropertyPathSegment::Field { name: key.into() }];
    Some((path, new_val))
}

/// Produces an Euler-degree property edit for one rotation axis.
pub fn apply_rotate_delta(
    current: &Value,
    axis: GizmoAxis,
    delta_degrees: f32,
) -> Option<(Vec<engine_authoring::PropertyPathSegment>, Value)> {
    let key = match axis {
        GizmoAxis::X => "rotation_x_degrees",
        GizmoAxis::Y => "rotation_y_degrees",
        GizmoAxis::Z => "rotation_z_degrees",
    };
    apply_numeric_delta(current, key, 0.0, delta_degrees as f64)
}

/// Produces a positive, non-singular scale property edit for one axis.
pub fn apply_scale_delta(
    current: &Value,
    axis: GizmoAxis,
    delta: f32,
) -> Option<(Vec<engine_authoring::PropertyPathSegment>, Value)> {
    let key = match axis {
        GizmoAxis::X => "scale_x",
        GizmoAxis::Y => "scale_y",
        GizmoAxis::Z => "scale_z",
    };
    let (path, value) = apply_numeric_delta(current, key, 1.0, delta as f64)?;
    let Value::F64(value) = value else {
        return None;
    };
    Some((path, Value::F64(value.max(0.001))))
}

fn apply_numeric_delta(
    current: &Value,
    key: &str,
    default: f64,
    delta: f64,
) -> Option<(Vec<engine_authoring::PropertyPathSegment>, Value)> {
    use engine_authoring::PropertyPathSegment;
    let fields = match current {
        Value::Object(fields) => fields,
        _ => return None,
    };
    let old = match fields.get(key) {
        Some(Value::F64(value)) => *value,
        Some(Value::I64(value)) => *value as f64,
        Some(Value::U64(value)) => *value as f64,
        None => default,
        Some(_) => return None,
    };
    Some((
        vec![PropertyPathSegment::Field { name: key.into() }],
        Value::F64(old + delta),
    ))
}

/// Returns the `ComponentTypeId` for the engine transform component.
pub fn transform_component_type() -> ComponentTypeId {
    ComponentTypeId::new("engine.transform")
}

/// Rounds an accumulated gizmo delta to the nearest multiple of `increment`.
///
/// A non-positive increment disables snapping and returns `delta` unchanged.
pub fn snap_delta(delta: f32, increment: f32) -> f32 {
    if increment <= 0.0 {
        return delta;
    }
    (delta / increment).round() * increment
}

/// Returns a transform value with a world-space translation delta added to
/// its `x`/`y`/`z` fields.
///
/// Local-space translate handles drag along an oblique world direction, so
/// one gesture legitimately changes all three position fields at once.
pub fn apply_translate_vector(current: &Value, delta: [f64; 3]) -> Option<Value> {
    let Value::Object(fields) = current else {
        return None;
    };
    let mut updated = fields.clone();
    for (key, amount) in [("x", delta[0]), ("y", delta[1]), ("z", delta[2])] {
        let old = match updated.get(key) {
            Some(Value::F64(value)) => *value,
            Some(Value::I64(value)) => *value as f64,
            Some(Value::U64(value)) => *value as f64,
            None => 0.0,
            Some(_) => return None,
        };
        updated.insert(key.to_owned(), Value::F64(old + amount));
    }
    Some(Value::Object(updated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn rotate_and_scale_edits_supply_v1_compatible_defaults() {
        let transform = Value::Object(BTreeMap::from([
            ("x".into(), Value::F64(0.0)),
            ("y".into(), Value::F64(0.0)),
            ("z".into(), Value::F64(0.0)),
        ]));
        let (_, rotation) = apply_rotate_delta(&transform, GizmoAxis::Y, 45.0).unwrap();
        let (_, scale) = apply_scale_delta(&transform, GizmoAxis::Z, 0.5).unwrap();
        assert_eq!(rotation, Value::F64(45.0));
        assert_eq!(scale, Value::F64(1.5));
    }

    #[test]
    fn translate_vector_updates_all_three_axes_with_defaults() {
        let transform = Value::Object(BTreeMap::from([("x".into(), Value::F64(1.0))]));
        let updated = apply_translate_vector(&transform, [0.5, -1.0, 2.0]).unwrap();
        let Value::Object(fields) = updated else {
            panic!("translate vector must keep the object shape");
        };
        assert_eq!(fields.get("x"), Some(&Value::F64(1.5)));
        assert_eq!(fields.get("y"), Some(&Value::F64(-1.0)));
        assert_eq!(fields.get("z"), Some(&Value::F64(2.0)));
    }

    #[test]
    fn snap_delta_rounds_to_nearest_increment() {
        assert_eq!(snap_delta(1.4, 1.0), 1.0);
        assert_eq!(snap_delta(1.6, 1.0), 2.0);
        assert_eq!(snap_delta(-22.4, 15.0), -15.0);
        assert_eq!(snap_delta(0.04, 0.1), 0.0);
    }

    #[test]
    fn snap_delta_with_zero_increment_returns_input() {
        assert_eq!(snap_delta(1.234, 0.0), 1.234);
        assert_eq!(snap_delta(1.234, -1.0), 1.234);
    }

    #[test]
    fn scale_edit_never_produces_a_singular_axis() {
        let transform = Value::Object(BTreeMap::from([("scale_x".into(), Value::F64(0.25))]));
        let (_, scale) = apply_scale_delta(&transform, GizmoAxis::X, -5.0).unwrap();
        assert_eq!(scale, Value::F64(0.001));
    }
}
