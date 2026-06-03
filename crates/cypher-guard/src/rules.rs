//! Allow/deny policy tables and the pure classifier.
//!
//! The classifier is `fn(&[Token], &str) -> Result<(), SanitizeError>`: pure,
//! with no I/O and no globals. It applies its checks in a fixed order so the
//! rejection reason is deterministic, and consults only keyword-position
//! `Word` tokens - string, comment, and backtick tokens are never examined, so
//! keyword-looking data cannot trip a rule and a hidden keyword cannot slip past
//! one.
//!
//! Policy shape (sound without a full parser): a `Word` is rejected only if it
//! is an explicitly **denied** clause keyword. Every other word - permitted read
//! keywords, and bare identifiers such as variables, labels, and function names
//! that are indistinguishable from variables without binding analysis - is
//! allowed. Forward-compat against *new* write clauses rests on the read-entry
//! rule (a query must begin with a read clause) plus the read-only database user
//! (defense layer 2), not on default-denying arbitrary words, which would reject
//! every variable name.

use std::ops::Range;

use crate::error::{RejectReason, SanitizeError};
use crate::tokenizer::{Token, TokenKind};

/// Graph-mutating clauses plus the procedure-call keyword. Rejected anywhere.
///
/// Covers the classic Cypher write clauses, the GQL-style `INSERT`/`NODETACH`
/// that Neo4j 5.x added, and `CALL`/`LOAD`/`USING`/`FOREACH`. Because this
/// gateway runs against Neo4j Community (no role-based access control, so no
/// read-only database user is possible), the sanitizer is the *sole* write
/// guard - a missing keyword here is a real write hole, so this list errs
/// toward over-inclusion.
#[rustfmt::skip]
pub(crate) const DENY_MUTATION: &[&str] = &[
    "CREATE", "MERGE", "SET", "DELETE", "DETACH", "NODETACH", "REMOVE", "DROP",
    "INSERT", "FOREACH", "LOAD", "CALL", "USING",
];

/// Administration and graph-selector clauses. Rejected anywhere.
#[rustfmt::skip]
pub(crate) const DENY_ADMIN: &[&str] = &[
    "USE", "SHOW", "TERMINATE", "START", "GRANT", "DENY", "REVOKE", "ALTER", "RENAME",
    "ENABLE", "DISABLE", "PROFILE", "EXPLAIN",
];

/// Keywords that may legally begin a read-only query.
const READ_ENTRY: &[&str] = &["MATCH", "OPTIONAL", "WITH", "UNWIND", "RETURN"];

/// The full set of denied keywords (mutation + administration), as one list.
pub(crate) fn denied_keywords() -> Vec<&'static str> {
    DENY_MUTATION.iter().chain(DENY_ADMIN).copied().collect()
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
/// Returns the first [`SanitizeError`] encountered. Checks run in a fixed order:
/// comments/semicolons, then denied (mutation/admin) keywords, then namespaced
/// calls, then the read-entry requirement.
pub(crate) fn classify(tokens: &[Token], src: &str) -> Result<(), SanitizeError> {
    let significant: Vec<&Token> = tokens.iter().filter(|t| t.kind != TokenKind::Whitespace).collect();

    if significant.is_empty() {
        return Err(SanitizeError::new(RejectReason::Empty, None));
    }

    // 1. Comments and semicolons are structural injection vectors: never allowed.
    for t in &significant {
        match t.kind {
            TokenKind::Comment => return Err(SanitizeError::new(RejectReason::CommentInjection, Some(t.start..t.end))),
            TokenKind::Semicolon => return Err(SanitizeError::new(RejectReason::Semicolon, Some(t.start..t.end))),
            _ => {}
        }
    }

    // 2. Denied clause keywords (mutation or administration).
    for t in &significant {
        if t.kind != TokenKind::Word {
            continue;
        }
        let word = t.text(src);
        if in_table(word, DENY_MUTATION) {
            return Err(SanitizeError::new(RejectReason::Mutation, Some(t.start..t.end)));
        }
        if in_table(word, DENY_ADMIN) {
            return Err(SanitizeError::new(RejectReason::AdminClause, Some(t.start..t.end)));
        }
    }

    // 3. Namespaced call `Word ('.' Word)+ '('` (e.g. apoc.*, db.*, gds.*),
    //    whitespace-insensitive across the dots, keyed on the trailing '('.
    if let Some(span) = find_namespaced_call(&significant, src) {
        return Err(SanitizeError::new(RejectReason::NamespacedCall, Some(span)));
    }

    // 4. The first word must be a read-entry clause.
    let first_word = significant.iter().find(|t| t.kind == TokenKind::Word);
    match first_word {
        Some(t) if in_table(t.text(src), READ_ENTRY) => {}
        Some(t) => return Err(SanitizeError::new(RejectReason::NonReadEntry, Some(t.start..t.end))),
        None => return Err(SanitizeError::new(RejectReason::NonReadEntry, None)),
    }

    Ok(())
}

/// A token that can name a namespace segment. Backtick-quoted segments count:
/// Neo4j resolves `apoc.`cypher`.doIt` to `apoc.cypher.doIt`, so treating only
/// bare words as segments would let a backtick escape the namespaced-call rule.
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
