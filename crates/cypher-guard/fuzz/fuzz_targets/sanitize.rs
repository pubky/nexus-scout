#![no_main]
//! Differential fuzz oracle: the sanitizer must never panic, and any query it
//! *accepts* must satisfy every guardrail. An accepted query that violates a
//! guardrail is a crash, which turns "block 100%" into a fuzzable property.

use cypher_guard::{Limits, Sanitizer};
use libfuzzer_sys::fuzz_target;

fn upper_contains_keyword(out: &str, denied: &[&str]) -> bool {
    // An accepted output must not contain a denied clause keyword as a
    // standalone uppercase word outside any string. `denied` comes from the
    // crate itself so the oracle cannot drift from the classifier.
    let mut in_string = false;
    let mut quote = '\0';
    let upper = out.to_uppercase();
    // Token scan on the original (case-insensitive via `upper`) skipping strings.
    let mut word = String::new();
    for (oc, uc) in out.chars().zip(upper.chars()) {
        if in_string {
            if oc == quote {
                in_string = false;
            }
            continue;
        }
        if oc == '\'' || oc == '"' {
            in_string = true;
            quote = oc;
            continue;
        }
        if uc.is_ascii_alphabetic() {
            word.push(uc);
        } else {
            if denied.contains(&word.as_str()) {
                return true;
            }
            word.clear();
        }
    }
    denied.contains(&word.as_str())
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else { return };
    let denied = cypher_guard::denied_keywords();
    let sanitizer = Sanitizer::new(Limits::default());
    if let Ok(q) = sanitizer.sanitize(input) {
        let out = q.cypher();
        assert!(!out.contains(';'), "accepted output contains a semicolon: {out:?}");
        assert!(!out.contains("//") && !out.contains("/*"), "accepted output contains a comment: {out:?}");
        assert!(
            !upper_contains_keyword(out, &denied),
            "accepted output contains a denied keyword: {out:?}"
        );
    }
});
