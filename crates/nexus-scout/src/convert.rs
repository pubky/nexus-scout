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

/// Stack-safety backstop for both conversion directions. On the inbound (param)
/// side, caller-facing nesting is already bounded earlier by `params::check_params`,
/// so this far-higher ceiling only guards a pathological internal caller. On the
/// outbound (result) side it bounds an attacker-influenced shape: a sanitizer-
/// accepted query can still return a deeply nested value, so the cap keeps
/// `bolt_to_json` from recursing to full depth and overflowing the stack.
const MAX_JSON_DEPTH: usize = 128;

// Reserved JSON keys that define the agent-facing wire shape of graph entities.
// Pinned by the `*_json_shape_is_stable` tests in this module.
const KEY_ID: &str = "_id";
const KEY_LABELS: &str = "_labels";
const KEY_TYPE: &str = "_type";
const KEY_START: &str = "_start";
const KEY_END: &str = "_end";
const KEY_UNCONVERTIBLE: &str = "_unconvertible";

/// Converts a Bolt value to a JSON value. Total and infallible.
#[must_use]
pub(crate) fn bolt_to_json(value: &BoltType) -> Value {
    bolt_to_json_depth(value, 0)
}

fn bolt_to_json_depth(value: &BoltType, depth: usize) -> Value {
    if depth >= MAX_JSON_DEPTH {
        // Stack-safety backstop, mirroring `json_to_bolt` on the inbound side: a
        // sanitizer-accepted but pathologically nested result (e.g. `RETURN
        // [[[…]]]`, within the length cap) is truncated with an observable marker
        // rather than recursed to full depth and risking a stack overflow.
        return unconvertible("max_depth_exceeded");
    }
    match value {
        BoltType::Null(_) => Value::Null,
        BoltType::Boolean(b) => Value::Bool(b.value),
        BoltType::Integer(i) => Value::Number(i.value.into()),
        BoltType::Float(f) => Number::from_f64(f.value).map_or(Value::Null, Value::Number),
        BoltType::String(s) => Value::String(s.value.clone()),
        BoltType::List(list) => bolt_list_to_json(list, depth),
        BoltType::Map(map) => Value::Object(bolt_map_to_json(map, depth)),
        BoltType::Bytes(b) => Value::String(base64::engine::general_purpose::STANDARD.encode(&b.value)),
        BoltType::Node(n) => {
            let mut obj = entity_obj(&n.properties, n.id.value, depth);
            obj.insert(KEY_LABELS.into(), bolt_list_to_json(&n.labels, depth));
            Value::Object(obj)
        }
        BoltType::Relation(r) => {
            let mut obj = entity_obj(&r.properties, r.id.value, depth);
            obj.insert(KEY_TYPE.into(), Value::String(r.typ.value.clone()));
            obj.insert(KEY_START.into(), Value::Number(r.start_node_id.value.into()));
            obj.insert(KEY_END.into(), Value::Number(r.end_node_id.value.into()));
            Value::Object(obj)
        }
        BoltType::UnboundedRelation(r) => {
            let mut obj = entity_obj(&r.properties, r.id.value, depth);
            obj.insert(KEY_TYPE.into(), Value::String(r.typ.value.clone()));
            Value::Object(obj)
        }
        BoltType::Path(p) => {
            let mut obj = Map::new();
            obj.insert("nodes".into(), bolt_list_to_json(&p.nodes, depth));
            obj.insert("relationships".into(), bolt_list_to_json(&p.rels, depth));
            Value::Object(obj)
        }
        // Temporal and spatial values are uncommon in the Pubky graph (which
        // stores strings, integers, and JSON-encoded strings). Rather than ship
        // a bespoke conversion per type, emit a single structured marker. These
        // arms are listed explicitly (no `_` wildcard) so a driver upgrade that
        // adds a Bolt variant fails to compile here and is reviewed deliberately
        // — that exhaustiveness is the point of the `neo4rs = "=0.8.0"` pin.
        BoltType::Point2D(_) | BoltType::Point3D(_) => unconvertible("point"),
        BoltType::Duration(_) => unconvertible("duration"),
        BoltType::Date(_) => unconvertible("date"),
        BoltType::Time(_) => unconvertible("time"),
        BoltType::LocalTime(_) => unconvertible("local_time"),
        BoltType::DateTime(_) => unconvertible("date_time"),
        BoltType::LocalDateTime(_) => unconvertible("local_date_time"),
        BoltType::DateTimeZoneId(_) => unconvertible("date_time_zone_id"),
    }
}

