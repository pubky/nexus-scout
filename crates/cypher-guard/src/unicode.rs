//! Unicode hardening before lexing: after NFC normalization, any non-ASCII or
//! disallowed control character outside a string/backtick region is rejected. NFC
//! (not NFKC) is used because NFC never folds a non-ASCII code point into an ASCII
//! letter, so it cannot synthesize a keyword. String/backtick contents are exempt
//! opaque data.

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
    let mut chars = s.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
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
                    // `` is an escape; must agree with the lexer's backtick handling.
                    if matches!(chars.peek(), Some((_, '`'))) {
                        chars.next();
                    } else {
                        region = Region::Code;
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backtick_region_honors_doubled_backtick_escape() {
        assert!(validate("MATCH (n:`a``b`) RETURN n").is_ok());
        assert!(validate("MATCH (n:`a``b) RETURN n").is_err());
        assert!(validate("MATCH (n:`a`Ω) RETURN n").is_err());
    }
}
