//! Generic typed value for authoring properties.
//!
//! [`Value`] represents any authoring property. [`Value::EntityRef`] and
//! [`Value::AssetRef`] carry stable identifiers and serialize as explicit
//! tagged objects. See `AI_FRIENDLY_AUTHORING_SPEC.md` §7.3 and §7.4.

use crate::id::{AssetId, EntityId, StableId};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;
use std::fmt;

/// A typed value representing any authoring property.
///
/// Scalar variants (`Bool`, `I64`, `U64`, `F64`, `String`) serialize as their
/// natural JSON equivalents. `Array` and `Object` serialize recursively.
///
/// `EntityRef` and `AssetRef` MUST use the explicit tagged JSON form defined
/// in `AI_FRIENDLY_AUTHORING_SPEC.md` §7.4:
///
/// ```json
/// { "$type": "entity_ref", "id": "entity_01JZ..." }
/// { "$type": "asset_ref",  "id": "asset_01JZ..."  }
/// ```
///
/// Plain strings are never treated as entity or asset references.
///
/// # Numeric precision
///
/// `I64` and `U64` have overlapping ranges. When deserializing from JSON,
/// non-negative integers that fit in `i64` are stored as `I64`. Only values
/// exceeding `i64::MAX` are stored as `U64`. Schema-guided validation is
/// required to distinguish the two for values in the overlapping range.
/// This policy is subject to Open Decision #3 in the specification.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// The absence of a value.
    Null,
    /// A boolean value.
    Bool(bool),
    /// A signed 64-bit integer.
    I64(i64),
    /// An unsigned 64-bit integer for values exceeding `i64::MAX`.
    U64(u64),
    /// A 64-bit floating-point number.
    F64(f64),
    /// A UTF-8 string that is not an entity or asset reference.
    String(String),
    /// An ordered list of values.
    Array(Vec<Value>),
    /// A string-keyed map of values, sorted for deterministic serialization.
    Object(BTreeMap<String, Value>),
    /// A reference to an authoring entity by its stable identifier.
    EntityRef(EntityId),
    /// A reference to an authoring asset by its stable identifier.
    AssetRef(AssetId),
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => s.serialize_unit(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::I64(n) => s.serialize_i64(*n),
            Value::U64(n) => s.serialize_u64(*n),
            Value::F64(n) => s.serialize_f64(*n),
            Value::String(st) => s.serialize_str(st),
            Value::Array(arr) => arr.serialize(s),
            Value::Object(map) => map.serialize(s),
            // Tagged entity reference: {"$type":"entity_ref","id":"<id>"}
            Value::EntityRef(id) => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("$type", "entity_ref")?;
                map.serialize_entry("id", id.as_str())?;
                map.end()
            }
            // Tagged asset reference: {"$type":"asset_ref","id":"<id>"}
            Value::AssetRef(id) => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("$type", "asset_ref")?;
                map.serialize_entry("id", id.as_str())?;
                map.end()
            }
        }
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("any authoring value (null, bool, number, string, array, or object)")
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::I64(v))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Value, E> {
        // Normalize non-negative integers that fit in i64 to I64 so that
        // common game values (health, indices, counts) survive JSON roundtrip
        // as Value::I64. Only values exceeding i64::MAX are kept as U64.
        if let Ok(signed) = i64::try_from(v) {
            Ok(Value::I64(signed))
        } else {
            Ok(Value::U64(v))
        }
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(Value::F64(v))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(ValueVisitor)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(v) = seq.next_element::<Value>()? {
            values.push(v);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object: BTreeMap<String, Value> = BTreeMap::new();
        while let Some((k, v)) = map.next_entry::<String, Value>()? {
            object.insert(k, v);
        }

        // Detect the tagged reference form:
        // {"$type": "entity_ref", "id": "<stable-id>"} or
        // {"$type": "asset_ref",  "id": "<stable-id>"}
        if object.len() == 2
            && let (Some(Value::String(type_str)), Some(Value::String(id_str))) =
                (object.get("$type"), object.get("id"))
        {
            let type_str = type_str.clone();
            let id_str = id_str.clone();
            match type_str.as_str() {
                "entity_ref" => {
                    return EntityId::from_stable_id(StableId::new(id_str))
                        .map(Value::EntityRef)
                        .map_err(de::Error::custom);
                }
                "asset_ref" => {
                    return AssetId::from_stable_id(StableId::new(id_str))
                        .map(Value::AssetRef)
                        .map_err(de::Error::custom);
                }
                _ => {}
            }
        }

        Ok(Value::Object(object))
    }
}

