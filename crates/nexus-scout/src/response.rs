//! Serialize-only wire types matching the spec's response contract (§5.1).
//!
//! These types are produced by the gateway and serialized to JSON; they are
//! never deserialized in production, so they carry only `Serialize`.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::ErrorCode;

/// A successful `query_cypher` response.
///
/// Each row in `results` is a JSON object keyed by the query's `RETURN` columns.
/// `count` always equals `results.len()` (a convenience, not a pre-truncation
/// total); `truncated` is `true` when the row or byte limit capped the result.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct QueryResponse {
    pub results: Vec<Map<String, Value>>,
    pub count: usize,
    pub truncated: bool,
}

impl QueryResponse {
    /// Builds a response, deriving `count` from the rows so it cannot disagree.
    #[must_use]
    pub fn new(results: Vec<Map<String, Value>>, truncated: bool) -> Self {
        let count = results.len();
        Self {
            results,
            count,
            truncated,
        }
    }
}

/// An error response: a machine-readable `error` code, a human `message`, and a
/// stable, actionable `hint`.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ErrorResponse {
    pub error: ErrorCode,
    pub message: String,
    pub hint: String,
}

/// The serialized envelope for either outcome of a query.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Response {
    Ok(QueryResponse),
    Err(ErrorResponse),
}
