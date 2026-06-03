//! Unicode hardening that runs before lexing.
//!
//! The policy is deliberately blunt: after NFC normalization, **any** non-ASCII
//! byte or disallowed control character appearing outside a string literal or
//! backtick-quoted identifier is rejected. This single rule subsumes a whole
//! family of attacks - homoglyph keywords (`СREATE`), zero-width splitters
//! (`CRE\u{200b}ATE`), fullwidth forms (`ＣＲＥＡＴＥ`), BOM/bidi controls, and the
//! Unicode line separators `U+2028`/`U+2029`/`U+0085` - because none of them are
//! ASCII. NFC (not NFKC) is used precisely because NFC never folds a non-ASCII
//! code point into an ASCII letter, so it cannot synthesize a keyword.
//!
//! String and backtick contents are exempt: they are opaque data (bound as
//! parameters or used as identifier names), never interpreted as keywords.

use unicode_normalization::UnicodeNormalization;

use crate::error::{RejectReason, SanitizeError};

/// Normalizes `input` to NFC and verifies the ASCII-outside-strings policy.
///
/// Returns the normalized string on success.
///
/// # Errors
///
/// Returns [`RejectReason::NonAsciiKeyword`] if a non-ASCII or disallowed
/// control character appears outside a string/backtick region, and
/// [`RejectReason::Unterminated`] if a string or backtick region is left open.
pub(crate) fn normalize_and_validate(input: &str) -> Result<String, SanitizeError> {
    let normalized: String = input.nfc().collect();
    validate(&normalized)?;
    Ok(normalized)
}

#[derive(Clone, Copy)]
enum Region {
    Code,
    Single,
    Double,
    Backtick,
}

fn validate(s: &str) -> Result<(), SanitizeError> {
    let mut region = Region::Code;
    let mut escaped = false;
    let mut region_start = 0usize;

    for (idx, ch) in s.char_indices() {
        match region {
            Region::Code => match ch {
                '\'' => {
                    region = Region::Single;
                    region_start = idx;
                }
                '"' => {
                    region = Region::Double;
                    region_start = idx;
                }
                '`' => {
                    region = Region::Backtick;
                    region_start = idx;
                }
                '\t' | '\n' | '\r' => {}
                c if c.is_ascii() && !c.is_ascii_control() => {}
                _ => {
                    return Err(SanitizeError::new(
                        RejectReason::NonAsciiKeyword,
                        Some(idx..idx + ch.len_utf8()),
                    ));
                }
            },
            Region::Single | Region::Double => {
                let quote = if matches!(region, Region::Single) { '\'' } else { '"' };
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == quote {
                    region = Region::Code;
                }
            }
            Region::Backtick => {
                if ch == '`' {
                    region = Region::Code;
                }
            }
        }
    }

    if !matches!(region, Region::Code) {
        return Err(SanitizeError::new(
            RejectReason::Unterminated,
            Some(region_start..s.len()),
        ));
    }
    Ok(())
}
