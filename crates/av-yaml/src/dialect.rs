//! The plain-scalar dialect AltaVista's YAML has always been read in.
//!
//! Every committed DRM, SoS, system definition, sweep, profile and delegation file, and every
//! hash computed from one, was read by `serde_yaml` 0.9. That parser resolves an unquoted
//! (plain) scalar by a short, fixed rule list (its `visit_untagged_scalar`), and a replacement
//! parser resolves some of the same texts differently: `nULL` is a null and `tRUE` a boolean to
//! one and plain strings to the other; `1_000` is an integer to one and a string to the other;
//! `017` is a string to one and a number to the other; an integer past 128 bits is a float to
//! one; `1e400` is a string to one and an infinity to the other. A typed field can hide the
//! difference (a `String` field is given the scalar's text either way) but an untyped read (a
//! `serde_json::Value`, a `#[serde(untagged)]` enum, `flatten`) cannot, and a numeric field
//! would accept what it used to refuse.
//!
//! So this module states the old rule list ([`legacy_class`]), asks the new parser how it reads
//! the same text ([`parser_class`]), and [`check`] refuses the scalar, with the text and the
//! place, when the two answers differ. An author who meets the refusal quotes the scalar, which
//! both parsers read as a string. The rule list is written from `serde_yaml` 0.9.34's source and
//! pinned by `tests::legacy_class_table`, whose rows were produced by running that parser; the
//! scratch harness for question 246 also compared it with `serde_yaml` over every string of up to
//! four characters from a 60-character alphabet.

use std::collections::HashMap;

use serde::de::{Deserialize, Deserializer, Visitor};

use crate::Error;

/// How a plain scalar reads when no type is asked for.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Class {
    Null,
    Bool(bool),
    /// An integer anywhere in `-2^127 ..= 2^128 - 1`: a sign and a magnitude.
    Int {
        negative: bool,
        magnitude: u128,
    },
    Float(f64),
    Str,
}

impl PartialEq for Class {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Class::Null, Class::Null) | (Class::Str, Class::Str) => true,
            (Class::Bool(a), Class::Bool(b)) => a == b,
            (Class::Int { negative: n1, magnitude: m1 }, Class::Int { negative: n2, magnitude: m2 }) => n1 == n2 && m1 == m2,
            // Bit for bit, except that every NaN is the same NaN.
            (Class::Float(a), Class::Float(b)) => (a.is_nan() && b.is_nan()) || a.to_bits() == b.to_bits(),
            _ => false,
        }
    }
}

fn digits_but_not_number(s: &str) -> bool {
    let s = s.strip_prefix(['-', '+']).unwrap_or(s);
    s.len() > 1 && s.starts_with('0') && s[1..].bytes().all(|b| b.is_ascii_digit())
}

fn unsigned_int(s: &str) -> Option<u128> {
    let unpositive = s.strip_prefix('+').unwrap_or(s);
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(rest) = unpositive.strip_prefix(prefix) {
            if rest.starts_with(['+', '-']) {
                return None;
            }
            if let Ok(v) = u128::from_str_radix(rest, radix) {
                return Some(v);
            }
        }
    }
    if unpositive.starts_with(['+', '-']) || digits_but_not_number(s) {
        return None;
    }
    unpositive.parse::<u128>().ok()
}

fn negative_int(s: &str) -> Option<i128> {
    for (prefix, radix) in [("-0x", 16), ("-0o", 8), ("-0b", 2)] {
        if let Some(rest) = s.strip_prefix(prefix) {
            if let Ok(v) = i128::from_str_radix(&format!("-{rest}"), radix) {
                return Some(v);
            }
        }
    }
    if digits_but_not_number(s) {
        return None;
    }
    s.parse::<i128>().ok()
}

fn float(s: &str) -> Option<f64> {
    let unpositive = if let Some(rest) = s.strip_prefix('+') {
        if rest.starts_with(['+', '-']) {
            return None;
        }
        rest
    } else {
        s
    };
    if matches!(unpositive, ".inf" | ".Inf" | ".INF") {
        return Some(f64::INFINITY);
    }
    if matches!(s, "-.inf" | "-.Inf" | "-.INF") {
        return Some(f64::NEG_INFINITY);
    }
    if matches!(s, ".nan" | ".NaN" | ".NAN") {
        return Some(f64::NAN);
    }
    match unpositive.parse::<f64>() {
        Ok(f) if f.is_finite() => Some(f),
        _ => None,
    }
}

