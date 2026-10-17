//! `av-yaml`: the one place AltaVista parses YAML (question 246, chartered by question 250(b)).
//!
//! AltaVista read its YAML (DRMs, SoS files, system definitions, sweep files, profiles,
//! delegations, the gateway's auth configuration, the Kubernetes kubeconfig) with `serde_yaml`
//! 0.9, whose C-port parser `unsafe-libyaml` is unmaintained (`RUSTSEC-2024-0449`). This crate
//! owns the replacement, `serde-saphyr`, and the policy that keeps every committed file reading
//! as the same values:
//!
//! ```
//! #[derive(serde::Deserialize)]
//! struct Doc { name: String, count: u32 }
//! let doc: Doc = av_yaml::from_str("name: sat-1\ncount: 3\n")?;
//! assert_eq!((doc.name.as_str(), doc.count), ("sat-1", 3));
//! # Ok::<(), av_yaml::Error>(())
//! ```
//!
//! # The policy: the same value, or a typed refusal, never a different value
//!
//! The replacement parser is looser than `serde_yaml` in places where looseness changes a value,
//! so [`from_str`] refuses, with an [`Error`] naming the line and column, every input on which the
//! two would disagree (the table of behaviours is recorded in `docs/p5-plan.md`'s copyleft and YAML
//! round status, question 250, and each row has a unit test in the module that owns it):
//!
//! | input | `serde_yaml` 0.9 | here |
//! |---|---|---|
//! | anchors, aliases | expanded | expanded; alias bombs and recursion refused ([`Error::LimitExceeded`]) |
//! | merge key `<<` | an ordinary key | [`Error::MergeKey`] |
//! | explicit tag (`!Foo`, `!!str`, ...) | ignored typed, kept untyped | [`Error::ExplicitTag`] |
//! | duplicate mapping key | last wins in a map, an error in a struct | [`Error::DuplicateKey`] |
//! | `0x1F`, `0o17`, `0b101`, `+1`, `1e3`, `.inf`, `-.inf`, `.nan` | numbers | the same numbers |
//! | `017`, `1_000`, `1e400`, an integer past 128 bits, `nULL`, `tRUE` | strings (or a float) | [`Error::AmbiguousScalar`] |
//! | `yes`/`no`/`on`/`off`/`y`/`n` | strings | strings (a boolean field refuses them, as before) |
//! | `~`, `null`, `Null`, `NULL`, an empty value | null | null |
//! | more than one document | refused | [`Error::MultipleDocuments`] |
//! | `%YAML` directive | accepted | [`Error::Directive`] |
//! | a raw U+0085, U+2028 or U+2029 | a line break (folded to a space) | [`Error::UnicodeLineBreak`] |
//! | a quoted or block scalar in a number or boolean field (`"5"`, `'true'`, `" 5 "`) | a type error | read as the number or boolean: an input that used to be refused loads; no document that loaded changes value |
//! | a block scalar last in an input with no final newline | text as written | [`Error::BlockScalarAtEnd`] |
//!
//! Nothing else is read differently that this crate knows of: the question-246 harness compared
//! the two parsers over every committed YAML file and a differential corpus (see the round's
//! report), and the units here pin the table.

#![forbid(unsafe_code)]

mod dialect;
mod scan;

use serde::de::DeserializeOwned;

