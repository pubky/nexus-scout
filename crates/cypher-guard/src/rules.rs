//! Allow/deny policy tables and the pure classifier. Only keyword-position `Word`
//! tokens are examined — string, comment, and backtick contents are never scanned
//! — so keyword-looking data cannot trip a rule and a hidden keyword cannot slip
//! past one. A word is rejected only if it is an explicitly denied clause keyword;
//! bare identifiers are allowed.

use std::ops::Range;

use crate::error::{RejectReason, SanitizeError};
use crate::tokenizer::{Token, TokenKind};

/// Graph-mutating clauses. Rejected anywhere.
///
/// Because the gateway runs against Neo4j Community (no RBAC, so no read-only DB
/// user), the sanitizer is the *sole* write guard, so this list errs toward
/// over-inclusion. Version-pinned to Cypher 5 / Neo4j 5.x.
#[rustfmt::skip]
pub(crate) const DENY_MUTATION: &[&str] = &[
    "CREATE", "MERGE", "SET", "DELETE", "DETACH", "NODETACH", "REMOVE", "DROP",
    "INSERT", "FOREACH", "LOAD", "USING",
];

/// The procedure-call keyword, denied on its own terms. It is not a mutation: a
/// `CALL db.labels()` writes nothing, and reporting it as one tells the caller to
/// remove write clauses that do not exist. Kept a table of its own so
/// [`denied_keywords`] still yields it for the property test and the fuzz oracle.
pub(crate) const DENY_CALL: &[&str] = &["CALL"];

/// Administration and graph-selector clauses. Rejected anywhere.
#[rustfmt::skip]
pub(crate) const DENY_ADMIN: &[&str] = &[
    "USE", "SHOW", "TERMINATE", "START", "GRANT", "DENY", "REVOKE", "ALTER", "RENAME",
    "ENABLE", "DISABLE", "PROFILE", "EXPLAIN",
];

/// Keywords that may legally begin a read-only query.
const READ_ENTRY: &[&str] = &["MATCH", "OPTIONAL", "WITH", "UNWIND", "RETURN"];

/// The full set of denied keywords (mutation + `CALL` + administration), as one
/// list. Every deny table must be chained in here: it is the independent oracle
/// the property test and the fuzzer scan accepted output against.
pub(crate) fn denied_keywords() -> Vec<&'static str> {
    DENY_MUTATION.iter().chain(DENY_CALL).chain(DENY_ADMIN).copied().collect()
}

fn in_table(word: &str, table: &[&str]) -> bool {
    table.iter().any(|kw| word.eq_ignore_ascii_case(kw))
}

fn punct_is(tok: &Token, src: &str, c: u8) -> bool {
    tok.kind == TokenKind::Punct && tok.text(src).as_bytes() == [c]
}

/// Classifies a token stream, enforcing the read-only policy.
///
/// # Errors
///
/// Returns the first [`SanitizeError`] encountered.
pub(crate) fn classify(tokens: &[Token], src: &str) -> Result<(), SanitizeError> {
    let significant: Vec<&Token> = tokens.iter().filter(|t| t.kind != TokenKind::Whitespace).collect();

    if significant.is_empty() {
        return Err(SanitizeError::new(RejectReason::Empty, None));
    }

    // Comments and semicolons are structural injection vectors.
    for t in &significant {
        match t.kind {
            TokenKind::Comment => return Err(SanitizeError::new(RejectReason::CommentInjection, Some(t.start..t.end))),
            TokenKind::Semicolon => return Err(SanitizeError::new(RejectReason::Semicolon, Some(t.start..t.end))),
            _ => {}
        }
    }

    for t in &significant {
        if t.kind != TokenKind::Word {
            continue;
        }
        let word = t.text(src);
        if in_table(word, DENY_MUTATION) {
            return Err(SanitizeError::new(RejectReason::Mutation, Some(t.start..t.end)));
        }
        // Before the mutation check would have caught it via `CALL`: a `CALL { }`
        // wrapping a write still reports the CALL, because removing the CALL is the
        // fix either way and scan order should not decide the message.
        if in_table(word, DENY_CALL) {
            return Err(SanitizeError::new(RejectReason::CallClause, Some(t.start..t.end)));
        }
        if in_table(word, DENY_ADMIN) {
            return Err(SanitizeError::new(RejectReason::AdminClause, Some(t.start..t.end)));
        }
    }

    // Namespaced call `Word ('.' Word)+ '('`, whitespace-insensitive across the dots.
    if let Some(span) = find_namespaced_call(&significant, src) {
        return Err(SanitizeError::new(RejectReason::NamespacedCall, Some(span)));
    }

    // Quantified path patterns have unbounded traversal cost (the `*1..5` cap only
    // bounds classic variable-length relationships, not these).
    if let Some(span) = find_quantified_path(&significant, src) {
        return Err(SanitizeError::new(RejectReason::QuantifiedPath, Some(span)));
    }

    let first_word = significant.iter().find(|t| t.kind == TokenKind::Word);
    match first_word {
        Some(t) if in_table(t.text(src), READ_ENTRY) => {}
        Some(t) => return Err(SanitizeError::new(RejectReason::NonReadEntry, Some(t.start..t.end))),
        None => return Err(SanitizeError::new(RejectReason::NonReadEntry, None)),
    }

    Ok(())
}

