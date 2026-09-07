//! Hand-written lexer for ADR-005 sec 6's EBNF (no third-party parser/lexer crate, per the
//! task's dependency rule).
//!
//! ## Lexing `number [unit]` as one token
//!
//! The grammar's `primary := number [unit]` and `time := ... | number 's'` both attach a unit
//! suffix directly to a numeric literal, with no operator between them. Several unit tokens
//! contain characters that are *also* expr operators (`m/s` contains `/`, `N*m` contains `*`),
//! so this lexer resolves the ambiguity the same way the ADR's own worked examples read
//! naturally: a unit suffix must follow its number **immediately** (at most one space, and no
//! space at all *inside* a multi-character unit token like `m/s`). `100 m/s` is the single
//! unit `m/s`; `100 m / s` (spaced around the `/`) is `(100 m) / (ref "s")`, a division by
//! whatever `s` refers to -- not a unit, since a real unit suffix never has embedded
//! whitespace. This convention is an ordinary lexical decision the EBNF (a grammar, not a
//! lexer spec) leaves unstated; see `crate::expr::units`'s module doc comment for the token
//! vocabulary itself.
//!
//! Matching is longest-candidate-first (`crate::expr::units::UNIT_TOKENS`'s declared order)
//! with a word-boundary check after the match (the next character, if any, must not be
//! alphanumeric/`_`) -- so `5 seconds` does not falsely consume `s` out of `seconds` and leave
//! `econds` behind; it instead lexes as `Number(5, None)` followed by `Ident("seconds")`,
//! which the parser then rejects as unexpected trailing input (an honest parse error, not a
//! silent misparse).

use crate::expr::units::UNIT_TOKENS;
use crate::expr::error::ExprError;