/// Why a YAML text was not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The text is not YAML, or does not fit the type asked for (an unknown or missing field, a
    /// value of the wrong type). The message carries the line and column.
    #[error("{0}")]
    Parse(String),
    /// More than one document in the text.
    #[error("the text holds more than one YAML document; exactly one is read")]
    MultipleDocuments,
    /// A mapping lists the same key twice.
    #[error("{0}")]
    DuplicateKey(String),
    /// An explicit tag (`!Foo`, `!!str`, `!!binary`, ...).
    #[error("line {line} column {column}: explicit YAML tag `{tag}` is not read; remove it (quote a value that must stay a string)")]
    ExplicitTag { tag: String, line: usize, column: usize },
    /// A plain `<<` mapping key.
    #[error("line {line} column {column}: YAML merge key `<<` is not read; write the merged keys out")]
    MergeKey { line: usize, column: usize },
    /// A `%YAML` directive.
    #[error("line {line} column {column}: the `%YAML` directive is not read; remove it")]
    Directive { line: usize, column: usize },
    /// A plain scalar that this crate's parser and `serde_yaml` 0.9 read as different values.
    #[error("line {line} column {column}: the plain scalar `{text}` is read differently by the parser AltaVista's files were written for; quote it (\"{text}\") to keep it a string")]
    AmbiguousScalar { text: String, line: usize, column: usize },
    /// A raw U+0085, U+2028 or U+2029: a line break to one parser, an ordinary character to the other.
    #[error("line {line} column {column}: the character U+{:04X} is a line break to YAML 1.1 readers and an ordinary character to YAML 1.2 readers; write it as an escape in a double-quoted scalar (\"\\N\", \"\\L\", \"\\P\")", *character as u32)]
    UnicodeLineBreak { character: char, line: usize, column: usize },
    /// A block scalar (`|` or `>`) is the last thing in a text that does not end in a line break.
    #[error("the text ends inside a block scalar (`|` or `>`) with no final newline; add the newline")]
    BlockScalarAtEnd,
    /// Alias expansion, nesting or size beyond the limits (an alias bomb, recursion).
    #[error("{0}")]
    LimitExceeded(String),
}

/// The options every read uses. Anything not named here is the parser's default; the ones named
/// close a looseness (`crate::dialect` and `crate::scan` close the rest).
pub(crate) fn options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        // `yes`/`no`/`on`/`off`/`y`/`n` are strings, as `serde_yaml` read them.
        strict_booleans: true,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        // Also refused by `scan` with a position; this is the backstop.
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        reject_unsupported_tags: true,
        // An untyped read of `.inf`/`.nan` would otherwise become a JSON null.
        non_finite_float_policy: serde_saphyr::NonFiniteFloatPolicy::Reject,
        // One-line messages: the text is the caller's, not ours to echo back.
        with_snippet: false,
    }
}

/// Reads the single YAML document in `text` as a `T`. See the crate doc for what is refused.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, Error> {
    scan::scan(text)?;
    serde_saphyr::from_str_with_options::<T>(text, options()).map_err(convert)
}

fn convert(error: serde_saphyr::Error) -> Error {
    let error = error.without_snippet();
    let message = error.to_string();
    let message = message.strip_prefix("error: ").unwrap_or(&message).to_string();
    match kind(error) {
        Kind::MultipleDocuments => Error::MultipleDocuments,
        Kind::DuplicateKey => Error::DuplicateKey(message),
        Kind::Limit => Error::LimitExceeded(message),
        Kind::Other => Error::Parse(message),
    }
}

enum Kind {
    MultipleDocuments,
    DuplicateKey,
    Limit,
    Other,
}

/// What the parser's error is, looking through the wrapper it puts around an error found while
/// replaying an alias.
fn kind(error: &serde_saphyr::Error) -> Kind {
    use serde_saphyr::Error as E;
    match error {
        E::MultipleDocuments { .. } => Kind::MultipleDocuments,
        E::DuplicateMappingKey { .. } => Kind::DuplicateKey,
        E::Budget { .. } | E::AliasReplayLimitExceeded { .. } | E::AliasExpansionLimitExceeded { .. } | E::AliasReplayStackDepthExceeded { .. } | E::AliasReplayCounterOverflow { .. } | E::RecursiveReferencesRequireWeakTypes { .. } => Kind::Limit,
        E::AliasError { error, .. } => kind(error),
        _ => Kind::Other,
    }
}

#[cfg(test)]
mod tests {
    //! One test per row of the crate doc's table. The "old" column of each comment is what
    //! `serde_yaml` 0.9.34 did, measured by the question-246 harness (`cases.txt`).
    use super::*;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Doc {
        #[serde(default)]
        a: Option<i64>,
        #[serde(default)]
        s: Option<String>,
        #[serde(default)]
        b: Option<bool>,
        #[serde(default)]
        f: Option<f64>,
        #[serde(default)]
        u: Option<u64>,
    }

