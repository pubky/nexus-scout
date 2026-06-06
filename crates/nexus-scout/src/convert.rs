//! Conversions between `neo4rs` Bolt values and `serde_json`. [`bolt_to_json`] is
//! total over every [`neo4rs::BoltType`] variant of the pinned driver;
//! temporal/spatial variants fall back to a `{"_unconvertible": "<tag>"}` marker.
//! It is the only module coupled to the Bolt value model, so a driver change is
//! contained here.

use std::collections::HashMap;

use base64::Engine as _;
use neo4rs::{BoltList, BoltMap, BoltNull, BoltType};
use serde_json::{Map, Number, Value};

use crate::error::Error;

/// Stack-safety backstop for both conversion directions. The outbound side bounds
/// an attacker-influenced shape: a sanitizer-accepted query can still return a
/// deeply nested value, so the cap keeps `bolt_to_json` from overflowing the stack.
const MAX_JSON_DEPTH: usize = 128;

// Reserved JSON keys for the result-row wire shape. `value` is unprefixed; the
// rest are `_`-prefixed.
const KEY_UNCONVERTIBLE: &str = "_unconvertible";
/// Wraps a single-column row's bare value (see [`row_to_json`]).
const KEY_VALUE: &str = "value";
/// Marks a row whose Bolt→JSON conversion failed (see [`row_to_json`]).
const KEY_ROW_ERROR: &str = "_row_error";

/// Converts a Bolt value to a JSON value. Total and infallible.
#[must_use]
pub(crate) fn bolt_to_json(value: &BoltType) -> Value {
    bolt_to_json_depth(value, 0)
}

