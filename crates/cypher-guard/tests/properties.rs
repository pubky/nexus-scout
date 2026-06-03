//! Property-based invariants over arbitrary and structured input.

use cypher_guard::{Limits, Sanitizer};
use proptest::prelude::*;

fn s() -> Sanitizer {
    Sanitizer::new(Limits::default())
}

proptest! {
    // The sanitizer must never panic or hang on arbitrary input (the lexer is
    // single-pass O(n), so termination is structural).
    #[test]
    fn never_panics_on_arbitrary_input(input in ".{0,300}") {
        let _ = s().sanitize(&input);
    }

    // A denied keyword present as a real, space-delimited word is always rejected.
    #[test]
    fn deny_keyword_as_real_token_is_rejected(
        tail in "[a-z ]{0,40}",
        kw in prop::sample::select(&["CREATE", "DELETE", "SET", "MERGE", "REMOVE", "DROP", "DETACH"][..]),
    ) {
        let q = format!("MATCH (n) {kw} {tail}");
        prop_assert!(s().sanitize(&q).is_err());
    }

    // A denied keyword that appears only inside a string literal is NOT rejected
    // for being a keyword (the query is otherwise a valid read).
    #[test]
    fn deny_keyword_inside_string_is_not_a_mutation(
        kw in prop::sample::select(&["CREATE", "DELETE", "SET", "MERGE"][..]),
    ) {
        let q = format!("MATCH (n:User) WHERE n.bio = '{kw}' RETURN n LIMIT 5");
        let res = s().sanitize(&q);
        prop_assert!(res.is_ok(), "rejected keyword-in-string: {:?}", res.err());
    }

    // Any accepted query is idempotent under re-sanitization (transforms settle).
    #[test]
    fn accepted_output_is_idempotent(depth in 0u32..12) {
        let q = format!("MATCH (a)-[:R*{depth}..]->(b) RETURN a, b LIMIT 5");
        if let Ok(first) = s().sanitize(&q) {
            let second = s().sanitize(first.cypher()).expect("re-sanitize accepted output");
            prop_assert_eq!(first.cypher(), second.cypher());
        }
    }
}