    fn val(text: &str) -> Result<Value, Error> {
        from_str::<Value>(text)
    }

    #[test]
    fn anchors_and_aliases_are_expanded() {
        assert_eq!(val("a: &x 5\nb: *x\n").unwrap(), json!({"a": 5, "b": 5}));
        assert_eq!(val("a: &m {p: 1, q: [1, 2]}\nb: *m\n").unwrap(), json!({"a": {"p": 1, "q": [1, 2]}, "b": {"p": 1, "q": [1, 2]}}));
        assert_eq!(val("&k a: 1\nb: *k\n").unwrap(), json!({"a": 1, "b": "a"}));
        assert!(matches!(val("a: *nope\n"), Err(Error::Parse(_))));
    }

    #[test]
    fn an_alias_bomb_and_a_recursive_alias_are_refused() {
        // serde_yaml: "repetition limit exceeded" / "recursion limit exceeded".
        let mut bomb = String::from("a0: &a0 [\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\"]\n");
        for i in 1..10 {
            let prev = format!("*a{}", i - 1);
            bomb.push_str(&format!("a{i}: &a{i} [{}]\n", vec![prev; 9].join(",")));
        }
        assert!(matches!(val(&bomb), Err(Error::LimitExceeded(_))), "{:?}", val(&bomb));
        assert!(val("a: &a [*a]\n").is_err());
    }

    #[test]
    fn a_merge_key_is_refused_where_serde_yaml_kept_it_as_an_ordinary_key() {
        let e = val("base: &b {x: 1}\nd:\n  <<: *b\n  y: 3\n").unwrap_err();
        assert_eq!(e, Error::MergeKey { line: 3, column: 3 });
        // A quoted "<<" is an ordinary key to both parsers.
        assert_eq!(val("d: {\"<<\": 1}\n").unwrap(), json!({"d": {"<<": 1}}));
        // As a value it is a string.
        assert_eq!(val("d: <<\n").unwrap(), json!({"d": "<<"}));
    }

    #[test]
    fn every_explicit_tag_is_refused() {
        for (text, tag) in [("a: !Foo 5\n", "!Foo"), ("a: !!str 5\n", "tag:yaml.org,2002:str"), ("a: !!binary aGVsbG8=\n", "binary"), ("a: !!null x\n", "null"), ("a: !Foo {x: 1}\n", "!Foo"), ("a: !Foo [1]\n", "!Foo"), ("a: !!seq {x: 1}\n", "seq"), ("a: !!set {x, y}\n", "set"), ("a: !\n", "!")] {
            match val(text) {
                Err(Error::ExplicitTag { tag: t, line: 1, .. }) => assert!(t.contains(tag), "{text:?}: {t}"),
                other => panic!("{text:?}: {other:?}"),
            }
        }
        // a tag on the root, and a tag directive that names one
        assert!(matches!(val("--- !Foo\na: 1\n"), Err(Error::ExplicitTag { .. })));
        assert!(matches!(val("%TAG !e! tag:example.com,2000:\n---\na: !e!foo 1\n"), Err(Error::ExplicitTag { .. })));
    }

    #[test]
    fn a_duplicate_mapping_key_is_refused() {
        // serde_yaml: kept both in a map, "duplicate field" in a struct.
        assert!(matches!(val("a: 1\na: 2\n"), Err(Error::DuplicateKey(_))));
        assert!(matches!(val("m:\n  k: 1\n  k: 2\n"), Err(Error::DuplicateKey(_))));
        assert!(matches!(val("0x10: a\n16: b\n"), Err(Error::DuplicateKey(_))));
        assert!(matches!(from_str::<BTreeMap<String, i64>>("x: 1\nx: 2\n"), Err(Error::DuplicateKey(_))));
        assert!(matches!(from_str::<Doc>("a: 1\na: 2\n"), Err(Error::DuplicateKey(_))));
    }

