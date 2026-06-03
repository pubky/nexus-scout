//! A single-pass, allocation-light Cypher lexer.
//!
//! The lexer classifies the (already Unicode-validated, ASCII-outside-strings)
//! input into typed [`Token`]s with byte spans. It is deliberately not a full
//! grammar: it recognizes just enough structure for the classifier to make
//! allow/deny decisions on keyword-position tokens only, and never to mistake
//! string/comment/backtick contents for keywords.
//!
//! It runs in O(n) with no backtracking, so it cannot be driven into
//! catastrophic blow-up by adversarial input.

use crate::error::{RejectReason, SanitizeError};

/// The lexical category of a [`Token`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// An ASCII word in keyword position (subject to allow/deny). Whether a word
    /// is a keyword vs an identifier is decided by the keyword table in `rules`.
    Word,
    /// A `` `quoted` `` identifier; opaque, never a keyword.
    BacktickIdent,
    /// A `'...'` or `"..."` string literal; opaque, never scanned for keywords.
    StringLit,
    /// A numeric literal.
    Number,
    /// A `$name` query parameter reference.
    Parameter,
    /// A variable-length path range token such as `*`, `*2`, `*1..5` (only
    /// emitted inside relationship brackets `[ ... ]`).
    PathRange,
    /// A `//` line comment or `/* */` block comment.
    Comment,
    /// A statement separator `;`.
    Semicolon,
    /// Punctuation or an operator (`( ) [ ] { } . , : = < > * ...`).
    Punct,
    /// A run of ASCII whitespace.
    Whitespace,
}

/// A lexed token and its byte span in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token {
    pub kind: TokenKind,
    /// The byte offset of the first character (inclusive).
    pub start: usize,
    /// The byte offset just past the last character (exclusive).
    pub end: usize,
}

impl Token {
    /// Borrows this token's text from the original source.
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
}

/// Lexes `src` into tokens.
///
/// `src` is assumed to already satisfy the Unicode policy (ASCII outside string
/// and backtick regions). Open brackets are tracked on a stack tagged
/// relationship-detail (`-[`) vs list-literal, so `*` is treated as a
/// [`TokenKind::PathRange`] only inside a relationship bracket, never as
/// multiplication (`a * b`, `count(*)`, `[a * 2]`) or a map-projection wildcard
/// (`n{.*}`).
///
/// # Errors
///
/// Returns [`RejectReason::Unterminated`] for an unterminated string literal,
/// backtick identifier, or block comment.
#[expect(
    clippy::too_many_lines,
    reason = "single cohesive lexer dispatch; splitting per-token-kind would hurt readability"
)]
pub(crate) fn lex(src: &str) -> Result<Vec<Token>, SanitizeError> {
    let bytes = src.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    // A stack of open `[`, each tagged true if it is a relationship-detail
    // bracket, where `*` is a variable-length PathRange, vs a list literal
    // `[...]`, where `*` is multiplication. A valid Cypher var-length
    // relationship is always written `)-[` or `)<-[` (a relationship needs a
    // node on its left, and nodes are parenthesized), so the bracket is tagged
    // relationship only when the two preceding significant bytes are `-` after
    // `)` or `<`. Arithmetic minus (`3 - [`, `a-[`) fails this and stays a list.
    let mut bracket_stack: Vec<bool> = Vec::new();
    let mut sig1: u8 = 0; // most recent significant byte
    let mut sig2: u8 = 0; // the one before that

    while i < bytes.len() {
        let start = i;
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => {
                while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
                    i += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Whitespace,
                    start,
                    end: i,
                });
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Comment,
                    start,
                    end: i,
                });
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut closed = false;
                while i + 1 < bytes.len() {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Err(SanitizeError::new(RejectReason::Unterminated, Some(start..bytes.len())));
                }
                tokens.push(Token {
                    kind: TokenKind::Comment,
                    start,
                    end: i,
                });
            }
            b'\'' | b'"' => {
                let quote = b;
                i += 1;
                let mut closed = false;
                while i < bytes.len() {
                    let c = bytes[i];
                    if c == b'\\' {
                        i += 2;
                        continue;
                    }
                    if c == quote {
                        i += 1;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Err(SanitizeError::new(RejectReason::Unterminated, Some(start..bytes.len())));
                }
                tokens.push(Token {
                    kind: TokenKind::StringLit,
                    start,
                    end: i,
                });
            }
            b'`' => {
                i += 1;
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == b'`' {
                        // `` is an escaped backtick inside a quoted identifier.
                        if bytes.get(i + 1) == Some(&b'`') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                if !closed {
                    return Err(SanitizeError::new(RejectReason::Unterminated, Some(start..bytes.len())));
                }
                tokens.push(Token {
                    kind: TokenKind::BacktickIdent,
                    start,
                    end: i,
                });
            }
            b'$' => {
                i += 1;
                if i < bytes.len() && bytes[i] == b'`' {
                    // $`name` parameter.
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'`' {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                } else {
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                        i += 1;
                    }
                }
                tokens.push(Token {
                    kind: TokenKind::Parameter,
                    start,
                    end: i,
                });
            }
            b'*' if matches!(bracket_stack.last(), Some(true)) => {
                // Variable-length path range: * , *n , *n.. , *..m , *n..m .
                // Only inside a relationship-detail bracket `-[ ... ]`; inside a
                // list literal `[ ... ]` a `*` is multiplication.
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::PathRange,
                    start,
                    end: i,
                });
            }
            b';' => {
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Semicolon,
                    start,
                    end: i,
                });
            }
            b'0'..=b'9' => {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_') {
                    i += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Number,
                    start,
                    end: i,
                });
            }
            _ if b.is_ascii_alphabetic() || b == b'_' => {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Word,
                    start,
                    end: i,
                });
            }
            _ => {
                if b == b'[' {
                    // Relationship detail only for `)-[` / `)<-[`: a `-` (sig1)
                    // that itself follows a node close `)` or arrow `<` (sig2).
                    let is_rel = sig1 == b'-' && (sig2 == b')' || sig2 == b'<');
                    bracket_stack.push(is_rel);
                } else if b == b']' {
                    bracket_stack.pop();
                }
                i += 1;
                tokens.push(Token {
                    kind: TokenKind::Punct,
                    start,
                    end: i,
                });
            }
        }
        // Track the two most recent significant (non-whitespace, non-comment)
        // bytes so the next `[` can distinguish a relationship dash from minus.
        if !matches!(
            tokens.last().map(|t| t.kind),
            Some(TokenKind::Whitespace | TokenKind::Comment)
        ) {
            if let Some(&last) = bytes[..i].last() {
                sig2 = sig1;
                sig1 = last;
            }
        }
    }

    Ok(tokens)
}
