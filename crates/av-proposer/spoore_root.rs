// Question 219(b): resolves the spoore checkout root `build.rs` compiles
// `spoore.v0.model_service.proto` from -- `SPOORE_ROOT` if set (a typed error if set but
// empty, never silently treated as unset), else the sibling-checkout default
// `<workspace root>/../spoore` (question 12's convention).
//
// `include!`'d directly into `build.rs` (never compiled as an ordinary crate module -- this
// crate has no `lib.rs` `mod spoore_root;`), and separately `#[path = "../spoore_root.rs"]
// mod spoore_root;`-included by `tests/spoore_root.rs`, so both consumers compile the
// identical source text. Plain `//` comments only at module level (rather than `//!`): once
// spliced into the middle of `build.rs` by `include!`, this text is no longer at the true
// start of a module, where inner doc comments are required to live. No `use` statements for
// the same reason -- `build.rs` already imports `std::path::PathBuf` itself, and a second
// `use` of it here would collide once the two files' text is merged; every path type below is
// named in full instead (`std::path::Path`/`std::path::PathBuf`).
//
// That dual use is also why this module stays pure and I/O-light: no network, no reading
// `SPOORE_ROOT` (or any other environment variable) itself -- the caller passes `env_value`
// in -- and it never calls `std::fs::canonicalize` on the resolved path. A symlinked spoore
// root must resolve and work exactly like a real directory in that spot
// (`crates/av-proposer/tests/spoore_root.rs` builds its whole proof around a symlink for
// precisely this reason); canonicalizing would resolve the symlink away and defeat that test.

/// The one file every real spoore checkout has that this crate actually compiles against --
/// proof the resolved root is a real spoore checkout, not merely some directory that happens
/// to exist.
const SPOORE_MARKER_PROTO: &str = "proto/spoore/v0/model_service.proto";

/// Resolves the spoore checkout root.
///
/// - `env_value`: `SPOORE_ROOT`'s value if the caller found the variable set (`Some`, which
///   may be an empty string), or `None` if it was unset. A *relative* path in `SPOORE_ROOT`
///   resolves against the current working directory, exactly the way any other relative path
///   given to a process does -- this function does not special-case it.
/// - `manifest_dir`: `av-proposer`'s own `CARGO_MANIFEST_DIR` (`<repo>/crates/av-proposer`),
///   used only to compute the sibling-checkout default when `SPOORE_ROOT` is unset. The
///   workspace root is `manifest_dir/../..`; the default spoore root is that workspace root's
///   own `../spoore` sibling.
///
/// On success, the returned path exists and contains `proto/spoore/v0/model_service.proto`.
/// On failure, the `Err` message names the resolved path, says whether it came from
/// `SPOORE_ROOT` or the `../spoore` default, and tells the reader to set `SPOORE_ROOT` -- a
/// missing spoore checkout must be a clear, actionable build failure, never a confusing protoc
/// error surfaced three steps downstream.
pub fn resolve_spoore_root(
    env_value: Option<String>,
    manifest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let (root, origin): (std::path::PathBuf, &str) = match env_value {
        Some(v) if v.is_empty() => {
            return Err(
                "SPOORE_ROOT is set but empty -- unset it entirely to use the ../spoore \
                 sibling-checkout default, or set it to your spoore checkout's root directory."
                    .to_string(),
            );
        }
        Some(v) => (std::path::PathBuf::from(v), "SPOORE_ROOT"),
        None => {
            let workspace_root = manifest_dir
                .parent()
                .and_then(std::path::Path::parent)
                .ok_or_else(|| {
                    format!(
                        "cannot compute the ../spoore sibling-checkout default: \
                         CARGO_MANIFEST_DIR {manifest_dir:?} has fewer than two parent \
                         directories. Set SPOORE_ROOT instead."
                    )
                })?;
            (workspace_root.join("..").join("spoore"), "the ../spoore sibling-checkout default")
        }
    };

    if !root.exists() {
        return Err(format!(
            "spoore checkout not found at {root:?} (resolved from {origin}) -- set SPOORE_ROOT \
             to your spoore checkout's root directory."
        ));
    }
    let marker = root.join(SPOORE_MARKER_PROTO);
    if !marker.is_file() {
        return Err(format!(
            "{root:?} (resolved from {origin}) exists but has no {SPOORE_MARKER_PROTO} -- it \
             does not look like a real spoore checkout. Set SPOORE_ROOT to your spoore \
             checkout's root directory."
        ));
    }
    Ok(root)
}