    #[test]
    fn numeric_forms_are_read_as_serde_yaml_read_them() {
        let int = |t: &str| from_str::<Doc>(&format!("a: {t}\n")).map(|d| d.a.unwrap());
        assert_eq!(int("0x1F").unwrap(), 31);
        assert_eq!(int("-0x1F").unwrap(), -31);
        assert_eq!(int("0o17").unwrap(), 15);
        assert_eq!(int("0b101").unwrap(), 5);
        assert_eq!(int("+1").unwrap(), 1);
        assert_eq!(int("-1").unwrap(), -1);
        assert_eq!(int("9223372036854775807").unwrap(), i64::MAX);
        assert_eq!(int("-9223372036854775808").unwrap(), i64::MIN);
        assert!(matches!(int("9223372036854775808"), Err(Error::Parse(_))));
        assert!(matches!(int("1e3"), Err(Error::Parse(_))), "a float is not an integer field");
        let u = |t: &str| from_str::<Doc>(&format!("u: {t}\n")).map(|d| d.u.unwrap());
        assert_eq!(u("18446744073709551615").unwrap(), u64::MAX);
        assert_eq!(u("0xFFFFFFFFFFFFFFFF").unwrap(), u64::MAX);
        assert!(u("-1").is_err());
        assert!(u("18446744073709551616").is_err());
        let f = |t: &str| from_str::<Doc>(&format!("f: {t}\n")).map(|d| d.f.unwrap());
        assert_eq!(f("1e3").unwrap(), 1000.0);
        assert_eq!(f("1E3").unwrap(), 1000.0);
        assert_eq!(f(".5").unwrap(), 0.5);
        assert_eq!(f("5.").unwrap(), 5.0);
        assert_eq!(f("+.5").unwrap(), 0.5);
        assert_eq!(f("-0.0").unwrap().to_bits(), (-0.0f64).to_bits());
        assert_eq!(f("1e-400").unwrap(), 0.0);
        assert_eq!(f(".inf").unwrap(), f64::INFINITY);
        assert_eq!(f("-.inf").unwrap(), f64::NEG_INFINITY);
        assert!(f(".nan").unwrap().is_nan());
        assert_eq!(f("7").unwrap(), 7.0, "an integer text is a valid float field");
        // untyped, the same numbers
        assert_eq!(val("a: [0x1F, 0o17, 0b101, +1, 1e3, 1.5, -0.0]\n").unwrap(), json!({"a": [31, 15, 5, 1, 1000.0, 1.5, -0.0]}));
        assert_eq!(val("a: [9223372036854775808, 18446744073709551615, -9223372036854775808]\n").unwrap(), json!({"a": [9223372036854775808u64, 18446744073709551615u64, -9223372036854775808i64]}));
    }

    #[test]
    fn an_untyped_read_of_a_non_finite_float_is_refused_not_turned_into_null() {
        // serde_yaml: .inf and .nan became a float, which serde_json::Value turns into null.
        for t in ["a: .inf\n", "a: -.inf\n", "a: .nan\n"] {
            assert!(matches!(val(t), Err(Error::Parse(_))), "{t:?}: {:?}", val(t));
        }
    }

    #[test]
    fn scalars_the_two_parsers_read_differently_are_refused_with_the_text() {
        // serde_yaml: strings (017, 1_000, 1e400, nULL, tRUE) or a float (past 128 bits).
        for (text, scalar) in [("a: 017\n", "017"), ("a: 0017\n", "0017"), ("a: -017\n", "-017"), ("a: 1_000\n", "1_000"), ("a: 0xFF_FF\n", "0xFF_FF"), ("a: 1e400\n", "1e400"), ("a: nULL\n", "nULL"), ("a: tRUE\n", "tRUE"), ("a: FaLsE\n", "FaLsE"), ("a: [1, 0O7]\n", "0O7")] {
            match from_str::<Value>(text) {
                Err(Error::AmbiguousScalar { text: t, line: 1, .. }) => assert_eq!(t, scalar),
                other => panic!("{text:?}: {other:?}"),
            }
        }
        // A typed field is no way round it.
        assert!(matches!(from_str::<Doc>("s: 017\n"), Err(Error::AmbiguousScalar { .. })));
        assert!(matches!(from_str::<Doc>("a: 1_000\n"), Err(Error::AmbiguousScalar { .. })));
        // Quoted, they are the strings they always were.
        assert_eq!(val("a: ['017', \"1_000\", '1e400', \"nULL\"]\n").unwrap(), json!({"a": ["017", "1_000", "1e400", "nULL"]}));
        assert_eq!(from_str::<Doc>("s: \"017\"\n").unwrap().s.as_deref(), Some("017"));
        // A key is a plain scalar too.
        assert!(matches!(val("017: a\n"), Err(Error::AmbiguousScalar { .. })));
    }

