//! Conversions between `neo4rs` Bolt values and `serde_json`.
//!
//! [`bolt_to_json`] is total over every [`neo4rs::BoltType`] variant of the
//! pinned driver: scalars map directly, containers recurse, graph entities
//! become tagged objects, and the temporal/spatial variants fall back to a
//! single structured `{"_unconvertible": "<tag>"}` shape rather than emitting
//! `Debug` garbage. It is the only module coupled to the Bolt value model, so a
//! driver change is contained here.
//!
//! This module deliberately does **not** reuse `nexus-common`'s
//! `bolt_to_cypher_literal`, which renders Cypher literals via string
//! interpolation (an injection-prone path); we emit JSON values, not Cypher.

use std::collections::HashMap;

use base64::Engine as _;
use neo4rs::{BoltList, BoltMap, BoltNull, BoltType};
use serde_json::{Map, Number, Value};

use crate::error::Error;

/// Stack-safety backstop for the conversion recursion. Caller-facing nesting is
/// already bounded by `params::check_params` (which rejects with a clear reason
/// before `execute` runs); this far-higher ceiling exists only so a pathological
/// internal caller cannot overflow the stack, and reaching it is a genuine bug.
const MAX_JSON_DEPTH: usize = 128;

/// Converts a Bolt value to a JSON value. Total and infallible.
#[must_use]
pub(crate) fn bolt_to_json(value: &BoltType) -> Value {
    match value {
        BoltType::Null(_) => Value::Null,
        BoltType::Boolean(b) => Value::Bool(b.value),
        BoltType::Integer(i) => Value::Number(i.value.into()),
        BoltType::Float(f) => Number::from_f64(f.value).map_or(Value::Null, Value::Number),
        BoltType::String(s) => Value::String(s.value.clone()),
        BoltType::List(list) => Value::Array(list.value.iter().map(bolt_to_json).collect()),
        BoltType::Map(map) => Value::Object(bolt_map_to_json(map)),
        BoltType::Bytes(b) => Value::String(base64::engine::general_purpose::STANDARD.encode(&b.value)),
        BoltType::Node(n) => {
            let mut obj = bolt_map_to_json(&n.properties);
            obj.insert("_id".into(), Value::Number(n.id.value.into()));
            obj.insert("_labels".into(), bolt_list_to_json(&n.labels));
            Value::Object(obj)
        }
        BoltType::Relation(r) => {
            let mut obj = bolt_map_to_json(&r.properties);
            obj.insert("_id".into(), Value::Number(r.id.value.into()));
            obj.insert("_type".into(), Value::String(r.typ.value.clone()));
            obj.insert("_start".into(), Value::Number(r.start_node_id.value.into()));
            obj.insert("_end".into(), Value::Number(r.end_node_id.value.into()));
            Value::Object(obj)
        }
        BoltType::UnboundedRelation(r) => {
            let mut obj = bolt_map_to_json(&r.properties);
            obj.insert("_id".into(), Value::Number(r.id.value.into()));
            obj.insert("_type".into(), Value::String(r.typ.value.clone()));
            Value::Object(obj)
        }
        BoltType::Path(p) => {
            let mut obj = Map::new();
            obj.insert("nodes".into(), bolt_list_to_json(&p.nodes));
            obj.insert("relationships".into(), bolt_list_to_json(&p.rels));
            Value::Object(obj)
        }
        // Temporal and spatial values are uncommon in the Pubky graph (which
        // stores strings, integers, and JSON-encoded strings). Rather than ship
        // a bespoke conversion per type, emit a single structured marker.
        other => unconvertible(other),
    }
}

fn bolt_list_to_json(list: &BoltList) -> Value {
    Value::Array(list.value.iter().map(bolt_to_json).collect())
}

fn bolt_map_to_json(map: &BoltMap) -> Map<String, Value> {
    let mut obj = Map::new();
    for (k, v) in &map.value {
        obj.insert(k.value.clone(), bolt_to_json(v));
    }
    obj
}

