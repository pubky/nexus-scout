//! The [`SanitizedQuery`] proof token.

/// The row limit a query requests at top level, used by the gateway to size its
/// read budget so a query's own `LIMIT` is honored (up to the gateway's ceiling).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultLimit {
    /// No top-level `LIMIT` clause.
    None,
    /// A top-level `LIMIT <literal>` (clamped to the gateway ceiling on use).
    Fixed(u32),
    /// A top-level `LIMIT $param`: the value is only known once parameters bind,
    /// so the gateway reads up to its ceiling and lets the server-side `LIMIT`
    /// do the cutting.
    Dynamic,
}

/// A Cypher query that has passed read-only validation.
///
/// A proof token: its only constructor is crate-private, so the sole way to obtain
/// one is [`Sanitizer::sanitize`](crate::Sanitizer::sanitize). It deliberately
/// implements no `Deserialize`/`Default`/mutator, which would let a caller forge a
/// token and bypass validation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SanitizedQuery {
    cypher: String,
    result_limit: ResultLimit,
    notes: Vec<String>,
}

impl SanitizedQuery {
    pub(crate) fn new(cypher: String, result_limit: ResultLimit, notes: Vec<String>) -> Self {
        Self {
            cypher,
            result_limit,
            notes,
        }
    }

    /// Returns the validated, possibly path-bounded, Cypher string.
    #[must_use]
    pub fn cypher(&self) -> &str {
        &self.cypher
    }

    /// The top-level row limit the query asked for, if any.
    #[must_use]
    pub fn result_limit(&self) -> ResultLimit {
        self.result_limit
    }

    /// Human-readable notes about transforms applied to the query (e.g. a
    /// variable-length path that was bounded). Empty when nothing was rewritten.
    #[must_use]
    pub fn notes(&self) -> &[String] {
        &self.notes
    }
}