/// Finds a quantified path pattern — a parenthesized group that contains a
/// relationship and is immediately followed by a quantifier `+`, `*`, or `{` — and
/// returns its byte span. Requiring a relationship inside the parens distinguishes a
/// real QPP from arithmetic like `(a + b) * 2` or `(x)*2` (no relationship → not a
/// QPP). A relationship inside is a `-[` detail bracket or an `->`/`<-` arrow.
fn find_quantified_path(sig: &[&Token], src: &str) -> Option<Range<usize>> {
    // Stack of open parens: (index in `sig`, whether a relationship was seen inside).
    let mut stack: Vec<(usize, bool)> = Vec::new();
    for i in 0..sig.len() {
        if punct_is(sig[i], src, b'(') {
            stack.push((i, false));
        } else if is_relationship_at(sig, i, src) {
            if let Some(top) = stack.last_mut() {
                top.1 = true;
            }
        } else if punct_is(sig[i], src, b')') {
            if let Some((open, saw_rel)) = stack.pop() {
                let next_is_quantifier = sig
                    .get(i + 1)
                    .is_some_and(|n| punct_is(n, src, b'+') || punct_is(n, src, b'*') || punct_is(n, src, b'{'));
                if saw_rel && next_is_quantifier {
                    return Some(sig[open].start..sig[i + 1].end);
                }
            }
        }
    }
    None
}

/// Whether the token at `i` marks a relationship: a `-[` detail bracket, an `->` /
/// `<-` arrow head (so `-[:R]->` and bare `-->`/`<--` patterns count), or a bare
/// undirected `--` edge between node patterns.
fn is_relationship_at(sig: &[&Token], i: usize, src: &str) -> bool {
    let prev_is = |c: u8| i > 0 && punct_is(sig[i - 1], src, c);
    let next_is = |c: u8| sig.get(i + 1).is_some_and(|t| punct_is(t, src, c));
    let after_is = |off: usize, c: u8| sig.get(i + off).is_some_and(|t| punct_is(t, src, c));
    (punct_is(sig[i], src, b'[') && prev_is(b'-'))
        || (punct_is(sig[i], src, b'>') && prev_is(b'-'))
        || (punct_is(sig[i], src, b'<') && next_is(b'-'))
        // Bare undirected `--` edge. The `)`-before / `(`-after node-pattern boundary
        // distinguishes a relationship (`(a)--(b)`) from arithmetic double-minus
        // (`a - -b`), whose dashes are flanked by operands, not node groups.
        || (punct_is(sig[i], src, b'-') && next_is(b'-') && (prev_is(b')') || after_is(2, b'(')))
}

/// A token that can name a namespace segment. Backtick-quoted segments count, else
/// a backtick could escape the namespaced-call rule (`apoc.`cypher`.doIt`).
fn is_name_segment(tok: &Token) -> bool {
    matches!(tok.kind, TokenKind::Word | TokenKind::BacktickIdent)
}

/// Scans the significant-token slice for a namespaced call and returns the byte
/// span of the offending call head, if any.
fn find_namespaced_call(sig: &[&Token], src: &str) -> Option<Range<usize>> {
    let mut i = 0;
    while i < sig.len() {
        if is_name_segment(sig[i]) {
            let head_start = sig[i].start;
            let mut j = i;
            let mut dots = 0u32;
            while j + 2 < sig.len() && punct_is(sig[j + 1], src, b'.') && is_name_segment(sig[j + 2]) {
                dots += 1;
                j += 2;
            }
            if dots >= 1 && j + 1 < sig.len() && punct_is(sig[j + 1], src, b'(') {
                return Some(head_start..sig[j].end);
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }
    None
}
