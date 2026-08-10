//! A single-pass O(n) Cypher lexer. It recognizes just enough structure for the
//! classifier to decide allow/deny on keyword-position tokens, and never mistakes
//! string/comment/backtick contents for keywords.

use crate::error::{RejectReason, SanitizeError};

/// The lexical category of a [`Token`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// An ASCII word in keyword position (keyword-vs-identifier decided by `rules`).
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

/// Lexes `src` (assumed Unicode-validated) into tokens. `*` is a
/// [`TokenKind::PathRange`] only inside a relationship bracket `-[`, never
/// multiplication or a map-projection wildcard.
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
    // Stack of open `[`, tagged true for a relationship-detail bracket (`*` is a
    // PathRange) vs a list literal (`*` is multiplication). Relationship only when
    // the bracket follows `)-` or `<-`.
    let mut bracket_stack: Vec<bool> = Vec::new();
    let mut sig1: u8 = 0;
    let mut sig2: u8 = 0;

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
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == b'`' {
                            // `` is an escaped backtick inside the quoted name; mirror
                            // the BacktickIdent and unicode-region scanners so all three
                            // agree on where the name ends.
                            if bytes.get(i + 1) == Some(&b'`') {
                                i += 2;
                                continue;
                            }
                            i += 1;
                            break;
                        }
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
                // Variable-length path range: `*`, `*n`, `*n..`, `*..m`, `*n..m`.
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
                // Lex the number to the SAME boundary Neo4j's lexer uses (hex/octal/
                // decimal/float/exponent), so any keyword fused to it (`0CREATE`,
                // `1SET`, `0xFFSET`) stays a separate Word the classifier inspects
                // rather than vanishing inside the numeric token. See `scan_number`.
                i = scan_number(bytes, i);
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
                    // Relationship bracket only for `)-[` / `)<-[`.
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
        // Track the two most recent significant bytes so the next `[` can tell a
        // relationship dash from minus.
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

/// Consumes a Cypher numeric literal starting at `start` (a digit) and returns the
/// index just past it, ending exactly where Neo4j's lexer ends the number: a hex
/// (`0x…`) or octal (`0o…`) literal, or a decimal/float with optional `.` fraction
/// and `[eE]` exponent, with `_` digit separators allowed. Letters that are not part
/// of the number are deliberately left for the next token, so a keyword glued to a
/// number (`0CREATE`, `0xFFSET`) becomes a separate Word the classifier can reject —
/// it cannot hide inside the numeric span.
fn scan_number(bytes: &[u8], start: usize) -> usize {
    let n = bytes.len();
    let mut i = start;
    if bytes[i] == b'0' && i + 1 < n {
        match bytes[i + 1] {
            b'x' | b'X' => {
                i += 2;
                while i < n && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
                    i += 1;
                }
                return i;
            }
            b'o' | b'O' => {
                i += 2;
                while i < n && (matches!(bytes[i], b'0'..=b'7') || bytes[i] == b'_') {
                    i += 1;
                }
                return i;
            }
            _ => {}
        }
    }
    // Decimal integer part.
    while i < n && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
        i += 1;
    }
    // Fraction: a `.` joins the number only when a digit follows, so `1.5` is one
    // number but `1..5` / `n.prop` keep the `.` as separate punctuation.
    if i + 1 < n && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
        i += 1;
        while i < n && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
    }
    // Exponent: `[eE]`, optionally signed, only when a digit follows (else the `e`
    // begins a separate Word, e.g. `1e` + `SET` would never share a token).
    if i < n && matches!(bytes[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < n && matches!(bytes[j], b'+' | b'-') {
            j += 1;
        }
        if j < n && bytes[j].is_ascii_digit() {
            i = j;
            while i < n && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
        }
    }
    i
}

#[cfg(test)]
mod tests {
    use super::{lex, TokenKind};

    fn kinds(src: &str) -> Vec<(TokenKind, &str)> {
        lex(src)
            .unwrap()
            .into_iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| (t.kind, &src[t.start..t.end]))
            .collect()
    }

    #[test]
    fn backtick_parameter_with_doubled_backtick_is_one_token() {
        // `$`a``b`` names a single parameter `a`b` (the `` is an escaped backtick).
        // The whole reference must lex as ONE Parameter token, matching how the
        // BacktickIdent scanner and the unicode region scanner treat `` `` `` — not
        // split at the first inner backtick.
        let src = "$`a``b`";
        assert_eq!(kinds(src), vec![(TokenKind::Parameter, "$`a``b`")]);
    }

    #[test]
    fn backtick_parameter_escape_does_not_leak_trailing_text_as_code() {
        // The doubled backtick keeps ` x` inside the name; nothing after the escape
        // may re-lex as a bare Word.
        let src = "$`a`` x` RETURN";
        let toks = kinds(src);
        assert_eq!(toks[0], (TokenKind::Parameter, "$`a`` x`"));
        assert_eq!(toks[1], (TokenKind::Word, "RETURN"));
    }

    #[test]
    fn plain_backtick_parameter_still_lexes_whole() {
        assert_eq!(kinds("$`name`"), vec![(TokenKind::Parameter, "$`name`")]);
    }

    #[test]
    fn a_keyword_glued_to_a_number_is_a_separate_word() {
        // Decimal, hex, and octal numbers must end where Neo4j ends them, so a fused
        // keyword stays its own Word (this is the write-bypass the fuzzer found).
        assert_eq!(kinds("1SET"), vec![(TokenKind::Number, "1"), (TokenKind::Word, "SET")]);
        assert_eq!(
            kinds("0xFFSET"),
            vec![(TokenKind::Number, "0xFF"), (TokenKind::Word, "SET")]
        );
        assert_eq!(
            kinds("0o17SET"),
            vec![(TokenKind::Number, "0o17"), (TokenKind::Word, "SET")]
        );
    }

    #[test]
    fn whole_numeric_literals_stay_one_number() {
        for src in ["0xFF", "0o17", "1_000", "1.5", "1.5e3", "1e-3", "42"] {
            assert_eq!(kinds(src), vec![(TokenKind::Number, src)], "{src}");
        }
    }
}
