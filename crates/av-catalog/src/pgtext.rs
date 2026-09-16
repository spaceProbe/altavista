//! PostgreSQL TEXT-format encoding/decoding for the two wire shapes `src/client.rs`'s `Param`
//! enum has no dedicated variant for: arrays (`text[]`/`double precision[]`) and `bytea`.
//! `crate::client`'s module doc explains why this crate speaks every parameter and every
//! result column in TEXT format (`Param::Text(String)` is the one variant every non-scalar
//! value this crate ever sends or reads is built from) -- this module is where an array or a
//! `bytea` value's own TEXT-format literal grammar is encoded and decoded, once, rather than
//! reimplemented at each of `crate::model`/`crate::query`'s own call sites.
//!
//! # No I/O in this module
//!
//! Every function here is pure (`&str`/`&[u8]`/`&[String]`/`&[f64]` in, `String`/`Vec<_>`/
//! `Result<_, CatalogError>` out) -- the same "no I/O, exhaustively testable" discipline
//! `src/protocol.rs`'s own module doc states for the reason this crate's wire framing is
//! trustworthy: every edge case here (an empty array, a quoted element containing a comma or a
//! backslash, an odd-length hex string) is a plain unit test below, none of them needing a
//! socket or a running server.

use crate::error::CatalogError;

// ------------------------------------------------------------------------------------------
// bytea: PostgreSQL's hex TEXT-format ("\x" + lower-case hex), the default `bytea_output`
// since PostgreSQL 9.0 and the format `services/catalog/IMAGE_DIGEST.md`'s measured server
// (17.11) uses. This crate never sets `bytea_output` itself, so encoding always WRITES this
// format and decoding only ever needs to READ it -- the legacy escape format ("\\NNN" octal
// per non-printable byte) is a server-side output option this crate's own connections never
// request and so never need to parse.
// ------------------------------------------------------------------------------------------

