//! Parameter payload bounds.
//!
//! Query parameters are bound natively (never interpolated), so their values are
//! inert against injection. Their only risk is a denial-of-service payload, so
//! the gateway bounds the count, serialized size, and nesting depth before
//! binding. These checks live here (gateway-owned resource policy), not in the
//! sanitizer, which concerns itself only with the query string.

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
    let bytes = serde_json::to_vec(params).map_or(0, |v| v.len());
    if bytes > limits.max_param_bytes {
        return Err(Error::rejected_params("parameter payload too large"));
    }
    for value in params.values() {
        if depth(value) > limits.max_param_depth {
            return Err(Error::rejected_params("parameter nesting too deep"));
        }
    }
    Ok(())
}

fn depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        _ => 0,
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
}
