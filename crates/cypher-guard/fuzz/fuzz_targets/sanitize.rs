#![no_main]
//! Differential fuzz oracle: the sanitizer must never panic, and any query it
//! accepts must satisfy every guardrail.

use cypher_guard::{Limits, Sanitizer};
use libfuzzer_sys::fuzz_target;

/// Whether a maximal identifier-ish run resolves to a denied keyword the way Neo4j
/// would lex it. A run is `[A-Za-z0-9_]+`; Neo4j reads a leading number then an
/// identifier, so we strip a leading digit run and check whether the remainder is
/// exactly a denied keyword. This flags digit-glued keywords (`0CREATE`, `1SET`)
/// while correctly leaving identifiers like `load3` / `set1` alone.
fn run_is_denied(run: &str, denied: &[&str]) -> bool {
    if run.is_empty() {
        return false;
    }
    let tail = run.trim_start_matches(|c: char| c.is_ascii_digit());
    denied.iter().any(|k| tail.eq_ignore_ascii_case(k))
}

/// Scans an accepted output for a guardrail violation *in code position* and returns
/// its kind, or `None` if the output is clean. String literals (`'..'`/`"..."`) and
/// backtick identifiers are opaque data, a `;`, comment, or keyword inside them is
/// exactly as inert as Neo4j treats it, so they are skipped with the same escape
/// rules the sanitizer's own lexer uses (`\` in strings, `` `` `` in backticks).
/// Checking only code position is essential: a naive `out.contains(';')` would
/// false-positive on `'a;b'`, and `out.contains("//")` on `'http://x'`.
fn code_position_violation(out: &str, denied: &[&str]) -> Option<&'static str> {
    #[derive(PartialEq)]
    enum Region {
        Code,
        Single,
        Double,
        Backtick,
    }
    let mut region = Region::Code;
    let mut escaped = false;
    let mut run = String::new();
    let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        match region {
            Region::Code => {
                if c.is_ascii_alphanumeric() || c == '_' {
                    run.push(c);
                    continue;
                }
                if run_is_denied(&run, denied) {
                    return Some("denied keyword");
                }
                run.clear();
                match c {
                    ';' => return Some("semicolon"),
                    '/' if matches!(chars.peek(), Some('/' | '*')) => return Some("comment"),
                    '\'' => region = Region::Single,
                    '"' => region = Region::Double,
                    '`' => region = Region::Backtick,
                    '$' => {
                        // Parameter reference ($name or $`name`): the name is data the
                        // driver binds, never a keyword Neo4j executes, so consume it
                        // without classifying, `$revoke` must not read as REVOKE.
                        if chars.peek() == Some(&'`') {
                            chars.next();
                            while let Some(c2) = chars.next() {
                                if c2 == '`' {
                                    if chars.peek() == Some(&'`') {
                                        chars.next();
                                    } else {
                                        break;
                                    }
                                }
                            }
                        } else {
                            while chars.peek().is_some_and(|c2| c2.is_ascii_alphanumeric() || *c2 == '_') {
                                chars.next();
                            }
                        }
                    }
                    _ => {}
                }
            }
            Region::Single | Region::Double => {
                let quote = if region == Region::Single { '\'' } else { '"' };
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == quote {
                    region = Region::Code;
                }
            }
            Region::Backtick => {
                if c == '`' {
                    if chars.peek() == Some(&'`') {
                        chars.next();
                    } else {
                        region = Region::Code;
                    }
                }
            }
        }
    }
    if run_is_denied(&run, denied) {
        return Some("denied keyword");
    }
    None
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else { return };
    let denied = cypher_guard::denied_keywords();
    let sanitizer = Sanitizer::new(Limits::default());
    if let Ok(q) = sanitizer.sanitize(input) {
        let out = q.cypher();
        if let Some(kind) = code_position_violation(out, &denied) {
            panic!("accepted output has a {kind} in code position: {out:?}");
        }
    }
});