/// Encodes `bytes` as a `bytea` TEXT-format literal: `"\x"` followed by lower-case hex, two
/// characters per byte. Infallible -- every `&[u8]` has exactly one such encoding.
pub fn encode_bytea(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("\\x");
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Decodes a `bytea` TEXT-format value PostgreSQL sent back (hex format -- see this module's
/// own doc for why the escape format is never handled here). `context` names the column, for
/// [`CatalogError::ColumnParse`] (this module's one error variant, shared with `crate::client::
/// Row`'s own typed accessors -- see that variant's own doc for why one shape serves both).
pub fn decode_bytea(raw: &str, context: &'static str) -> Result<Vec<u8>, CatalogError> {
    let hex = raw.strip_prefix("\\x").ok_or_else(|| CatalogError::ColumnParse { column: context.to_string(), expected: "a \\x-prefixed hex bytea value", raw: raw.to_string() })?;
    if hex.len() % 2 != 0 {
        return Err(CatalogError::ColumnParse { column: context.to_string(), expected: "a \\x-prefixed hex bytea value with an even number of hex digits", raw: raw.to_string() });
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16);
        let lo = (bytes[i + 1] as char).to_digit(16);
        match (hi, lo) {
            (Some(hi), Some(lo)) => out.push((hi as u8) << 4 | lo as u8),
            _ => return Err(CatalogError::ColumnParse { column: context.to_string(), expected: "a \\x-prefixed hex bytea value (valid hex digits only)", raw: raw.to_string() }),
        }
        i += 2;
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------
// Arrays (`text[]`/`double precision[]`): PostgreSQL's own array literal grammar, `{elem,
// elem, ...}`, elements optionally double-quoted with `\"`/`\\` escapes. This module always
// WRITES every element double-quoted (simplest correct encoding -- PostgreSQL accepts a
// quoted form for any element, quoting need never be "if this element needs it") and READS
// the general grammar (quoted or bare elements, since a value this crate never wrote itself --
// e.g. read back after some other tool inserted a row -- is not guaranteed to be quoted the
// same way).
// ------------------------------------------------------------------------------------------

/// Encodes `values` as a `text[]` TEXT-format literal, every element double-quoted with `"`/`\`
/// escaped (`"` -> `\"`, `\` -> `\\`) -- correct for any element content, including one
/// containing a comma, a brace, or another quote. `{}` for an empty slice (PostgreSQL's own
/// empty-array literal).
pub fn encode_text_array(values: &[String]) -> String {
    let mut s = String::from("{");
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push('"');
        for c in v.chars() {
            if c == '"' || c == '\\' {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
    }
    s.push('}');
    s
}

/// Encodes `values` as a `double precision[]` TEXT-format literal. Rust's `f64` `Display` is
/// shortest-round-trip-accurate (`crate::client::Param::to_text`'s own doc makes the identical
/// claim for a scalar `F64` parameter), so no element here is ever quoted -- a bare numeral
/// (or `Infinity`/`-Infinity`/`NaN`, which PostgreSQL's own `float8in` accepts unquoted) is
/// always a valid, unambiguous array element with no comma/brace/quote of its own to escape.
pub fn encode_f64_array(values: &[f64]) -> String {
    let mut s = String::from("{");
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&v.to_string());
    }
    s.push('}');
    s
}

/// Parses one PostgreSQL array literal's `{...}` body into its raw element strings (quotes
/// consumed and escapes resolved for a quoted element; returned verbatim for a bare one) --
/// shared by [`parse_pg_text_array`] and [`parse_pg_f64_array`], which differ only in what
/// they do with each resulting `String`. `context` names the column, for
/// [`CatalogError::ColumnParse`] on a value with no matching `{`/`}` pair.
fn parse_pg_array_elements(raw: &str, context: &'static str) -> Result<Vec<String>, CatalogError> {
    let trimmed = raw.trim();
    let inner = trimmed
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .ok_or_else(|| CatalogError::ColumnParse { column: context.to_string(), expected: "a {...} PostgreSQL array literal", raw: raw.to_string() })?;
    if inner.is_empty() {
        return Ok(Vec::new());
    }
    let mut elems = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => in_quotes = !in_quotes,
            '\\' if in_quotes => {
                if let Some(next) = chars.next() {
                    current.push(next);
                } else {
                    return Err(CatalogError::ColumnParse { column: context.to_string(), expected: "a well-formed PostgreSQL array literal (no trailing backslash inside a quoted element)", raw: raw.to_string() });
                }
            }
            ',' if !in_quotes => {
                elems.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    if in_quotes {
        return Err(CatalogError::ColumnParse { column: context.to_string(), expected: "a well-formed PostgreSQL array literal (an opening quote with no matching close)", raw: raw.to_string() });
    }
    elems.push(current);
    Ok(elems)
}

/// Parses a `text[]` TEXT-format value (this crate always reads/writes double-quoted elements
/// -- see this module's own doc -- but accepts a bare, unquoted element too, since a value
/// this crate did not itself write is not guaranteed to be quoted the same way).
pub fn parse_pg_text_array(raw: &str, context: &'static str) -> Result<Vec<String>, CatalogError> {
    parse_pg_array_elements(raw, context)
}

/// Parses a `double precision[]` TEXT-format value: [`parse_pg_array_elements`]'s raw element
/// strings, each further parsed as `f64` (accepting PostgreSQL's own unquoted `Infinity`/
/// `-Infinity`/`NaN` spellings, same as [`crate::client::Row::get_f64`]).
pub fn parse_pg_f64_array(raw: &str, context: &'static str) -> Result<Vec<f64>, CatalogError> {
    parse_pg_array_elements(raw, context)?
        .into_iter()
        .map(|e| e.parse::<f64>().map_err(|_| CatalogError::ColumnParse { column: context.to_string(), expected: "an array of f64 elements", raw: raw.to_string() }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- bytea -------------------------------------------------------------------------

    #[test]
    fn bytea_round_trips_arbitrary_bytes() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        let encoded = encode_bytea(&bytes);
        assert!(encoded.starts_with("\\x"));
        assert_eq!(decode_bytea(&encoded, "test").unwrap(), bytes);
    }

    #[test]
    fn bytea_empty_round_trips() {
        assert_eq!(encode_bytea(&[]), "\\x");
        assert_eq!(decode_bytea("\\x", "test").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn bytea_known_answer() {
        // "hi" == 0x68 0x69.
        assert_eq!(encode_bytea(b"hi"), "\\x6869");
        assert_eq!(decode_bytea("\\x6869", "test").unwrap(), b"hi".to_vec());
    }

    #[test]
    fn bytea_decode_rejects_missing_prefix() {
        let err = decode_bytea("6869", "somecol").unwrap_err();
        match err {
            CatalogError::ColumnParse { column, .. } => assert_eq!(column, "somecol"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn bytea_decode_rejects_odd_length_hex() {
        assert!(decode_bytea("\\x686", "test").is_err());
    }

    #[test]
    fn bytea_decode_rejects_non_hex_digits() {
        assert!(decode_bytea("\\xzz", "test").is_err());
    }

    // -- text[] ------------------------------------------------------------------------

    #[test]
    fn text_array_round_trips_empty() {
        let encoded = encode_text_array(&[]);
        assert_eq!(encoded, "{}");
        assert_eq!(parse_pg_text_array(&encoded, "test").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn text_array_round_trips_simple_values() {
        let values = vec!["SP-EXPT".to_string(), "REL-TO//FVEY".to_string()];
        let encoded = encode_text_array(&values);
        assert_eq!(parse_pg_text_array(&encoded, "test").unwrap(), values);
    }

    #[test]
    fn text_array_round_trips_values_with_commas_quotes_and_backslashes() {
        let values = vec!["a,b".to_string(), "she said \"hi\"".to_string(), "back\\slash".to_string(), "".to_string()];
        let encoded = encode_text_array(&values);
        assert_eq!(parse_pg_text_array(&encoded, "test").unwrap(), values);
    }

    #[test]
    fn text_array_parses_a_bare_unquoted_element() {
        // A value this crate did not itself write -- PostgreSQL's own output for simple
        // elements omits quotes when none are needed.
        assert_eq!(parse_pg_text_array("{UNCLASSIFIED,CUI}", "test").unwrap(), vec!["UNCLASSIFIED".to_string(), "CUI".to_string()]);
    }

    #[test]
    fn text_array_decode_rejects_missing_braces() {
        assert!(parse_pg_text_array("a,b", "test").is_err());
    }

    #[test]
    fn text_array_decode_rejects_unterminated_quote() {
        assert!(parse_pg_text_array("{\"a}", "test").is_err());
    }

    // -- double precision[] --------------------------------------------------------------

    #[test]
    fn f64_array_round_trips_empty() {
        let encoded = encode_f64_array(&[]);
        assert_eq!(encoded, "{}");
        assert_eq!(parse_pg_f64_array(&encoded, "test").unwrap(), Vec::<f64>::new());
    }

    #[test]
    fn f64_array_round_trips_values() {
        let values = vec![-179.5, 0.0, 42.125, 90.0];
        let encoded = encode_f64_array(&values);
        assert_eq!(parse_pg_f64_array(&encoded, "test").unwrap(), values);
    }

    #[test]
    fn f64_array_decode_rejects_non_numeric_element() {
        assert!(parse_pg_f64_array("{1.0,not-a-number}", "test").is_err());
    }
}