fn unconvertible(value: &BoltType) -> Value {
    let tag = match value {
        BoltType::Point2D(_) | BoltType::Point3D(_) => "point",
        BoltType::Duration(_) => "duration",
        BoltType::Date(_) => "date",
        BoltType::Time(_) => "time",
        BoltType::LocalTime(_) => "local_time",
        BoltType::DateTime(_) => "date_time",
        BoltType::LocalDateTime(_) => "local_date_time",
        BoltType::DateTimeZoneId(_) => "date_time_zone_id",
        _ => "unknown",
    };
    let mut obj = Map::new();
    obj.insert("_unconvertible".into(), Value::String(tag.to_owned()));
    Value::Object(obj)
}

/// Converts a JSON value to a Bolt value for parameter binding.
///
/// # Errors
///
/// Returns [`Error`] if the value nests deeper than [`MAX_JSON_DEPTH`].
pub(crate) fn json_to_bolt(value: &Value) -> Result<BoltType, Error> {
    json_to_bolt_depth(value, 0)
}

fn json_to_bolt_depth(value: &Value, depth: usize) -> Result<BoltType, Error> {
    if depth > MAX_JSON_DEPTH {
        return Err(Error::internal("parameter nesting too deep"));
    }
    let bolt = match value {
        Value::Null => BoltType::Null(BoltNull),
        Value::Bool(b) => BoltType::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                BoltType::from(i)
            } else if let Some(f) = n.as_f64() {
                BoltType::from(f)
            } else {
                return Err(Error::internal("unrepresentable JSON number"));
            }
        }
        Value::String(s) => BoltType::from(s.clone()),
        Value::Array(items) => {
            let mut list = Vec::with_capacity(items.len());
            for item in items {
                list.push(json_to_bolt_depth(item, depth + 1)?);
            }
            BoltType::List(BoltList::from(list))
        }
        Value::Object(map) => {
            let mut bolt = HashMap::with_capacity(map.len());
            for (k, v) in map {
                bolt.insert(k.as_str().into(), json_to_bolt_depth(v, depth + 1)?);
            }
            BoltType::Map(BoltMap { value: bolt })
        }
    };
    Ok(bolt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use neo4rs::{BoltBoolean, BoltFloat, BoltInteger, BoltNull, BoltString};
    use serde_json::json;

    #[test]
    fn scalars_round_trip_to_json() {
        assert_eq!(bolt_to_json(&BoltType::Null(BoltNull)), json!(null));
        assert_eq!(bolt_to_json(&BoltType::Boolean(BoltBoolean::new(true))), json!(true));
        assert_eq!(bolt_to_json(&BoltType::Integer(BoltInteger::new(42))), json!(42));
        assert_eq!(bolt_to_json(&BoltType::Float(BoltFloat::new(1.5))), json!(1.5));
        assert_eq!(bolt_to_json(&BoltType::String(BoltString::from("hi"))), json!("hi"));
    }

    #[test]
    fn list_and_map_recurse() {
        let list = BoltList::from(vec![
            BoltType::Integer(BoltInteger::new(1)),
            BoltType::String(BoltString::from("two")),
        ]);
        assert_eq!(bolt_to_json(&BoltType::List(list)), json!([1, "two"]));

        let mut map = BoltMap::default();
        map.put("k".into(), BoltType::Integer(BoltInteger::new(9)));
        assert_eq!(bolt_to_json(&BoltType::Map(map)), json!({"k": 9}));
    }

    #[test]
    fn nan_float_becomes_null() {
        assert_eq!(bolt_to_json(&BoltType::Float(BoltFloat::new(f64::NAN))), json!(null));
    }

    #[test]
    fn json_params_convert_to_bolt() {
        assert!(matches!(json_to_bolt(&json!(7)).unwrap(), BoltType::Integer(_)));
        assert!(matches!(json_to_bolt(&json!("x")).unwrap(), BoltType::String(_)));
        assert!(matches!(json_to_bolt(&json!([1, 2])).unwrap(), BoltType::List(_)));
        assert!(matches!(json_to_bolt(&json!({"a": 1})).unwrap(), BoltType::Map(_)));
    }

    #[test]
    fn pathologically_nested_param_hits_the_stack_backstop() {
        // Caller-facing depth is bounded earlier by params::check_params; this
        // asserts the internal stack-safety backstop still trips far deeper.
        let mut v = json!(1);
        for _ in 0..200 {
            v = json!([v]);
        }
        assert!(json_to_bolt(&v).is_err());
    }
}
