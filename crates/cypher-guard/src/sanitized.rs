//! The [`SanitizedQuery`] proof token.

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
}

impl SanitizedQuery {
    pub(crate) fn new(cypher: String) -> Self {
        Self { cypher }
    }

    /// Returns the validated, possibly path-bounded, Cypher string.
    #[must_use]
    pub fn cypher(&self) -> &str {
        &self.cypher
    }
}
