//! Pure, read-only Cypher sanitizer: validates an untrusted query is side-effect
//! free and bounds unbounded path patterns. No I/O, no driver dependency.
//!
//! # Examples
//!
//! ```
//! use cypher_guard::{Sanitizer, Limits};
//!
//! let sanitizer = Sanitizer::new(Limits::default());
//! let q = sanitizer.sanitize("MATCH (u:User) RETURN u.name").unwrap();
//! assert!(q.cypher().contains("MATCH"));
//!
//! assert!(sanitizer.sanitize("MATCH (u) DETACH DELETE u").is_err());
//! ```

#![deny(missing_docs)]

mod error;
mod rules;
mod sanitized;
mod tokenizer;
mod transforms;
mod unicode;

#[doc(inline)]
pub use error::{RejectReason, SanitizeError};
#[doc(inline)]
pub use sanitized::SanitizedQuery;

/// The keywords the sanitizer rejects in keyword position (mutation and admin clauses).
#[must_use]
pub fn denied_keywords() -> Vec<&'static str> {
    rules::denied_keywords()
}

/// Guardrail limits applied during sanitization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum query length in characters, checked after Unicode normalization.
    pub max_query_length: usize,
    /// Classic variable-length relationship paths (`*`, `*2..`) are capped at this
    /// depth; Neo4j 5 quantified paths are bounded by the server transaction
    /// timeout, not here.
    pub max_path_depth: u32,
}

impl Limits {
    /// Creates limits with an explicit query length and path depth.
    #[must_use]
    pub fn new(max_query_length: usize, max_path_depth: u32) -> Self {
        Self {
            max_query_length,
            max_path_depth,
        }
    }
}

/// Default maximum query length in characters.
const DEFAULT_MAX_QUERY_LENGTH: usize = 2000;
/// Default cap on variable-length relationship path depth.
const DEFAULT_MAX_PATH_DEPTH: u32 = 5;

impl Default for Limits {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_QUERY_LENGTH, DEFAULT_MAX_PATH_DEPTH)
    }
}

/// Validates and transforms untrusted Cypher into a [`SanitizedQuery`].
#[derive(Debug, Clone)]
pub struct Sanitizer {
    limits: Limits,
}

impl Sanitizer {
    /// Creates a sanitizer with the given guardrail limits.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self { limits }
    }

    /// Validates that `cypher` is read-only and returns an executable proof token.
    ///
    /// The query is normalized, lexed, classified against an allow/deny policy,
    /// and has unbounded variable-length paths rewritten to be bounded.
    ///
    /// # Errors
    ///
    /// Returns [`SanitizeError`] if the query contains a forbidden construct
    /// (mutation, administration clause, namespaced call, comment, semicolon),
    /// uses an unsupported keyword, has a non-ASCII character in keyword
    /// position, is unterminated, or exceeds the configured length.
    pub fn sanitize(&self, cypher: impl AsRef<str>) -> Result<SanitizedQuery, SanitizeError> {
        let cypher = cypher.as_ref();

        // Reject oversized raw input (>4 bytes/char ceiling) before NFC allocates a
        // copy of an attacker-sized string.
        if cypher.len() > self.limits.max_query_length.saturating_mul(4) {
            return Err(SanitizeError::new(RejectReason::TooLong, None));
        }

        let normalized = unicode::normalize_and_validate(cypher)?;

        if normalized.chars().count() > self.limits.max_query_length {
            return Err(SanitizeError::new(RejectReason::TooLong, None));
        }

        let tokens = tokenizer::lex(&normalized)?;
        rules::classify(&tokens, &normalized)?;
        let bounded = transforms::bound_paths(&normalized, &tokens, self.limits.max_path_depth);
        Ok(SanitizedQuery::new(bounded))
    }
}