#[derive(Debug, Clone, PartialEq)]
pub enum TokKind {
    /// The raw unit suffix text (one of `crate::expr::units::UNIT_TOKENS`'s left-hand sides),
    /// already validated to exist -- `None` when no unit suffix followed the number.
    Number(f64, Option<&'static str>),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    Comma,
    At,
    Dot,
    // -- Comparison operators (ADR-005 amendment 2026-09-02's `comparison` level). --
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    Ne,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokKind,
    pub pos: usize,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Try every [`UNIT_TOKENS`] candidate at `src[pos..]` (after skipping at most one space),
/// longest declared order first, requiring an exact contiguous match with a word boundary
/// immediately after. Returns `(token_text, end_byte_offset)`.
fn try_scan_unit_suffix(src: &str, pos: usize) -> Option<(&'static str, usize)> {
    let after_space = if src[pos..].starts_with(' ') { pos + 1 } else { pos };
    let rest = &src[after_space..];
    for (tok, _unit) in UNIT_TOKENS {
        if let Some(remainder) = rest.strip_prefix(tok) {
            let boundary_ok = remainder.chars().next().is_none_or(|c| !is_ident_continue(c) && c != '/' && c != '*' && c != '^');
            if boundary_ok {
                return Some((tok, after_space + tok.len()));
            }
        }
    }
    None
}

/// Scan one decimal-literal number starting at `pos` (`src.as_bytes()[pos]` is a digit or
/// `.`). `number := decimal literal` -- digits, an optional `.` fraction, an optional
/// exponent (`[eE][+-]?digits`), matching ordinary `f64::from_str` syntax (never a sign here;
/// unary `-` is a separate grammar token, handled by the parser, not folded into the literal).
fn scan_number(src: &str, start: usize) -> (f64, usize) {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        if j < bytes.len() && bytes[j].is_ascii_digit() {
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            i = j;
        }
    }
    let text = &src[start..i];
    (text.parse::<f64>().expect("scan_number only consumes a syntactically valid f64 literal"), i)
}

pub fn lex(src: &str) -> Result<Vec<Token>, ExprError> {
    let mut tokens = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        match c {
            '+' => {
                tokens.push(Token { kind: TokKind::Plus, pos: start });
                i += 1;
            }
            '-' => {
                tokens.push(Token { kind: TokKind::Minus, pos: start });
                i += 1;
            }
            '*' => {
                tokens.push(Token { kind: TokKind::Star, pos: start });
                i += 1;
            }
            '/' => {
                tokens.push(Token { kind: TokKind::Slash, pos: start });
                i += 1;
            }
            '(' => {
                tokens.push(Token { kind: TokKind::LParen, pos: start });
                i += 1;
            }
            ')' => {
                tokens.push(Token { kind: TokKind::RParen, pos: start });
                i += 1;
            }
            ',' => {
                tokens.push(Token { kind: TokKind::Comma, pos: start });
                i += 1;
            }
            '@' => {
                tokens.push(Token { kind: TokKind::At, pos: start });
                i += 1;
            }
            '<' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    tokens.push(Token { kind: TokKind::Le, pos: start });
                    i += 2;
                } else {
                    tokens.push(Token { kind: TokKind::Lt, pos: start });
                    i += 1;
                }
            }
            '>' => {
                if bytes.get(i + 1) == Some(&b'=') {
                    tokens.push(Token { kind: TokKind::Ge, pos: start });
                    i += 2;
                } else {
                    tokens.push(Token { kind: TokKind::Gt, pos: start });
                    i += 1;
                }
            }
            '=' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token { kind: TokKind::EqEq, pos: start });
                i += 2;
            }
            '!' if bytes.get(i + 1) == Some(&b'=') => {
                tokens.push(Token { kind: TokKind::Ne, pos: start });
                i += 2;
            }
            '.' if !(i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) => {
                // A '.' not immediately followed by a digit is the ref path separator; a '.'
                // that *is* followed by a digit is the start of a fractional literal like
                // ".5", handled by scan_number below.
                tokens.push(Token { kind: TokKind::Dot, pos: start });
                i += 1;
            }
            c if c.is_ascii_digit() || c == '.' => {
                let (value, end) = scan_number(src, start);
                let unit = try_scan_unit_suffix(src, end);
                match unit {
                    Some((tok, unit_end)) => {
                        tokens.push(Token { kind: TokKind::Number(value, Some(tok)), pos: start });
                        i = unit_end;
                    }
                    None => {
                        tokens.push(Token { kind: TokKind::Number(value, None), pos: start });
                        i = end;
                    }
                }
            }
            c if is_ident_start(c) => {
                let mut j = i + 1;
                while j < bytes.len() && is_ident_continue(bytes[j] as char) {
                    j += 1;
                }
                tokens.push(Token { kind: TokKind::Ident(src[start..j].to_string()), pos: start });
                i = j;
            }
            other => {
                return Err(ExprError::Lex { pos: start, message: format!("unrecognized character {other:?}") });
            }
        }
    }
    tokens.push(Token { kind: TokKind::Eof, pos: bytes.len() });
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lexes_a_bare_number_as_dimensionless() {
        assert_eq!(kinds("42"), vec![TokKind::Number(42.0, None), TokKind::Eof]);
    }

    #[test]
    fn lexes_a_number_with_a_simple_unit() {
        assert_eq!(kinds("100 m"), vec![TokKind::Number(100.0, Some("m")), TokKind::Eof]);
        assert_eq!(kinds("100m"), vec![TokKind::Number(100.0, Some("m")), TokKind::Eof]);
    }

    #[test]
    fn lexes_a_compound_unit_only_when_contiguous() {
        assert_eq!(kinds("10 m/s"), vec![TokKind::Number(10.0, Some("m/s")), TokKind::Eof]);
        // Spaced around the '/': not a unit -- (10 m) / (ref "s").
        assert_eq!(
            kinds("10 m / s"),
            vec![TokKind::Number(10.0, Some("m")), TokKind::Slash, TokKind::Ident("s".to_string()), TokKind::Eof]
        );
    }

    #[test]
    fn word_boundary_stops_a_false_partial_match() {
        assert_eq!(kinds("5 seconds"), vec![TokKind::Number(5.0, None), TokKind::Ident("seconds".to_string()), TokKind::Eof]);
    }

    #[test]
    fn lexes_dotted_refs_calls_and_at_time() {
        assert_eq!(
            kinds("entity.leo.pos_x@end"),
            vec![
                TokKind::Ident("entity".to_string()),
                TokKind::Dot,
                TokKind::Ident("leo".to_string()),
                TokKind::Dot,
                TokKind::Ident("pos_x".to_string()),
                TokKind::At,
                TokKind::Ident("end".to_string()),
                TokKind::Eof
            ]
        );
        assert_eq!(
            kinds("mean(x)"),
            vec![TokKind::Ident("mean".to_string()), TokKind::LParen, TokKind::Ident("x".to_string()), TokKind::RParen, TokKind::Eof]
        );
    }

    #[test]
    fn lexes_a_leading_dot_fraction() {
        assert_eq!(kinds(".5"), vec![TokKind::Number(0.5, None), TokKind::Eof]);
    }

    #[test]
    fn refuses_an_unrecognized_character() {
        let err = lex("3 % 2").unwrap_err();
        assert!(matches!(err, ExprError::Lex { pos: 2, .. }), "{err:?}");
    }

    #[test]
    fn lexes_every_comparison_operator_including_the_two_char_forms() {
        assert_eq!(kinds("<"), vec![TokKind::Lt, TokKind::Eof]);
        assert_eq!(kinds("<="), vec![TokKind::Le, TokKind::Eof]);
        assert_eq!(kinds(">"), vec![TokKind::Gt, TokKind::Eof]);
        assert_eq!(kinds(">="), vec![TokKind::Ge, TokKind::Eof]);
        assert_eq!(kinds("=="), vec![TokKind::EqEq, TokKind::Eof]);
        assert_eq!(kinds("!="), vec![TokKind::Ne, TokKind::Eof]);
        assert_eq!(kinds("range(a, b) < 100 m"), {
            let mut v = vec![
                TokKind::Ident("range".to_string()),
                TokKind::LParen,
                TokKind::Ident("a".to_string()),
                TokKind::Comma,
                TokKind::Ident("b".to_string()),
                TokKind::RParen,
                TokKind::Lt,
                TokKind::Number(100.0, Some("m")),
            ];
            v.push(TokKind::Eof);
            v
        });
    }
}
