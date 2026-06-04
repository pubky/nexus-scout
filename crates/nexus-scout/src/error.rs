//! The gateway error type and its wire representation.
//!
//! [`Error`] is a canonical struct with a private [`Kind`]: callers classify it
//! through `is_*` helpers and [`Error::code`] rather than matching variants, so
//! new failure modes never break callers. The serialized [`ErrorCode`] enum is a
//! separate, deliberately coarser data-transfer type fixed by the spec.

use std::backtrace::Backtrace;

use cypher_guard::SanitizeError;

use crate::response::ErrorResponse;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// The wire error code is defined in [`nexus_scout_types`] (shared with the
/// `scout` client) and re-exported so the gateway's public API is unchanged.
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
    #[expect(dead_code, reason = "captured for diagnostics; surfaced via Debug")]
    backtrace: Backtrace,
}

impl Error {
    fn new(kind: Kind, source: Option<BoxError>) -> Self {
        Self {
            kind,
            source,
            backtrace: Backtrace::capture(),
        }
    }

    /// Builds a timeout error for a query that exceeded `ms` milliseconds.
    #[must_use]
    pub fn timeout(ms: u64) -> Self {
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
    pub fn internal(source: impl Into<BoxError>) -> Self {
        Self::new(Kind::Internal, Some(source.into()))
    }

    /// Builds a rate-limit rejection (HTTP 429). Produced when the public HTTP
    /// gateway sheds load past its admission limits.
    #[must_use]
    pub fn rate_limited() -> Self {
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
            Kind::Syntax => "The query is not valid Cypher. Check syntax against the schema from get_schema.",
            Kind::BadConfig { .. } => {
                "An environment variable has an invalid value (see the message and .env.example); correct it and restart."
            }
            Kind::Internal => "An internal error occurred. Retry; if it persists, report it.",
        }
    }

    /// Renders this error as its wire response.
    #[must_use]
    pub fn to_response(&self) -> ErrorResponse {
        ErrorResponse::new(self.code(), self.to_string(), self.hint())
    }

    /// Classifies a `neo4rs` driver error into a gateway error.
    ///
    /// Syntax errors are distinguished by their Neo4j status code (the typed
    /// `kind()` has no syntax variant); everything else maps to an internal
    /// error, preserving the driver error as the source.
    pub(crate) fn from_neo4rs(e: neo4rs::Error) -> Self {
        if let neo4rs::Error::Neo4j(ref n) = e {
            // The one unavoidable string match: Neo4jErrorKind models no syntax
            // category, so syntax is detected from the status code prefix.
            let status = n.code();
            if status.starts_with(SYNTAX_PREFIX) || status.starts_with(SEMANTIC_PREFIX) {
                return Self::new(Kind::Syntax, Some(Box::new(e)));
            }
        }
        Self::internal(Box::new(e))
    }
}

/// Neo4j status-code prefixes that denote a Cypher syntax/semantic error.
const SYNTAX_PREFIX: &str = "Neo.ClientError.Statement.Syntax";
const SEMANTIC_PREFIX: &str = "Neo.ClientError.Statement.Semantic";

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            Kind::Rejected(e) => write!(f, "{e}"),
            Kind::RejectedParams(reason) => write!(f, "parameters rejected: {reason}"),
            Kind::BadRequest(message) => write!(f, "{message}"),
            Kind::BadConfig { key } => write!(f, "invalid value for environment variable {key}"),
            Kind::Timeout { ms } => write!(f, "query exceeded {ms}ms timeout"),
            Kind::RateLimited => f.write_str("rate limit exceeded"),
            Kind::Syntax => f.write_str("cypher syntax error"),
            Kind::Internal => f.write_str("internal error"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // A rejection's cause is the embedded `SanitizeError`; every other kind
        // carries its cause (if any) in the boxed `source`. Both are exposed so
        // chain-walkers (`anyhow`, `{:#}`) reach the underlying reason uniformly.
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