/// Assembles the shared graph-entity object: the entity's properties followed by
/// its reserved `_id` key (nodes add `_labels`; relationships add `_type` etc.).
fn entity_obj(properties: &BoltMap, id: i64, depth: usize) -> Map<String, Value> {
    let mut obj = bolt_map_to_json(properties, depth);
    obj.insert(KEY_ID.into(), Value::Number(id.into()));
    obj
}

fn bolt_list_to_json(list: &BoltList, depth: usize) -> Value {
    Value::Array(list.value.iter().map(|v| bolt_to_json_depth(v, depth + 1)).collect())
}

fn bolt_map_to_json(map: &BoltMap, depth: usize) -> Map<String, Value> {
    map.value
        .iter()
        .map(|(k, v)| (k.value.clone(), bolt_to_json_depth(v, depth + 1)))
        .collect()
}

fn unconvertible(tag: &str) -> Value {
    let mut obj = Map::new();
    obj.insert(KEY_UNCONVERTIBLE.into(), Value::String(tag.to_owned()));
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
        return Err(Error::internal(
            "parameter nesting exceeded the internal depth backstop",
        ));
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
    use neo4rs::{
        BoltBoolean, BoltFloat, BoltInteger, BoltNode, BoltNull, BoltRelation, BoltString, BoltUnboundedRelation,
    };
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

    #[test]
    fn deeply_nested_result_value_is_depth_capped_not_overflowing() {
        // A pathologically nested DB result (e.g. RETURN [[[...]]]) must be
        // truncated by the outbound backstop, not recursed to full depth.
        let mut v = BoltType::Integer(BoltInteger::new(1));
        for _ in 0..400 {
            v = BoltType::List(BoltList::from(vec![v]));
        }
        // Converts without a stack overflow; the over-deep tail is an observable
        // marker (the conversion stops recursing at MAX_JSON_DEPTH).
        let json = serde_json::to_string(&bolt_to_json(&v)).unwrap();
        assert!(json.contains("max_depth_exceeded"));
    }

    #[test]
    fn configurable_param_depth_stays_below_the_backstop() {
        // Anything that passes params::check_params (bounded by max_param_depth)
        // must never reach this conversion backstop. Pin that the default honors
        // the invariant, so a clean rejection never degrades to an INTERNAL_ERROR.
        assert!(crate::config::Limits::default().max_param_depth < MAX_JSON_DEPTH);
    }

    #[test]
    fn node_json_shape_is_stable() {
        let mut props = BoltMap::default();
        props.put("name".into(), BoltType::String(BoltString::from("Alice")));
        let node = BoltNode::new(
            BoltInteger::new(7),
            BoltList::from(vec![BoltType::String(BoltString::from("User"))]),
            props,
        );
        assert_eq!(
            bolt_to_json(&BoltType::Node(node)),
            json!({"name": "Alice", "_id": 7, "_labels": ["User"]})
        );
    }

    #[test]
    fn relation_json_shape_is_stable() {
        let mut props = BoltMap::default();
        props.put("since".into(), BoltType::Integer(BoltInteger::new(2020)));
        let rel = BoltRelation {
            id: BoltInteger::new(3),
            start_node_id: BoltInteger::new(1),
            end_node_id: BoltInteger::new(2),
            typ: BoltString::from("FOLLOWS"),
            properties: props,
        };
        assert_eq!(
            bolt_to_json(&BoltType::Relation(rel)),
            json!({"since": 2020, "_id": 3, "_type": "FOLLOWS", "_start": 1, "_end": 2})
        );
    }

    #[test]
    fn unbounded_relation_json_shape_is_stable() {
        let rel = BoltUnboundedRelation::new(BoltInteger::new(5), BoltString::from("TAGGED"), BoltMap::default());
        assert_eq!(
            bolt_to_json(&BoltType::UnboundedRelation(rel)),
            json!({"_id": 5, "_type": "TAGGED"})
        );
    }
}
