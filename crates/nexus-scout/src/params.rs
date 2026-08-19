//! Parameter payload bounds. Parameters are bound natively (never interpolated),
//! so their only risk is a denial-of-service payload; the gateway bounds count,
//! size, and nesting depth before binding.

use serde_json::{Map, Value};

use crate::config::Limits;
use crate::error::Error;

/// Validates a parameter map against the configured limits.
///
/// # Errors
///
/// Returns [`Error`] (a rejection) if the parameters exceed the count, byte, or
/// depth limits.
pub(crate) fn check_params(params: &Map<String, Value>, limits: &Limits) -> Result<(), Error> {
    if params.len() > limits.max_param_count {
        return Err(Error::rejected_params("too many parameters"));
    }
    if serialized_len(params) > limits.max_param_bytes {
        return Err(Error::rejected_params("parameter payload too large"));
    }
    for value in params.values() {
        if !within_depth(value, limits.max_param_depth) {
            return Err(Error::rejected_params("parameter nesting too deep"));
        }
    }
    Ok(())
}

/// The serialized JSON byte length of `value`, shared with the executor's byte cap.
/// A serialization failure returns `usize::MAX` so the value fails closed against
/// the bound, never under-counted as 0.
pub(crate) fn serialized_len<T: serde::Serialize>(value: &T) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |v| v.len())
}

/// Whether `value` nests no deeper than `limit` levels. Descends at most
/// `limit + 1` levels, so an over-deep payload is rejected without recursing to its
/// full depth.
fn within_depth(value: &Value, limit: usize) -> bool {
    match value {
        Value::Array(items) => limit >= 1 && items.iter().all(|v| within_depth(v, limit - 1)),
        Value::Object(map) => limit >= 1 && map.values().all(|v| within_depth(v, limit - 1)),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits() -> Limits {
        Limits::default()
    }

    #[test]
    fn ordinary_params_pass() {
        let mut m = Map::new();
        m.insert("id".into(), json!("pk:abc"));
        m.insert("since".into(), json!(1_709_251_200_000_i64));
        assert!(check_params(&m, &limits()).is_ok());
    }

    #[test]
    fn too_many_params_rejected() {
        let mut m = Map::new();
        for i in 0..100 {
            m.insert(format!("k{i}"), json!(i));
        }
        assert!(check_params(&m, &limits()).is_err());
    }

    #[test]
    fn too_deep_params_rejected() {
        let mut v = json!(1);
        for _ in 0..20 {
            v = json!([v]);
        }
        let mut m = Map::new();
        m.insert("deep".into(), v);
        assert!(check_params(&m, &limits()).is_err());
    }

    #[test]
    fn very_deeply_nested_param_is_rejected_within_bounded_recursion() {
        // Far past max_param_depth: must reject without recursing to the input's full depth.
        let mut v = json!(1);
        for _ in 0..500 {
            v = json!([v]);
        }
        let mut m = Map::new();
        m.insert("deep".into(), v);
        assert!(check_params(&m, &limits()).is_err());
    }
}