/// How `serde_yaml` 0.9 read the plain scalar `text` when no type was asked for.
pub(crate) fn legacy_class(text: &str) -> Class {
    if text.is_empty() || matches!(text, "null" | "Null" | "NULL" | "~") {
        return Class::Null;
    }
    match text {
        "true" | "True" | "TRUE" => return Class::Bool(true),
        "false" | "False" | "FALSE" => return Class::Bool(false),
        _ => {}
    }
    if let Some(m) = unsigned_int(text) {
        return Class::Int { negative: false, magnitude: m };
    }
    if let Some(i) = negative_int(text) {
        return Class::Int { negative: i < 0, magnitude: i.unsigned_abs() };
    }
    if !digits_but_not_number(text) {
        if let Some(f) = float(text) {
            return Class::Float(f);
        }
    }
    Class::Str
}

struct ClassVisitor;

impl<'de> Visitor<'de> for ClassVisitor {
    type Value = Class;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a scalar")
    }
    fn visit_bool<E>(self, v: bool) -> Result<Class, E> {
        Ok(Class::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Class, E> {
        Ok(Class::Int { negative: v < 0, magnitude: v.unsigned_abs() as u128 })
    }
    fn visit_u64<E>(self, v: u64) -> Result<Class, E> {
        Ok(Class::Int { negative: false, magnitude: v as u128 })
    }
    fn visit_i128<E>(self, v: i128) -> Result<Class, E> {
        Ok(Class::Int { negative: v < 0, magnitude: v.unsigned_abs() })
    }
    fn visit_u128<E>(self, v: u128) -> Result<Class, E> {
        Ok(Class::Int { negative: false, magnitude: v })
    }
    fn visit_f64<E>(self, v: f64) -> Result<Class, E> {
        Ok(Class::Float(v))
    }
    fn visit_str<E>(self, _: &str) -> Result<Class, E> {
        Ok(Class::Str)
    }
    fn visit_unit<E>(self) -> Result<Class, E> {
        Ok(Class::Null)
    }
    fn visit_none<E>(self) -> Result<Class, E> {
        Ok(Class::Null)
    }
}

impl<'de> Deserialize<'de> for Class {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Class, D::Error> {
        d.deserialize_any(ClassVisitor)
    }
}

/// How the replacement parser reads the plain scalar `text` when no type is asked for, under the
/// options this crate uses except that non-finite floats are passed through (they are compared
/// here; an untyped read of one is refused by the parse itself, see `crate::options`).
fn parser_class(text: &str) -> Result<Class, ()> {
    let mut options = crate::options();
    options.non_finite_float_policy = serde_saphyr::NonFiniteFloatPolicy::PassThrough;
    // As the only entry of a block sequence, so that `---` and `...` are scalars here as they
    // are in a value, not document markers.
    let probe = format!("- {text}\n");
    match serde_saphyr::from_str_with_options::<Vec<Class>>(&probe, options) {
        Ok(mut v) if v.len() == 1 => Ok(v.remove(0)),
        _ => Err(()),
    }
}

/// `1_000`, `0xFF_FF`: a string to `serde_yaml`, an integer to a typed integer field of the new
/// parser at every width (untyped, it reads the narrow ones as integers and `parser_class` sees
/// that; a 128-bit-wide one it reads as a string, which only a typed field then turns into an
/// integer). So the text is refused whatever its untyped class.
fn underscored_integer(text: &str) -> bool {
    text.contains('_') && matches!(legacy_class(&text.replace('_', "")), Class::Int { .. })
}

/// The place a scalar was read from, for the refusal.
#[derive(Debug, Clone, Copy)]
pub(crate) struct At {
    pub line: usize,
    pub column: usize,
}

/// Remembers the answer per distinct text; a document repeats a few hundred scalars thousands of
/// times.
#[derive(Default)]
pub(crate) struct Checker {
    seen: HashMap<String, bool>,
}

impl Checker {
    /// Refuses the plain scalar `text` when the two parsers would read it differently.
    pub(crate) fn check(&mut self, text: &str, at: At) -> Result<(), Error> {
        if let Some(&ok) = self.seen.get(text) {
            return if ok { Ok(()) } else { Err(refusal(text, at)) };
        }
        // Only a scalar without whitespace can be anything but a string to either parser, and
        // `parser_class` reads the text as a document of its own, which would change what a
        // multi-word scalar means.
        let ok = text.contains(char::is_whitespace) || {
            let legacy = legacy_class(text);
            match parser_class(text) {
                Ok(new) => legacy == new && !underscored_integer(text),
                // A text the new parser cannot read standalone (it opens with an indicator that
                // is fine inside a longer line, say) reads as a string in both unless the old
                // rules called it something else.
                Err(()) => legacy == Class::Str && !underscored_integer(text),
            }
        };
        self.seen.insert(text.to_string(), ok);
        if ok {
            Ok(())
        } else {
            Err(refusal(text, at))
        }
    }
}

fn refusal(text: &str, at: At) -> Error {
    Error::AmbiguousScalar { text: text.to_string(), line: at.line, column: at.column }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(v: i128) -> Class {
        Class::Int { negative: v < 0, magnitude: v.unsigned_abs() }
    }

    /// Rows produced by running `serde_yaml` 0.9.34 (`serde_yaml::Value`) over each text, in the
    /// question-246 scratch harness; see the module doc.
    #[test]
    fn legacy_class_table() {
        let rows: &[(&str, Class)] = &[
            ("", Class::Null),
            ("~", Class::Null),
            ("null", Class::Null),
            ("Null", Class::Null),
            ("NULL", Class::Null),
            ("nULL", Class::Str),
            ("true", Class::Bool(true)),
            ("True", Class::Bool(true)),
            ("TRUE", Class::Bool(true)),
            ("tRUE", Class::Str),
            ("false", Class::Bool(false)),
            ("yes", Class::Str),
            ("no", Class::Str),
            ("on", Class::Str),
            ("off", Class::Str),
            ("y", Class::Str),
            ("n", Class::Str),
            ("0", int(0)),
            ("-0", int(0)),
            ("+1", int(1)),
            ("-1", int(-1)),
            ("42", int(42)),
            ("0x1F", int(31)),
            ("-0x1F", int(-31)),
            ("0o17", int(15)),
            ("0b101", int(5)),
            ("017", Class::Str),
            ("0017", Class::Str),
            ("-017", Class::Str),
            ("1_000", Class::Str),
            ("9223372036854775807", int(i64::MAX as i128)),
            ("9223372036854775808", int(i64::MAX as i128 + 1)),
            ("18446744073709551615", int(u64::MAX as i128)),
            ("18446744073709551616", int(u64::MAX as i128 + 1)),
            ("-9223372036854775809", int(i64::MIN as i128 - 1)),
            ("0xFFFFFFFFFFFFFFFF", int(u64::MAX as i128)),
            ("0x10000000000000000", int(u64::MAX as i128 + 1)),
            ("1.5", Class::Float(1.5)),
            (".5", Class::Float(0.5)),
            ("5.", Class::Float(5.0)),
            ("+.5", Class::Float(0.5)),
            ("-0.0", Class::Float(-0.0)),
            ("1e3", Class::Float(1000.0)),
            ("1E3", Class::Float(1000.0)),
            ("1e-400", Class::Float(0.0)),
            ("1e400", Class::Str),
            (".inf", Class::Float(f64::INFINITY)),
            ("-.Inf", Class::Float(f64::NEG_INFINITY)),
            (".NaN", Class::Float(f64::NAN)),
            ("inf", Class::Str),
            ("nan", Class::Str),
            ("infinity", Class::Str),
            ("1_0.5", Class::Str),
            ("1.2.3", Class::Str),
            ("1:30", Class::Str),
            ("2001-12-14", Class::Str),
            ("0x", Class::Str),
            ("++1", Class::Str),
            ("hello", Class::Str),
        ];
        for (text, want) in rows {
            let got = legacy_class(text);
            assert_eq!(got, *want, "{text:?}");
        }
    }

    /// The scalars on which the two parsers disagree are refused, and the ones on which they
    /// agree are not.
    #[test]
    fn the_checker_refuses_exactly_the_disagreements() {
        let at = At { line: 3, column: 7 };
        let mut c = Checker::default();
        for ok in ["hello", "5", "-5", "0", "1.5", "1e3", "0x1F", "0o17", "0b101", "true", "TRUE", "null", "~", "", "yes", "on", "y", "no", ".inf", "-.inf", ".nan", "1.2.3", "2001-12-14", "1:30", "inf", "nan", "0.0.0.0", "a b", "1 000", "340282366920938463463374607431768211456"] {
            assert!(c.check(ok, at).is_ok(), "{ok:?} should be read the same by both parsers");
        }
        for bad in ["nULL", "tRUE", "FaLsE", "1_000", "017", "0017", "-017", "+017", "1e400", "-1e400", "1_0"] {
            let e = c.check(bad, at).expect_err(bad);
            assert!(matches!(&e, Error::AmbiguousScalar { text, line: 3, column: 7 } if text == bad), "{bad:?}: {e:?}");
        }
    }
}