    #[test]
    fn yaml_1_1_booleans_are_strings_and_a_boolean_field_refuses_them() {
        // serde_yaml: strings; a bool field: "invalid type: string".
        assert_eq!(val("a: [yes, no, on, off, Yes, NO, On, OFF, y, n, Y, N]\n").unwrap(), json!({"a": ["yes", "no", "on", "off", "Yes", "NO", "On", "OFF", "y", "n", "Y", "N"]}));
        assert!(from_str::<Doc>("b: yes\n").is_err());
        assert!(from_str::<Doc>("b: on\n").is_err());
        assert_eq!(from_str::<Doc>("s: yes\n").unwrap().s.as_deref(), Some("yes"));
        assert_eq!(val("a: [true, false, True, False, TRUE, FALSE]\n").unwrap(), json!({"a": [true, false, true, false, true, false]}));
        assert_eq!(from_str::<Doc>("b: TRUE\n").unwrap().b, Some(true));
    }

    #[test]
    fn nulls_are_read_as_serde_yaml_read_them() {
        assert_eq!(val("a: [~, null, Null, NULL]\nb:\nc: \"\"\n").unwrap(), json!({"a": [null, null, null, null], "b": null, "c": ""}));
        let d = from_str::<Doc>("a: ~\ns: null\nb:\n").unwrap();
        assert_eq!((d.a, d.s, d.b), (None, None, None));
        assert_eq!(from_str::<Doc>("s: \"null\"\n").unwrap().s.as_deref(), Some("null"));
        assert_eq!(val("").unwrap(), Value::Null);
        assert_eq!(val("# only a comment\n").unwrap(), Value::Null);
    }

    #[test]
    fn a_null_in_a_string_field_is_refused() {
        // serde_yaml gave the text (`null`, `~`) or the empty string (a bare `key:`); the new
        // parser's typed read refuses it. Loud, and the fix is to write "" or quote the text.
        assert!(matches!(from_str::<BTreeMap<String, String>>("k:\n"), Err(Error::Parse(_))));
        assert!(matches!(from_str::<BTreeMap<String, String>>("k: ~\n"), Err(Error::Parse(_))));
        assert!(matches!(from_str::<BTreeMap<String, String>>("k: null\n"), Err(Error::Parse(_))));
        assert_eq!(from_str::<BTreeMap<String, String>>("k: \"\"\n").unwrap()["k"], "");
    }

    #[test]
    fn only_one_document_is_read() {
        assert_eq!(val("a: 1\n---\nb: 2\n"), Err(Error::MultipleDocuments));
        assert_eq!(val("a: 1\n---\n"), Err(Error::MultipleDocuments));
        assert_eq!(val("---\na: 1\n").unwrap(), json!({"a": 1}));
        assert_eq!(val("a: 1\n...\n").unwrap(), json!({"a": 1}));
    }

    #[test]
    fn a_yaml_directive_is_refused() {
        assert!(matches!(val("%YAML 1.1\n---\na: yes\n"), Err(Error::Directive { .. })));
        assert!(matches!(val("%YAML 1.2\n---\na: 1\n"), Err(Error::Directive { .. })));
    }

