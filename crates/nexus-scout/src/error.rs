//! The gateway error type and its wire representation. [`Error`] is a canonical
//! struct with a private [`Kind`]; callers classify via `is_*` helpers and
//! [`Error::code`], so new failure modes never break callers.

use cypher_guard::SanitizeError;

use crate::response::ErrorResponse;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The machine-readable wire error code.
pub use nexus_scout_types::ErrorCode;

#[derive(Debug)]
enum Kind {
    Rejected(SanitizeError),
    RejectedParams(&'static str),
    BadRequest(String),
    BadConfig { key: &'static str },
    Timeout { ms: u64 },
    RateLimited,
    Syntax,
    Internal,
}

/// The gateway's canonical error type.
#[derive(Debug)]
pub struct Error {
    kind: Kind,
    source: Option<BoxError>,
}

impl Error {
    fn new(kind: Kind, source: Option<BoxError>) -> Self {
        Self { kind, source }
    }

    /// Builds a timeout error for a query that exceeded `ms` milliseconds.
    #[must_use]
    pub(crate) fn timeout(ms: u64) -> Self {
        Self::new(Kind::Timeout { ms }, None)
    }

    /// Builds a rejection for a parameter payload that breached a resource bound.
    #[must_use]
    pub(crate) fn rejected_params(reason: &'static str) -> Self {
        Self::new(Kind::RejectedParams(reason), None)
    }

    /// Builds a rejection for malformed caller input (e.g. a bad `--param`).
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(Kind::BadRequest(message.into()), None)
    }

    /// Builds an internal error wrapping an arbitrary source.
    #[must_use]
    pub(crate) fn internal(source: impl Into<BoxError>) -> Self {
        Self::new(Kind::Internal, Some(source.into()))
    }

    /// Builds a rate-limit rejection (HTTP 429), produced when the HTTP gateway
    /// sheds load past its admission limits.
    #[must_use]
    pub(crate) fn rate_limited() -> Self {
        Self::new(Kind::RateLimited, None)
    }

    /// Builds an error for an environment variable whose value could not be
    /// parsed. Unlike a generic internal error, the message names `key` and the
    /// hint is actionable for an operator (retrying will not help).
    #[must_use]
    pub(crate) fn bad_config(key: &'static str, source: impl Into<BoxError>) -> Self {
        Self::new(Kind::BadConfig { key }, Some(source.into()))
    }

    /// Returns the wire error code for this error.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self.kind {
            Kind::Rejected(_) | Kind::RejectedParams(_) | Kind::BadRequest(_) => ErrorCode::QueryRejected,
            Kind::Timeout { .. } => ErrorCode::QueryTimeout,
            Kind::RateLimited => ErrorCode::RateLimited,
            Kind::Syntax => ErrorCode::QuerySyntaxError,
            Kind::BadConfig { .. } | Kind::Internal => ErrorCode::InternalError,
        }
    }

    /// Returns `true` if the query, its parameters, or the request were rejected.
    #[must_use]
    pub fn is_rejected(&self) -> bool {
        self.code() == ErrorCode::QueryRejected
    }

    /// Returns `true` if the query timed out.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        self.code() == ErrorCode::QueryTimeout
    }

    /// Returns a stable, caller-facing hint for how to fix the request.
    #[must_use]
    pub fn hint(&self) -> &'static str {
        match &self.kind {
            Kind::Rejected(e) => e.hint(),
            Kind::RejectedParams(_) => "Reduce the number, size, or nesting depth of your query parameters.",
            Kind::BadRequest(_) => "Check the request format: parameters must be key=value or valid JSON.",
            Kind::Timeout { .. } => {
                "Query exceeded the time budget. Add a LIMIT, narrow the MATCH pattern, or reduce path depth."
            }
            Kind::RateLimited => "Too many requests; slow down and retry after a short delay.",
            Kind::Syntax => {
                "The query could not be executed (invalid syntax, semantics, types, or arguments). Check it against the schema from get_schema; retrying it unchanged will not help."
            }
            Kind::BadConfig { .. } => {
                "An environment variable has an invalid value (see the message and .env.example); correct it and restart."
            }
            Kind::Internal => {
                "An internal or transient error. Retry; if it persists, the query may be too expensive or malformed — revise it or report it."
            }
        }
    }

    /// Renders this error as its wire response.
    #[must_use]
    pub fn to_response(&self) -> ErrorResponse {
        ErrorResponse::new(self.code(), self.to_string(), self.hint())
    }

    /// Classifies a `neo4rs` driver error: a client *statement* error (the query
    /// is the problem) maps to a query error (400); everything else — transient,
    /// connection, or protocol failures — maps to internal (500).
    pub(crate) fn from_neo4rs(e: neo4rs::Error) -> Self {
        if let neo4rs::Error::Neo4j(ref n) = e {
            // A statement error is the agent's query, not a gateway fault, so it is
            // an actionable 400 rather than a 500 to blindly retry.
            if is_query_statement_error(n.code()) {
                return Self::new(Kind::Syntax, Some(Box::new(e)));
            }
        }
        Self::internal(Box::new(e))
    }
}

/// Neo4j status-code prefix for a client *statement* error — the agent's query is
/// at fault (syntax, semantic, type, argument, …) — which maps to a 400, not a 500.
const STATEMENT_ERROR_PREFIX: &str = "Neo.ClientError.Statement.";

/// Whether a Neo4j status code is a client statement error (the query's fault).
fn is_query_statement_error(code: &str) -> bool {
    code.starts_with(STATEMENT_ERROR_PREFIX)
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            Kind::Rejected(e) => write!(f, "{e}"),
            Kind::RejectedParams(reason) => write!(f, "parameters rejected: {reason}"),
            Kind::BadRequest(message) => write!(f, "{message}"),
            Kind::BadConfig { key } => write!(f, "invalid value for environment variable {key}"),
            Kind::Timeout { ms } => write!(f, "query exceeded {ms}ms timeout"),
            Kind::RateLimited => f.write_str("rate limit exceeded"),
            Kind::Syntax => f.write_str("the query could not be executed"),
            Kind::Internal => f.write_str("internal error"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // A rejection's cause is its `SanitizeError`; every other kind carries it
        // in `source`.
        match &self.kind {
            Kind::Rejected(e) => Some(e),
            _ => self.source.as_deref().map(|s| s as &(dyn std::error::Error + 'static)),
        }
    }
}

impl From<SanitizeError> for Error {
    fn from(e: SanitizeError) -> Self {
        Self::new(Kind::Rejected(e), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_helpers_agree_with_the_code() {
        let timeout = Error::timeout(10);
        assert!(timeout.is_timeout());
        assert!(!timeout.is_rejected());

        let rejected = Error::bad_request("nope");
        assert!(rejected.is_rejected());
        assert!(!rejected.is_timeout());
    }

    #[test]
    fn client_statement_errors_classify_as_query_errors_not_internal() {
        // The whole statement-error family is the agent's query (400), not a 500.
        assert!(is_query_statement_error("Neo.ClientError.Statement.SyntaxError"));
        assert!(is_query_statement_error("Neo.ClientError.Statement.SemanticError"));
        assert!(is_query_statement_error("Neo.ClientError.Statement.TypeError"));
        assert!(is_query_statement_error("Neo.ClientError.Statement.ArgumentError"));
        // Transient and non-statement client errors stay internal (500).
        assert!(!is_query_statement_error(
            "Neo.TransientError.General.DatabaseUnavailable"
        ));
        assert!(!is_query_statement_error("Neo.ClientError.Security.Unauthorized"));
    }
}
