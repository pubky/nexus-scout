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
    /// The result rows; each is a JSON object keyed by the query's `RETURN` columns.
    pub results: Vec<Map<String, Value>>,
    /// The number of rows in `results` (equal to `results.len()`).
    pub count: usize,
    /// `true` if the row or byte cap truncated the result.
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
    /// The machine-readable error code.
    pub error: ErrorCode,
    /// A human-readable error message.
    pub message: String,
    /// A stable, actionable hint for fixing the request.
    pub hint: String,
}
