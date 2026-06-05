//! Shared wire contract for nexus-scout: the request/response DTOs and the
//! [`ErrorCode`] → HTTP-status / exit-code maps, depended on by both server and
//! client.
#![deny(missing_docs)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Machine-readable error code carried in the wire response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The sanitizer or request validation blocked the query.
    QueryRejected,
    /// The query exceeded the time budget.
    QueryTimeout,
    /// Neo4j reported a Cypher syntax error.
    QuerySyntaxError,
    /// The caller exceeded the request rate limit.
    RateLimited,
    /// An unexpected internal failure.
    InternalError,
}

impl ErrorCode {
    /// The stable `SCREAMING_SNAKE_CASE` wire string; the source of truth for
    /// `Serialize`, `Display`, and [`ErrorCode::from_wire`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::QueryRejected => "QUERY_REJECTED",
            Self::QueryTimeout => "QUERY_TIMEOUT",
            Self::QuerySyntaxError => "QUERY_SYNTAX_ERROR",
            Self::RateLimited => "RATE_LIMITED",
            Self::InternalError => "INTERNAL_ERROR",
        }
    }

    /// Parses a wire string back into a code, or `None` if unrecognized (so a
    /// client tolerates a newer server's code).
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "QUERY_REJECTED" => Self::QueryRejected,
            "QUERY_TIMEOUT" => Self::QueryTimeout,
            "QUERY_SYNTAX_ERROR" => Self::QuerySyntaxError,
            "RATE_LIMITED" => Self::RateLimited,
            "INTERNAL_ERROR" => Self::InternalError,
            _ => return None,
        })
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The HTTP status an [`ErrorCode`] maps to.
#[must_use]
pub fn http_status(code: ErrorCode) -> u16 {
    match code {
        ErrorCode::QueryRejected | ErrorCode::QuerySyntaxError => 400,
        ErrorCode::QueryTimeout => 504,
        ErrorCode::RateLimited => 429,
        ErrorCode::InternalError => 500,
    }
}

/// The `scout` client exit code for an [`ErrorCode`]: `0` ok, `1`
/// internal/transient, `2` rejected/bad-query, `3` timeout.
#[must_use]
pub fn exit_code(code: ErrorCode) -> u8 {
    match code {
        ErrorCode::QueryRejected | ErrorCode::QuerySyntaxError => 2,
        ErrorCode::QueryTimeout => 3,
        ErrorCode::RateLimited | ErrorCode::InternalError => 1,
    }
}

/// The body of `POST /v1/query`. Lenient: unknown fields are ignored so the API
/// can add optional fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    /// The Cypher query to run.
    pub cypher: String,
    /// Native query parameters (bound, never interpolated).
    #[serde(default)]
    pub params: Map<String, Value>,
    /// Optional row-limit override (capped server-side).
    #[serde(default)]
    pub limit: Option<u32>,
}

impl QueryRequest {
    /// Builds a request.
    #[must_use]
    pub fn new(cypher: impl Into<String>, params: Map<String, Value>, limit: Option<u32>) -> Self {
        Self {
            cypher: cypher.into(),
            params,
            limit,
        }
    }
}

/// A successful query response. `count` always equals `results.len()`.
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

/// An error response: a machine `error` code, a human `message`, and a stable,
/// actionable `hint`.
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

impl ErrorResponse {
    /// Builds an error response.
    #[must_use]
    pub fn new(error: ErrorCode, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            error,
            message: message.into(),
            hint: hint.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [ErrorCode; 5] = [
        ErrorCode::QueryRejected,
        ErrorCode::QueryTimeout,
        ErrorCode::QuerySyntaxError,
        ErrorCode::RateLimited,
        ErrorCode::InternalError,
    ];

    #[test]
    fn wire_string_roundtrips() {
        for code in ALL {
            assert_eq!(ErrorCode::from_wire(code.as_str()), Some(code));
        }
        assert_eq!(ErrorCode::from_wire("NOT_A_CODE"), None);
    }

    #[test]
    fn status_and_exit_maps_match_the_contract() {
        let cases = [
            (ErrorCode::QueryRejected, 400, 2),
            (ErrorCode::QuerySyntaxError, 400, 2),
            (ErrorCode::QueryTimeout, 504, 3),
            (ErrorCode::RateLimited, 429, 1),
            (ErrorCode::InternalError, 500, 1),
        ];
        for (code, status, exit) in cases {
            assert_eq!(http_status(code), status, "status for {code}");
            assert_eq!(exit_code(code), exit, "exit for {code}");
        }
    }

    #[test]
    fn error_code_serializes_to_the_wire_string() {
        let resp = ErrorResponse::new(ErrorCode::QueryRejected, "nope", "fix it");
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["error"], "QUERY_REJECTED");
    }

    #[test]
    fn query_request_ignores_unknown_fields_and_defaults() {
        let r: QueryRequest = serde_json::from_str(r#"{"cypher":"RETURN 1","future":true}"#).unwrap();
        assert_eq!(r.cypher, "RETURN 1");
        assert!(r.params.is_empty());
        assert_eq!(r.limit, None);
    }

    #[test]
    fn query_response_count_cannot_disagree_with_len() {
        let resp = QueryResponse::new(vec![Map::new(), Map::new()], true);
        assert_eq!(resp.count, 2);
        assert!(resp.truncated);
    }
}