fn bolt_to_json_depth(value: &BoltType, depth: usize) -> Value {
    if depth >= MAX_JSON_DEPTH {
        return unconvertible("max_depth_exceeded");
    }
    match value {
        BoltType::Null(_) => Value::Null,
        BoltType::Boolean(b) => Value::Bool(b.value),
        BoltType::Integer(i) => Value::Number(i.value.into()),
        // JSON has no NaN/±Inf; emit the tagged marker, not bare `null`
        // (indistinguishable from a real null).
        BoltType::Float(f) => {
            Number::from_f64(f.value).map_or_else(|| unconvertible("non_finite_float"), Value::Number)
        }
        BoltType::String(s) => Value::String(s.value.clone()),
        BoltType::List(list) => bolt_list_to_json(list, depth),
        BoltType::Map(map) => Value::Object(bolt_map_to_json(map, depth)),
        BoltType::Bytes(b) => Value::String(base64::engine::general_purpose::STANDARD.encode(&b.value)),
        // Nodes and relationships serialize to their property map only. Internal
        // Neo4j identity fields (`_id`/`_labels`/`_type`/`_start`/`_end`) are
        // deliberately omitted: they are implementation internals (distinct from
        // the public `id` property), they are noise for callers, and emitting them
        // would let a synthetic key clobber a real property of the same name. Use
        // `labels(n)` / `type(r)` explicitly to retrieve those.
        BoltType::Node(n) => Value::Object(bolt_map_to_json(&n.properties, depth)),
        BoltType::Relation(r) => Value::Object(bolt_map_to_json(&r.properties, depth)),
        BoltType::UnboundedRelation(r) => Value::Object(bolt_map_to_json(&r.properties, depth)),
        BoltType::Path(p) => {
            let mut obj = Map::new();
            obj.insert("nodes".into(), bolt_list_to_json(&p.nodes, depth));
            obj.insert("relationships".into(), bolt_list_to_json(&p.rels, depth));
            Value::Object(obj)
        }
        // Temporal/spatial values get a single marker. Arms are explicit (no `_`) so
        // a driver upgrade adding a Bolt variant fails to compile here.
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

/// Converts a Neo4j result row into a JSON object keyed by column name, with keys
/// sorted for stable output (the driver yields columns in nondeterministic order).
pub(crate) fn row_to_json(row: &neo4rs::Row) -> Map<String, Value> {
    match row.to::<BoltType>() {
        Ok(BoltType::Map(map)) => {
            let mut pairs: Vec<(String, Value)> = map
                .value
                .iter()
                .map(|(k, v)| (k.value.clone(), bolt_to_json(v)))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs.into_iter().collect()
        }
        // A single-column row deserializes to the bare value; wrap it.
        Ok(other) => single(KEY_VALUE, bolt_to_json(&other)),
        // Surface an observable marker rather than a silent empty row, so dropped
        // data is never mistaken for an absent result.
        Err(e) => {
            tracing::warn!(error = %e, "row conversion failed; returning an error marker row");
            single(KEY_ROW_ERROR, Value::String("row conversion failed".to_owned()))
        }
    }
}

/// A one-key JSON object.
fn single(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value);
    m
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
    if depth >= MAX_JSON_DEPTH {
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
            } else if n.is_u64() {
                // A JSON integer above i64::MAX cannot be represented exactly as
                // Neo4j's i64; reject it rather than silently binding a lossy f64
                // that changes the value the query sees.
                return Err(Error::rejected_params(
                    "integer parameter exceeds Neo4j's 64-bit signed range",
                ));
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
    fn non_finite_float_becomes_an_unconvertible_marker() {
        let marker = json!({"_unconvertible": "non_finite_float"});
        assert_eq!(bolt_to_json(&BoltType::Float(BoltFloat::new(f64::NAN))), marker);
        assert_eq!(bolt_to_json(&BoltType::Float(BoltFloat::new(f64::INFINITY))), marker);
        // A finite float is still a plain JSON number.
        assert_eq!(bolt_to_json(&BoltType::Float(BoltFloat::new(1.5))), json!(1.5));
    }

    #[test]
    fn json_params_convert_to_bolt() {
        assert!(matches!(json_to_bolt(&json!(7)).unwrap(), BoltType::Integer(_)));
        assert!(matches!(json_to_bolt(&json!("x")).unwrap(), BoltType::String(_)));
        assert!(matches!(json_to_bolt(&json!([1, 2])).unwrap(), BoltType::List(_)));
        assert!(matches!(json_to_bolt(&json!({"a": 1})).unwrap(), BoltType::Map(_)));
    }

    #[test]
    fn oversized_unsigned_integer_param_is_rejected_not_truncated() {
        let too_big: u64 = (i64::MAX as u64) + 1;
        assert!(json_to_bolt(&json!(too_big)).is_err());
        // The largest in-range integer still converts cleanly.
        assert!(matches!(json_to_bolt(&json!(i64::MAX)).unwrap(), BoltType::Integer(_)));
    }

    #[test]
    fn single_column_and_error_row_keys_are_stable() {
        assert_eq!(Value::Object(single(KEY_VALUE, json!(7))), json!({"value": 7}));
        assert_eq!(
            Value::Object(single(KEY_ROW_ERROR, json!("boom"))),
            json!({"_row_error": "boom"})
        );
    }

    #[test]
    fn pathologically_nested_param_hits_the_stack_backstop() {
        // Asserts the internal backstop trips deeper than params::check_params allows.
        let mut v = json!(1);
        for _ in 0..200 {
            v = json!([v]);
        }
        assert!(json_to_bolt(&v).is_err());
    }

    #[test]
    fn deeply_nested_result_value_is_depth_capped_not_overflowing() {
        let mut v = BoltType::Integer(BoltInteger::new(1));
        for _ in 0..400 {
            v = BoltType::List(BoltList::from(vec![v]));
        }
        let json = serde_json::to_string(&bolt_to_json(&v)).unwrap();
        assert!(json.contains("max_depth_exceeded"));
    }

    #[test]
    fn configurable_param_depth_stays_below_the_backstop() {
        // The default param depth must stay below this backstop so a clean rejection
        // never degrades to INTERNAL_ERROR.
        assert!(crate::config::Limits::default().max_param_depth < MAX_JSON_DEPTH);
    }

    #[test]
    fn node_is_properties_only() {
        // A whole-node return yields just its properties: no internal `_id`/`_labels`.
        let mut props = BoltMap::default();
        props.put("name".into(), BoltType::String(BoltString::from("Alice")));
        let node = BoltNode::new(
            BoltInteger::new(7),
            BoltList::from(vec![BoltType::String(BoltString::from("User"))]),
            props,
        );
        assert_eq!(bolt_to_json(&BoltType::Node(node)), json!({"name": "Alice"}));
    }

    #[test]
    fn relation_is_properties_only() {
        let mut props = BoltMap::default();
        props.put("since".into(), BoltType::Integer(BoltInteger::new(2020)));
        let rel = BoltRelation {
            id: BoltInteger::new(3),
            start_node_id: BoltInteger::new(1),
            end_node_id: BoltInteger::new(2),
            typ: BoltString::from("FOLLOWS"),
            properties: props,
        };
        // No internal `_id`/`_type`/`_start`/`_end`.
        assert_eq!(bolt_to_json(&BoltType::Relation(rel)), json!({"since": 2020}));
    }

    #[test]
    fn unbounded_relation_is_properties_only() {
        let rel = BoltUnboundedRelation::new(BoltInteger::new(5), BoltString::from("TAGGED"), BoltMap::default());
        assert_eq!(bolt_to_json(&BoltType::UnboundedRelation(rel)), json!({}));
    }

    #[test]
    fn a_property_named_id_is_not_clobbered() {
        // Regression: previously a synthetic `_id` would overwrite a real `_id`
        // property. With properties-only, the real property is preserved verbatim.
        let mut props = BoltMap::default();
        props.put("_id".into(), BoltType::String(BoltString::from("real-value")));
        let node = BoltNode::new(BoltInteger::new(7), BoltList::from(vec![]), props);
        assert_eq!(bolt_to_json(&BoltType::Node(node)), json!({"_id": "real-value"}));
    }
}