impl<'de> serde::Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(ValueVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::{AssetId, EntityId};

    #[test]
    fn null_serializes_and_deserializes() {
        let v = Value::Null;
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "null");
        let loaded: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, Value::Null);
    }

    #[test]
    fn bool_survives_roundtrip() {
        for b in [true, false] {
            let v = Value::Bool(b);
            let json = serde_json::to_string(&v).unwrap();
            let loaded: Value = serde_json::from_str(&json).unwrap();
            assert_eq!(loaded, v);
        }
    }

    #[test]
    fn negative_i64_survives_roundtrip() {
        let v = Value::I64(-42);
        let json = serde_json::to_string(&v).unwrap();
        let loaded: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, Value::I64(-42));
    }

    #[test]
    fn non_negative_i64_survives_roundtrip() {
        let v = Value::I64(100);
        let json = serde_json::to_string(&v).unwrap();
        let loaded: Value = serde_json::from_str(&json).unwrap();
        // Non-negative i64 is normalized through u64 deserialization but
        // then converted back to i64 because it fits.
        assert_eq!(loaded, Value::I64(100));
    }

    #[test]
    fn large_u64_beyond_i64_max_survives_roundtrip() {
        let large = u64::MAX;
        let v = Value::U64(large);
        let json = serde_json::to_string(&v).unwrap();
        let loaded: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, Value::U64(large));
    }

    #[test]
    fn string_survives_roundtrip() {
        let v = Value::String("hello world".into());
        let json = serde_json::to_string(&v).unwrap();
        let loaded: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, v);
    }

    #[test]
    fn entity_ref_serializes_with_tagged_format() {
        let entity_id = EntityId::generate();
        let v = Value::EntityRef(entity_id.clone());
        let json = serde_json::to_string(&v).unwrap();

        // Must contain both required keys.
        assert!(
            json.contains(r#""$type":"entity_ref""#),
            "missing $type: {json}"
        );
        assert!(json.contains(entity_id.as_str()), "missing id: {json}");
    }

    #[test]
    fn asset_ref_serializes_with_tagged_format() {
        let asset_id = AssetId::generate();
        let v = Value::AssetRef(asset_id.clone());
        let json = serde_json::to_string(&v).unwrap();

        assert!(
            json.contains(r#""$type":"asset_ref""#),
            "missing $type: {json}"
        );
        assert!(json.contains(asset_id.as_str()), "missing id: {json}");
    }

    #[test]
    fn entity_ref_survives_roundtrip() {
        let entity_id = EntityId::generate();
        let v = Value::EntityRef(entity_id);
        let json = serde_json::to_string(&v).unwrap();
        let loaded: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, v);
        // The loaded value must still be an EntityRef, not a plain String or Object.
        assert!(matches!(loaded, Value::EntityRef(_)));
    }

    #[test]
    fn asset_ref_survives_roundtrip() {
        let asset_id = AssetId::generate();
        let v = Value::AssetRef(asset_id);
        let json = serde_json::to_string(&v).unwrap();
        let loaded: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, v);
        assert!(matches!(loaded, Value::AssetRef(_)));
    }

    #[test]
    fn plain_string_is_not_treated_as_entity_ref() {
        // A JSON string that happens to look like a stable ID must deserialize
        // as Value::String, not as an entity reference.
        let json = r#""entity_01JZFAKEULID0000000000000""#;
        let loaded: Value = serde_json::from_str(json).unwrap();
        assert!(
            matches!(loaded, Value::String(_)),
            "plain string was incorrectly parsed as a reference: {loaded:?}"
        );
    }

    #[test]
    fn object_with_extra_keys_is_not_a_reference() {
        // An object with $type, id, AND an extra key is a plain Object.
        let json = r#"{"$type":"entity_ref","id":"entity_01JZFAKE00000000000000000","extra":"x"}"#;
        let loaded: Value = serde_json::from_str(json).unwrap();
        assert!(
            matches!(loaded, Value::Object(_)),
            "three-key object was incorrectly parsed as a reference: {loaded:?}"
        );
    }

    #[test]
    fn object_with_unknown_type_tag_is_plain_object() {
        let json = r#"{"$type":"future_ref","id":"something_01JZFAKE"}"#;
        let loaded: Value = serde_json::from_str(json).unwrap();
        assert!(
            matches!(loaded, Value::Object(_)),
            "unknown tagged object was not treated as plain object: {loaded:?}"
        );
    }

    #[test]
    fn nested_entity_ref_in_array_survives_roundtrip() {
        let entity_id = EntityId::generate();
        let v = Value::Array(vec![
            Value::I64(1),
            Value::EntityRef(entity_id.clone()),
            Value::Bool(true),
        ]);
        let json = serde_json::to_string(&v).unwrap();
        let loaded: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, v);
    }

    #[test]
    fn nested_entity_ref_in_object_survives_roundtrip() {
        let entity_id = EntityId::generate();
        let mut obj = BTreeMap::new();
        obj.insert("target".into(), Value::EntityRef(entity_id.clone()));
        obj.insert("priority".into(), Value::I64(10));
        let v = Value::Object(obj);
        let json = serde_json::to_string(&v).unwrap();
        let loaded: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, v);
    }

    #[test]
    fn object_keys_are_sorted_in_serialized_output() {
        let mut obj = BTreeMap::new();
        obj.insert("z_last".into(), Value::I64(3));
        obj.insert("a_first".into(), Value::I64(1));
        obj.insert("m_middle".into(), Value::I64(2));
        let v = Value::Object(obj);
        let json = serde_json::to_string(&v).unwrap();
        let a_pos = json.find("a_first").unwrap();
        let m_pos = json.find("m_middle").unwrap();
        let z_pos = json.find("z_last").unwrap();
        assert!(a_pos < m_pos && m_pos < z_pos, "keys not sorted: {json}");
    }
}