    #[test]
    fn a_block_scalar_that_ends_the_input_needs_its_final_newline() {
        // serde_yaml kept the text as written ("a\nb"); the new parser adds a newline.
        assert_eq!(val("k: |\n  a\n  b"), Err(Error::BlockScalarAtEnd));
        assert_eq!(val("k: >\n  a\n  b"), Err(Error::BlockScalarAtEnd));
        assert_eq!(val("k: |\n  a\n  b\n").unwrap(), json!({"k": "a\nb\n"}));
        assert_eq!(val("k: >-\n  a\n  b\n").unwrap(), json!({"k": "a b"}));
        // not at the end: unaffected
        assert_eq!(val("k: |\n  a\nj: 1").unwrap(), json!({"k": "a\n", "j": 1}));
        assert_eq!(val("k: 'x'").unwrap(), json!({"k": "x"}));
    }

    #[test]
    fn u0085_u2028_u2029_are_refused_raw_and_fine_escaped() {
        // serde_yaml folded each into a space (YAML 1.1 line breaks); the new parser keeps it.
        for ch in ['\u{85}', '\u{2028}', '\u{2029}'] {
            let text = format!("a: x{ch}y\nb: 1\n");
            assert_eq!(val(&text), Err(Error::UnicodeLineBreak { character: ch, line: 1, column: 5 }), "{ch:?}");
        }
        assert!(matches!(val("a: 1\n# a note\u{85}here\n"), Err(Error::UnicodeLineBreak { line: 2, .. })), "a comment too: refused wherever it stands");
        assert_eq!(val("a: \"x\\Ny\\Lz\\P\"\n").unwrap(), json!({"a": "x\u{85}y\u{2028}z\u{2029}"}));
    }

    #[test]
    fn unknown_and_missing_fields_name_the_field() {
        let e = from_str::<Doc>("zzz_not_a_field: 1\n").unwrap_err();
        assert!(e.to_string().contains("zzz_not_a_field"), "{e}");
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct Need {
            sigma_pointing_proportional_rad: f64,
        }
        let e = from_str::<Need>("other: 1\n").unwrap_err();
        assert!(e.to_string().contains("sigma_pointing_proportional_rad"), "{e}");
    }

    #[test]
    fn plain_text_things_are_the_same() {
        assert_eq!(val("a: 2001-12-14\nb: 1.2.3\nc: 1:30\nd: a:b\ne: a#b\n").unwrap(), json!({"a": "2001-12-14", "b": "1.2.3", "c": "1:30", "d": "a:b", "e": "a#b"}));
        assert_eq!(val("a: |\n  l1\n  l2\nb: >\n  f1\n  f2\nc: 'it''s'\nd: \"\\u00e9\\x41\\n\"\n").unwrap(), json!({"a": "l1\nl2\n", "b": "f1 f2\n", "c": "it's", "d": "éA\n"}));
        assert_eq!(val("\u{feff}a: 1\n").unwrap(), json!({"a": 1}));
        assert!(val("? [1, 2]\n: v\n").is_err(), "a sequence key cannot be a json object key");
        assert!(matches!(from_str::<BTreeMap<i64, String>>("1: a\n2: b\n").unwrap().len(), 2));
    }

    #[test]
    fn a_quoted_scalar_in_a_number_or_boolean_field_is_read_where_serde_yaml_refused_it() {
        // The one place the new parser accepts what the old refused, and it cannot be closed
        // from the text: whether `"5"` is a string or a number depends on the field it lands in.
        // No document that read before changes value (a String field gets "5" from both).
        assert_eq!(from_str::<Doc>("a: \"5\"\n").unwrap().a, Some(5));
        assert_eq!(from_str::<Doc>("a: ' 5 '\n").unwrap().a, Some(5));
        assert_eq!(from_str::<Doc>("b: 'true'\n").unwrap().b, Some(true));
        assert_eq!(from_str::<Doc>("s: \"5\"\n").unwrap().s.as_deref(), Some("5"));
        assert_eq!(val("a: \"5\"\n").unwrap(), json!({"a": "5"}), "untyped, still the string");
    }

    #[test]
    fn nesting_beyond_the_limit_is_refused() {
        let deep = format!("a: {}1{}\n", "[".repeat(300), "]".repeat(300));
        assert!(val(&deep).is_err());
    }
}
