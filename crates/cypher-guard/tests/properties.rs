//! Property-based invariants over arbitrary and structured input.

use cypher_guard::{Limits, Sanitizer};
use proptest::prelude::*;

fn s() -> Sanitizer {
    Sanitizer::new(Limits::default())
}

fn is_denied(word: &str, denied: &[&str]) -> bool {
    !word.is_empty() && denied.iter().any(|k| word.eq_ignore_ascii_case(k))
}

/// An INDEPENDENT reference scanner: does `s` contain a denied keyword as a
/// code-position word, following Neo4j's real string/backtick/comment grammar
/// (`'`/`"` with `\` escapes, `` ` `` with doubled-backtick escapes, `//` and
/// `/* */` comments)? It shares no code with `unicode::validate` or
/// `tokenizer::lex`, so if those two ever diverge in a way that exposes a denied
/// keyword the differential property below catches it.
fn denied_keyword_in_code(s: &str, denied: &[&str]) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let mut word = String::new();
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' => {
                if is_denied(&word, denied) {
                    return true;
                }
                word.clear();
                let quote = b[i];
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'`' => {
                if is_denied(&word, denied) {
                    return true;
                }
                word.clear();
                i += 1;
                while i < b.len() {
                    if b[i] == b'`' {
                        if b.get(i + 1) == Some(&b'`') {
                            i += 2; // doubled backtick: an escaped `, stays inside
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                if is_denied(&word, denied) {
                    return true;
                }
                word.clear();
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                if is_denied(&word, denied) {
                    return true;
                }
                word.clear();
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            c if c.is_ascii_alphabetic() || c == b'_' || (!word.is_empty() && c.is_ascii_digit()) => {
                word.push(c as char);
                i += 1;
            }
            _ => {
                if is_denied(&word, denied) {
                    return true;
                }
                word.clear();
                i += 1;
            }
        }
    }
    is_denied(&word, denied)
}

/// Adversarial fragments that stress the string/backtick/comment seam: bare
/// keywords interleaved with quotes, backticks, doubled backticks, and escapes.
fn adversarial_fragment() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("`"),
        Just("'"),
        Just("\""),
        Just("\\"),
        Just("/"),
        Just("*"),
        Just("CREATE"),
        Just("DELETE"),
        Just("MERGE"),
        Just("SET"),
        Just("MATCH"),
        Just("RETURN"),
        Just("(n)"),
        Just("n.x"),
        Just("a"),
        Just(" "),
    ]
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

    // The core safety invariant, exercised against the validator↔tokenizer
    // backtick/escape seam: no query the sanitizer ACCEPTS may contain a denied
    // keyword in code position, as judged by an independent reference scanner.
    // A future refactor that desynced the two hand-written region machines would
    // be caught here, deterministically, rather than only by the weekly fuzz job.
    #[test]
    fn accepted_query_never_exposes_a_denied_keyword(
        parts in prop::collection::vec(adversarial_fragment(), 0..24),
    ) {
        let input: String = parts.concat();
        let denied = cypher_guard::denied_keywords();
        if let Ok(out) = s().sanitize(&input) {
            prop_assert!(
                !denied_keyword_in_code(out.cypher(), &denied),
                "accepted output exposes a denied keyword in code position: input={input:?} output={:?}",
                out.cypher()
            );
        }
    }
}
