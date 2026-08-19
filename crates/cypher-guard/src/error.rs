//! Rejection reasons and the [`SanitizeError`] type.

use core::fmt;
use core::ops::Range;

/// Why a query was rejected by the sanitizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RejectReason {
    /// A graph-mutating clause (`CREATE`, `DELETE`, `SET`, ...).
    Mutation,
    /// An administration or graph-selector clause (`USE`, `SHOW`, `PROFILE`, ...).
    AdminClause,
    /// A `CALL`, in any form: a stored procedure or a `CALL { }` subquery. Denied
    /// wholesale rather than as a mutation, because a read-only `CALL db.labels()`
    /// contains nothing mutating and blaming writes sends the caller hunting for a
    /// clause that is not there.
    CallClause,
    /// A namespaced/qualified procedure or function call (`apoc.*`, `db.*`, ...).
    NamespacedCall,
    /// A quantified path pattern (`(...)+`, `(...)*`, `(...){n,m}`), whose
    /// traversal cost is unbounded by the path-depth cap.
    QuantifiedPath,
    /// A statement separator `;` (multi-statement injection).
    Semicolon,
    /// A `//` or `/* */` comment (which could hide a mutation keyword).
    CommentInjection,
    /// The query does not begin with a read-entry clause.
    NonReadEntry,
    /// A non-ASCII or control character appeared in keyword-eligible position.
    NonAsciiKeyword,
    /// An unterminated string literal or block comment.
    Unterminated,
    /// The query exceeds the configured maximum length.
    TooLong,
    /// The query is empty or contains no statement.
    Empty,
}

impl RejectReason {
    /// Returns a stable, caller-facing hint describing how to fix the rejection.
    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            Self::Mutation => "Only read-only queries are allowed. Remove write clauses such as CREATE, MERGE, SET, DELETE, REMOVE, or DROP.",
            Self::AdminClause => "Administration and graph-selector clauses (USE, SHOW, PROFILE, EXPLAIN, ...) are not permitted.",
            Self::CallClause => "CALL is not permitted in any form, neither stored procedures (CALL db.labels()) nor CALL { } subqueries. Built-in functions need no CALL and are allowed, e.g. count(), collect(), labels(), shortestPath().",
            Self::NamespacedCall => "Namespaced procedure/function calls (apoc.*, db.*, dbms.*, gds.*) are not permitted.",
            Self::QuantifiedPath => "Quantified path patterns ((...)+, (...)*, (...){n,m}) are not permitted; their cost is unbounded. Use a bounded variable-length relationship instead, e.g. -[:FOLLOWS*1..5]->.",
            Self::Semicolon => "Submit a single statement; the ';' separator is not allowed.",
            Self::CommentInjection => "Comments are not permitted; remove // and /* */ from the query.",
            Self::NonReadEntry => "A query must begin with MATCH, OPTIONAL MATCH, WITH, UNWIND, or RETURN.",
            Self::NonAsciiKeyword => "Keywords must be ASCII; non-ASCII or invisible characters were found outside string literals.",
            Self::Unterminated => "The query has an unterminated string literal or block comment.",
            Self::TooLong => "The query is too long; shorten it.",
            Self::Empty => "The query is empty.",
        }
    }
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Mutation => "query contains a mutating clause",
            Self::AdminClause => "query contains an administration or graph-selector clause",
            Self::CallClause => "query contains a CALL clause",
            Self::NamespacedCall => "query contains a namespaced procedure/function call",
            Self::QuantifiedPath => "query contains a quantified path pattern",
            Self::Semicolon => "query contains a statement separator ';'",
            Self::CommentInjection => "query contains a comment",
            Self::NonReadEntry => "query does not begin with a read-entry clause",
            Self::NonAsciiKeyword => "query contains a non-ASCII character in keyword position",
            Self::Unterminated => "query has an unterminated string or comment",
            Self::TooLong => "query exceeds the maximum length",
            Self::Empty => "query is empty",
        };
        f.write_str(s)
    }
}

/// The error returned when a query fails sanitization.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SanitizeError {
    reason: RejectReason,
    span: Option<Range<usize>>,
}

impl SanitizeError {
    pub(crate) fn new(reason: RejectReason, span: Option<Range<usize>>) -> Self {
        Self { reason, span }
    }

    /// Returns the reason the query was rejected.
    #[must_use]
    pub fn reason(&self) -> RejectReason {
        self.reason
    }

    /// Returns the byte span in the original query that triggered the rejection.
    #[must_use]
    pub fn span(&self) -> Option<Range<usize>> {
        self.span.clone()
    }

    /// Returns a stable, caller-facing hint describing how to fix the rejection.
    #[must_use]
    pub fn hint(&self) -> &'static str {
        self.reason.hint()
    }
}

impl fmt::Display for SanitizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for SanitizeError {}
