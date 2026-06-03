//! The [`SanitizedQuery`] proof token.

/// A Cypher query that has passed read-only validation.
///
/// `SanitizedQuery` is a proof token: its only constructor is crate-private, so
/// the sole way for a consumer to obtain one is [`Sanitizer::sanitize`], which
/// runs the full validation pipeline. An executor that accepts only a
/// `&SanitizedQuery` therefore cannot run an unvalidated query.
///
/// The type deliberately does **not** implement `Deserialize`, `Default`, or any
/// mutator: those would let a caller forge a token and bypass validation.
///
/// [`Sanitizer::sanitize`]: crate::Sanitizer::sanitize
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
