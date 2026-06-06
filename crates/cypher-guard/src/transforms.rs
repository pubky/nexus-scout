//! Variable-length path bounding: each well-formed `PathRange` whose upper bound is
//! missing or over `max_path_depth` is rewritten bounded (`*` -> `*1..5`). A
//! malformed range (e.g. `*1.2.3`) is left unchanged for Neo4j to reject — the
//! transform never turns invalid input into a valid bounded range. Edits splice
//! only the offending token, applied right-to-left so earlier offsets stay valid.

use crate::tokenizer::{Token, TokenKind};

/// Rewrites each unbounded or over-deep `PathRange` token in `src` to respect
/// `max_depth`, returning the (possibly unchanged) query string plus a note for
/// each rewrite (empty when nothing was bounded).
pub(crate) fn bound_paths(src: &str, tokens: &[Token], max_depth: u32) -> (String, Vec<String>) {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    for t in tokens {
        if t.kind != TokenKind::PathRange {
            continue;
        }
        let original = t.text(src);
        if let Some(replacement) = rebound(original, max_depth) {
            notes.push(format!("variable-length path '{original}' bounded to '{replacement}'"));
            edits.push((t.start, t.end, replacement));
        }
    }
    if edits.is_empty() {
        return (src.to_owned(), notes);
    }
    let mut out = src.to_owned();
    for (start, end, replacement) in edits.into_iter().rev() {
        out.replace_range(start..end, &replacement);
    }
    (out, notes)
}

/// Given the text of a `PathRange` token (starting with `*`), returns a bounded
/// replacement, or `None` to leave it unchanged — both when it is already within
/// `max_depth` and when it is malformed (left for Neo4j to reject, never rewritten
/// into a valid range).
fn rebound(text: &str, max_depth: u32) -> Option<String> {
    debug_assert!(text.starts_with('*'));
    let (lower, upper) = parse_range(&text[1..])?;
    let lower_val = lower.unwrap_or(1);
    match upper {
        Some(u) if u <= max_depth => None,
        _ => Some(format!("*{}..{}", lower_val.min(max_depth), max_depth)),
    }
}

/// Parses a path-range body (the text after `*`) into `(lower, upper)`, or `None`
/// if it is not one of the well-formed shapes `*`, `*n`, `*n..`, `*..m`, `*n..m`.
/// An empty side of a `..` range is an open (`None`) bound.
fn parse_range(body: &str) -> Option<(Option<u32>, Option<u32>)> {
    if body.is_empty() {
        return Some((None, None));
    }
    if let Some((lo, hi)) = body.split_once("..") {
        let lower = if lo.is_empty() { None } else { Some(parse_uint(lo)?) };
        let upper = if hi.is_empty() { None } else { Some(parse_uint(hi)?) };
        Some((lower, upper))
    } else {
        let n = parse_uint(body)?;
        Some((Some(n), Some(n)))
    }
}

/// A bare non-negative integer (digits only). An all-digit value that overflows
/// `u32` saturates to `u32::MAX`, so an over-large bound is still treated as
/// over-deep (and bounded), not mistaken for malformed and left through.
fn parse_uint(s: &str) -> Option<u32> {
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        Some(s.parse().unwrap_or(u32::MAX))
    } else {
        None
    }
}
