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
pub use sanitized::{ResultLimit, SanitizedQuery};

use tokenizer::{Token, TokenKind};

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
    /// depth. Neo4j 5 quantified path patterns are not rebindable this way and are
    /// rejected outright ([`RejectReason::QuantifiedPath`]).
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
        let result_limit = extract_result_limit(&tokens, &normalized);
        let (bounded, notes) = transforms::bound_paths(&normalized, &tokens, self.limits.max_path_depth);
        Ok(SanitizedQuery::new(bounded, result_limit, notes))
    }
}

/// Finds the query's top-level `LIMIT` so the gateway can honor it. Scans the
/// token stream tracking bracket depth and takes the **last** depth-0 `LIMIT`
/// (so subquery limits inside `{ }`/`( )` and earlier `UNION` branches don't win);
/// the following significant token decides the value.
fn extract_result_limit(tokens: &[Token], src: &str) -> ResultLimit {
    let mut depth: i32 = 0;
    let mut found = ResultLimit::None;
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::Punct => match t.text(src) {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                _ => {}
            },
            TokenKind::Word if depth == 0 && t.text(src).eq_ignore_ascii_case("LIMIT") => {
                let next = tokens[i + 1..]
                    .iter()
                    .find(|n| !matches!(n.kind, TokenKind::Whitespace | TokenKind::Comment));
                found = match next {
                    Some(n) if n.kind == TokenKind::Number => {
                        let txt = n.text(src);
                        if txt.bytes().all(|b| b.is_ascii_digit()) {
                            // Saturate an over-large literal to the u32 ceiling; it
                            // is clamped to the gateway max on use anyway.
                            ResultLimit::Fixed(txt.parse().unwrap_or(u32::MAX))
                        } else {
                            // A non-integer LIMIT is invalid Cypher; let Neo4j reject
                            // it while reading up to the ceiling.
                            ResultLimit::Dynamic
                        }
                    }
                    Some(n) if n.kind == TokenKind::Parameter => ResultLimit::Dynamic,
                    // No value after LIMIT: malformed; keep the previous finding.
                    _ => found,
                };
            }
            _ => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{Limits, ResultLimit, Sanitizer};

    fn limit_of(cypher: &str) -> ResultLimit {
        Sanitizer::new(Limits::default())
            .sanitize(cypher)
            .unwrap()
            .result_limit()
    }

    #[test]
    fn extracts_a_top_level_literal_limit() {
        assert_eq!(limit_of("MATCH (u:User) RETURN u.id LIMIT 50"), ResultLimit::Fixed(50));
        // brackets in the query don't break depth tracking
        assert_eq!(
            limit_of("MATCH (u:User) WHERE u.id IN [1,2,3] RETURN u LIMIT 5"),
            ResultLimit::Fixed(5)
        );
    }

    #[test]
    fn no_limit_is_none() {
        assert_eq!(limit_of("MATCH (u:User) RETURN u.id"), ResultLimit::None);
    }

    #[test]
    fn parameterized_limit_is_dynamic() {
        assert_eq!(limit_of("MATCH (u:User) RETURN u.id LIMIT $n"), ResultLimit::Dynamic);
    }

    #[test]
    fn subquery_limit_is_ignored() {
        // The LIMIT lives inside a COUNT{} subquery (depth > 0); there is no
        // top-level LIMIT, so the result limit is None.
        let q = "MATCH (u:User) RETURN count{ MATCH (u)-[:FOLLOWS]->(x) RETURN x LIMIT 3 } AS c";
        assert_eq!(limit_of(q), ResultLimit::None);
    }

    #[test]
    fn union_takes_the_last_top_level_limit() {
        let q = "MATCH (a:User) RETURN a.id LIMIT 5 UNION MATCH (b:Post) RETURN b.id AS id LIMIT 10";
        assert_eq!(limit_of(q), ResultLimit::Fixed(10));
    }

    #[test]
    fn oversized_literal_saturates() {
        assert_eq!(
            limit_of("MATCH (u:User) RETURN u.id LIMIT 99999999999"),
            ResultLimit::Fixed(u32::MAX)
        );
    }

    #[test]
    fn bounding_a_path_records_a_note() {
        let q = Sanitizer::new(Limits::default())
            .sanitize("MATCH p=(a:User)-[:FOLLOWS*1..10]->(b:User) RETURN p LIMIT 5")
            .unwrap();
        assert!(q.cypher().contains("*1..5"));
        assert_eq!(q.notes().len(), 1);
        assert!(q.notes()[0].contains("bounded to"));
        // an unbounded query has no notes
        let clean = Sanitizer::new(Limits::default())
            .sanitize("MATCH (u:User) RETURN u LIMIT 5")
            .unwrap();
        assert!(clean.notes().is_empty());
    }
}
