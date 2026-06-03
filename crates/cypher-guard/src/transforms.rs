//! Guardrail transforms applied after classification.
//!
//! The only transform is variable-length path bounding: every `PathRange` token
//! whose upper bound is missing or greater than `max_path_depth` is rewritten to
//! be bounded (e.g. `*` -> `*1..5`, `*2..` -> `*2..5`, `*1..50` -> `*1..5`). The
//! rewrite splices **only** the offending token's text, right-to-left so earlier
//! byte offsets stay valid, and never touches surrounding syntax.

use crate::tokenizer::{Token, TokenKind};

/// Rewrites each unbounded or over-deep `PathRange` token in `src` to respect
/// `max_depth`, returning the (possibly unchanged) query string.
pub(crate) fn bound_paths(src: &str, tokens: &[Token], max_depth: u32) -> String {
    // Collect rewrites as (span, replacement), then apply right-to-left.
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for t in tokens {
        if t.kind != TokenKind::PathRange {
            continue;
        }
        if let Some(replacement) = rebound(t.text(src), max_depth) {
            edits.push((t.start, t.end, replacement));
        }
    }
    if edits.is_empty() {
        return src.to_owned();
    }
    let mut out = src.to_owned();
    for (start, end, replacement) in edits.into_iter().rev() {
        out.replace_range(start..end, &replacement);
    }
    out
}

/// Given the text of a `PathRange` token (starting with `*`), returns a bounded
/// replacement, or `None` if it is already within `max_depth`.
fn rebound(text: &str, max_depth: u32) -> Option<String> {
    debug_assert!(text.starts_with('*'));
    let body = &text[1..];
    let (lower, upper) = match body.split_once("..") {
        Some((lo, hi)) => (parse_opt(lo), parse_opt(hi)),
        None => {
            if body.is_empty() {
                (None, None) // bare `*`
            } else {
                // `*n` means exactly n: bounded already if n <= max.
                let n = parse_opt(body);
                (n, n)
            }
        }
    };

    let lower_val = lower.unwrap_or(1);
    match upper {
        Some(u) if u <= max_depth => None,
        _ => Some(format!("*{}..{}", lower_val.min(max_depth), max_depth)),
    }
}

fn parse_opt(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        s.parse().ok()
    }
}
