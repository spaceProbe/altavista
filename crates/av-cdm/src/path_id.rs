//! The path-component id rule (question 254, item 1): when an id, name or run id that came from
//! outside the program (a CLI argument, a YAML field, a decoded protobuf, an HTTP request) is
//! about to become **one component of a filesystem path**, it must first pass
//! [`validate_path_component_id`].
//!
//! ## The rule
//!
//! A *path-component id* is 1 to [`MAX_LEN`] (128) bytes of ASCII `[A-Za-z0-9._-]` whose first
//! byte is `[A-Za-z0-9]`. That refuses, among everything else: the empty id, `.`, `..`, anything
//! beginning with `.` (so a `.incoming-*` staging name can never be a study id), `/`, `\`, NUL,
//! every control character, whitespace, `%`, `:` and every non-ASCII byte. A refusal is a typed
//! [`PathIdError`], returned **before** any path is built (no `join`, no `exists`, no
//! `create_dir`).
//!
//! The reasons are reported in a fixed order, so every implementation names the same one for the
//! same id: [`PathIdError::Empty`], then [`PathIdError::TooLong`], then the first byte outside the
//! character set ([`PathIdError::DisallowedByte`]), then a first byte that is `.`, `-` or `_`
//! ([`PathIdError::BadFirstByte`]). So `../x` is refused for its `/` and `..` for its leading dot.
//!
//! ## One definition, one set of vectors
//!
//! This is the Rust definition. The Python twin is `altavista/path_id.py`. Both read the accepted
//! and refused ids (each refused one with its reason) from the committed fixture
//! `data/path_component_id_vectors.json`, so the two cannot drift: change the rule and both
//! implementations' tests fail until they agree again.
//!
//! The rule is a *name* rule, not a containment check. A caller whose legitimate inputs need more
//! than this set must still enforce at least the minimum (non-empty, not `.`/`..`, no `/`, `\`, NUL
//! or control character) plus a lexical containment check, and say so where it does.

/// The longest id the rule accepts, in bytes.
pub const MAX_LEN: usize = 128;

/// Why an id is not a path-component id. See the module doc for the order the checks run in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathIdError {
    /// The id is the empty string.
    Empty,
    /// The id is `len` bytes, more than [`MAX_LEN`].
    TooLong { len: usize },
    /// The byte `byte` at byte offset `index` is not in `[A-Za-z0-9._-]` (this covers `/`, `\`,
    /// NUL, every control character, whitespace, `%`, `:` and every non-ASCII byte).
    DisallowedByte { index: usize, byte: u8 },
    /// Every byte is in the set but the first is `byte`, one of `.`, `-`, `_` (so `.`, `..`, a
    /// hidden name and a staging name all fail here).
    BadFirstByte { byte: u8 },
}

impl PathIdError {
    /// A stable machine name for the reason, the one the shared fixture records:
    /// `empty`, `too_long`, `disallowed_byte` or `bad_first_byte`.
    pub fn reason(&self) -> &'static str {
        match self {
            PathIdError::Empty => "empty",
            PathIdError::TooLong { .. } => "too_long",
            PathIdError::DisallowedByte { .. } => "disallowed_byte",
            PathIdError::BadFirstByte { .. } => "bad_first_byte",
        }
    }
}

impl std::fmt::Display for PathIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathIdError::Empty => write!(f, "it is empty (an id that becomes a path component must be 1 to {MAX_LEN} bytes of [A-Za-z0-9._-] starting with [A-Za-z0-9])"),
            PathIdError::TooLong { len } => write!(f, "it is {len} bytes, more than the {MAX_LEN} an id that becomes a path component may have"),
            PathIdError::DisallowedByte { index, byte } => {
                write!(f, "byte 0x{byte:02x} at offset {index} is not in [A-Za-z0-9._-] (an id that becomes a path component may hold nothing else)")
            }
            PathIdError::BadFirstByte { byte } => write!(f, "it begins with {:?}, but an id that becomes a path component must begin with [A-Za-z0-9]", *byte as char),
        }
    }
}

impl std::error::Error for PathIdError {}

/// Checks `id` against the module's rule and returns it unchanged when it passes.
///
/// ```
/// use av_cdm::path_id::{validate_path_component_id, PathIdError};
/// assert_eq!(validate_path_component_id("study-1.v2_a"), Ok("study-1.v2_a"));
/// assert_eq!(validate_path_component_id("../x"), Err(PathIdError::DisallowedByte { index: 2, byte: b'/' }));
/// assert_eq!(validate_path_component_id(".."), Err(PathIdError::BadFirstByte { byte: b'.' }));
/// ```
pub fn validate_path_component_id(id: &str) -> Result<&str, PathIdError> {
    let bytes = id.as_bytes();
    let Some(&first) = bytes.first() else { return Err(PathIdError::Empty) };
    if bytes.len() > MAX_LEN {
        return Err(PathIdError::TooLong { len: bytes.len() });
    }
    if let Some((index, &byte)) = bytes.iter().enumerate().find(|(_, b)| !(b.is_ascii_alphanumeric() || matches!(**b, b'.' | b'_' | b'-'))) {
        return Err(PathIdError::DisallowedByte { index, byte });
    }
    if !first.is_ascii_alphanumeric() {
        return Err(PathIdError::BadFirstByte { byte: first });
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vectors() -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/path_component_id_vectors.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {path}: {e}"))).expect("the vectors fixture is JSON")
    }

    /// Every accepted id passes and comes back unchanged.
    #[test]
    fn every_accepted_vector_passes() {
        let v = vectors();
        let accepted = v["accepted"].as_array().expect("accepted is an array");
        assert!(accepted.len() >= 10, "the fixture must keep a real set of accepted ids");
        for id in accepted {
            let id = id.as_str().expect("accepted ids are strings");
            assert_eq!(validate_path_component_id(id), Ok(id), "{id:?} must be accepted");
        }
    }

    /// Every refused id is refused, for exactly the reason the fixture records.
    #[test]
    fn every_refused_vector_is_refused_for_its_recorded_reason() {
        let v = vectors();
        let refused = v["refused"].as_array().expect("refused is an array");
        assert!(refused.len() >= 30, "the fixture must keep a real set of refused ids");
        let mut reasons = std::collections::BTreeSet::new();
        for entry in refused {
            let id = entry["id"].as_str().expect("refused ids are strings");
            let want = entry["reason"].as_str().expect("refused entries carry a reason");
            let got = validate_path_component_id(id).expect_err(&format!("{id:?} must be refused"));
            assert_eq!(got.reason(), want, "{id:?}: {got}");
            reasons.insert(want.to_string());
        }
        assert_eq!(reasons.into_iter().collect::<Vec<_>>(), vec!["bad_first_byte", "disallowed_byte", "empty", "too_long"], "every reason is exercised by the fixture");
    }

    /// The fixture's stated limit is this module's constant.
    #[test]
    fn the_fixture_names_the_same_limit() {
        assert_eq!(vectors()["max_len"].as_u64(), Some(MAX_LEN as u64));
    }

    /// The error messages name the offending byte or length (the typed value carries them).
    #[test]
    fn errors_carry_and_print_the_offending_detail() {
        assert_eq!(validate_path_component_id("a\0b"), Err(PathIdError::DisallowedByte { index: 1, byte: 0 }));
        assert!(validate_path_component_id("a\0b").unwrap_err().to_string().contains("0x00 at offset 1"));
        assert!(validate_path_component_id(&"a".repeat(129)).unwrap_err().to_string().contains("129 bytes"));
        assert!(validate_path_component_id(".x").unwrap_err().to_string().contains("begins with '.'"));
    }
}
