//! Rego policy evaluation at `CHECKED` (`docs/aiplane-plan.md` milestone A1.2;
//! `docs/open-questions.md` question 201(a)). Policies are `.rego` files loaded from a
//! directory the profile declares (`profiles/execution.yaml`'s new `authority:` block, see
//! [`load_profile_authority_config`]), evaluated **in process** by the `regorus` crate --
//! never by shelling out to an OPA binary (question 201(a): "running the OPA binary was
//! rejected for now because it bundles Go crypto and would be a fetched binary at test
//! time").
//!
//! # No network, by construction, not just no crypto
//!
//! `crates/av-command/Cargo.toml` builds `regorus` with `default-features = false, features =
//! ["arc", "regex"]`. That line is doing two jobs at once and both matter to this module:
//!
//! - **No bundled crypto** (ADR-004's crypto rule): regorus 0.12.0 ships **no crypto
//!   built-ins at all** (there is no `crypto` feature and no `src/builtins/crypto.rs` in the
//!   vendored source), and turning on regorus's `std` feature would pull in `rand` ->
//!   `chacha20`, a bundled-crypto crate the rule forbids. This crate never enables `std`.
//! - **No network builtin, at all, ever** (question 154: "no network at test time" -- and,
//!   by the same mechanism, no network at *run* time either, since the evaluator this module
//!   builds is the same one a test builds). regorus's `http.send` and the `net.*` builtins
//!   live behind its `http`/`net` Cargo features; this crate's dependency line above turns on
//!   neither. An `Engine` built by [`evaluate`] below therefore has **no way to make a
//!   network call from inside a policy** -- not "the test environment happens to be
//!   offline," but "the evaluator this crate links has no network-capable code path to call,"
//!   the same way it has no crypto-capable one. This is worth stating plainly because a
//!   reviewer checking question 154 for *this* crate should not have to re-derive it from the
//!   crypto rule alone -- they are two separate guarantees that happen to come from the same
//!   two absent feature flags.
//!
//! # The bundle and its hash
//!
//! [`PolicyBundle::load`] walks a directory recursively, collects every file ending in
//! `.rego`, and sorts them **by the path relative to the bundle root, compared as bytes**
//! (`str`'s `Ord` is already a byte-wise comparison of its UTF-8 encoding, but
//! [`collect_rego_files`] compares `.as_bytes()` explicitly so this is not an implicit
//! coincidence an assessor has to re-derive). A directory with no `.rego` file anywhere under
//! it is a typed [`PolicyLoadError::NoRegoFiles`], never an empty allow-everything bundle.
//!
//! [`PolicyBundle::policy_hash`] is the SHA-256 hex digest (via `openssl::sha::sha256`, the
//! same hashing path `crates/av-command/src/ledger.rs` uses -- ADR-004's crypto rule: SHA-256
//! only, the system OpenSSL only) of one buffer built by concatenating, **for each file in
//! that sorted order**:
//!
//! 1. the file's relative path, UTF-8 bytes, big-endian `u32` length prefix;
//! 2. the file's raw contents, bytes, big-endian `u32` length prefix.
//!
//! i.e. `SHA256( len(path_0) || path_0 || len(bytes_0) || bytes_0 || len(path_1) || path_1 ||
//! len(bytes_1) || bytes_1 || ... )`. An assessor can recompute this by hand from the files on
//! disk plus this description alone -- [`compute_policy_hash`] is the one function that does
//! it, and [`hash_stable_across_two_loads_and_changes_on_one_byte`] proves both halves of the
//! property the hash exists for: loading the same directory twice yields the same hash, and
//! changing one byte of one file changes it.
//!
//! # The canonical input document
//!
//! [`canonical_input_json`] is **the one function** that turns a `PolicyInput` into JSON --
//! used both as the document [`evaluate`] hands to the `regorus::Engine` (`set_input_json`)
//! and, unmodified, as one of the three preimages [`compute_decision_id`] hashes. Having one
//! function serve both purposes is deliberate: the hash a replay recomputes and the document
//! the evaluator actually saw can never drift apart, because they are, byte for byte, the
//! same string.
//!
//! Shape (so an assessor can check a real document against this description without reading
//! the code): a JSON object with keys `command_id`, `entity_id`, `command_class`, `hazardous`,
//! `envelope_id` **always present** (each holds the field's proto3 default when the caller did
//! not set it -- `""` for the four strings, `false` for `hazardous` -- rather than being
//! omitted, so a policy author can always write `input.envelope_id != ""` without also
//! handling "the key might not exist"), plus `label` and `rate` present **only when the
//! corresponding `PolicyInput` field is `Some`** (mirroring how a real protobuf-JSON
//! transcoder omits an unset message field, since proto3 gives a message field no
//! distinguishable "set to the empty message" state to preserve). Object keys serialize
//! sorted (this crate's `serde_json` has `preserve_order` off, so `serde_json::Map` is
//! `BTreeMap`-backed and iterates in sorted key order by construction, at every nesting
//! level) and the string carries no insignificant whitespace (`serde_json::to_string`'s
//! compact writer). This is the "identical JSON document an out-of-process OPA would
//! receive" `authority.proto`'s own `PolicyInput` doc comment promises -- **as a pinned
//! document shape, not as a cross-check executed against a real OPA binary**; see
//! [`tests::canonical_input_json_pins_the_exact_document_shape_a_real_opa_would_receive`]'s
//! own doc comment for why no OPA binary runs here (no network, no fetched binary, question
//! 201(a)/154) and for the exact record of that limitation.
//!
//! # Entry points, and `deny` always wins (`allow ∧ ¬deny`)
//!
//! [`ALLOW_ENTRYPOINT`] (`data.altavista.authority.allow`, a boolean) and
//! [`DENY_ENTRYPOINT`] (`data.altavista.authority.deny`, a set of reason strings) are the two
//! OPA-compatible entry points [`evaluate`] queries -- **both, every time, unconditionally.**
//! An earlier version of this module short-circuited: if `allow` was `true`, `deny` was never
//! even evaluated. That was overruled on review, because it made "the rule that explains why
//! this is denied" unreachable whenever a policy author's `allow` rule was not written to
//! guard against every `deny` condition itself -- a property of one `.rego` file a human can
//! edit, not a property the *evaluator* is allowed to depend on. The evaluator has to fail
//! closed regardless of how the policy file is shaped, so the calling convention is now:
//!
//! > **`final_allow = allow_rule_result ∧ deny_set.is_empty()`.** A non-empty `deny` set makes
//! > the decision a denial with those reasons, *no matter what `allow` says* -- `allow`
//! > merely wins when there is nothing in `deny` to override it.
//!
//! **This is now part of the OPA-compatibility contract, not an implementation detail**: a
//! deployed OPA evaluating this same bundle must be called the same way (query both
//! `data.altavista.authority.allow` and `data.altavista.authority.deny`, and let a non-empty
//! `deny` override a `true` `allow`) for the two evaluators to agree on every input, including
//! one where a policy author's `allow` and `deny` both fire.
//! [`tests::deny_wins_over_an_unconditionally_true_allow_rule`] pins this with a fixture the
//! shipped `command.rego` cannot itself reach (every `allow` rule there already guards on
//! `not envelope_claimed`), specifically so the property is tested independently of whether
//! today's shipped policy happens to need it.
//!
//! `allow_rule_result` is `true` only when [`ALLOW_ENTRYPOINT`] evaluates to the literal
//! boolean `true`; `false`, `undefined` (regorus's `Value::Undefined`, meaning no rule body
//! matched this input) and "the rule is not defined anywhere in the bundle at all" (regorus
//! reports that specific case as `Err("not a valid rule path")`, before it ever attempts
//! evaluation -- see [`is_rule_not_defined`]) all read as `false`, never as an evaluation
//! failure. A policy with no `allow` rule at all therefore denies every input, never allows
//! one; [`tests::a_policy_with_no_allow_rule_denies`] and
//! [`tests::a_bundle_directory_with_no_rego_file_is_a_typed_error`] are this module's two
//! halves of A1's "policy with no `allow` rule denies; a bundle directory with no `.rego`
//! file is a typed error" acceptance line.
//!
//! `reasons` on the returned [`PolicyDecision`] is the `deny` set's member strings (plus any
//! evaluation-failure reasons, see the next section), **sorted** (a Rego set has no defined
//! iteration order -- `regorus::Set` is not documented to iterate in any particular order, and
//! sorting here is what makes the decision byte-stable across runs, matching this crate's
//! wider determinism rule). When the outcome is a denial and `reasons` would otherwise be
//! empty (the no-`allow`-rule case, or an `allow` that is merely `false`/undefined with no
//! matching `deny` rule either and no evaluation failure), [`evaluate`] synthesizes exactly
//! one reason string saying so, so a denial is never silently reason-less -- see [`evaluate`]'s
//! own doc for the exact text. `matched_rule_path` is [`ALLOW_ENTRYPOINT`] when the decision
//! allows and [`DENY_ENTRYPOINT`] when it denies (including the synthesized-reason case): "the
//! entry point that produced the decision" is read here as *which boolean outcome the
//! decision followed*, not literally which Rego rule body happened to run.
//!
//! # Evaluation failures are never swallowed
//!
//! A rule that fails to evaluate -- a Rego runtime error inside its body (a builtin call that
//! bails, a type error), or a rule that evaluates cleanly but to a value of the wrong shape
//! (`allow := 5` instead of a boolean; `deny := "not a set"` instead of a set; a `deny` set
//! containing a non-string element) -- is **not** the same thing as "this rule is absent" or
//! "this rule evaluated to false/undefined," and this module never conflates the two. Each of
//! the three shapes below produces its own named reason string, appended to `reasons` exactly
//! like a real `deny` reason (which, per the previous section, forces the decision to deny --
//! evaluation failures fail closed *and* fail loud, never closed-and-silent):
//!
//! 1. **The rule errored during evaluation** (any `Err` from `engine.eval_rule` other than
//!    "not a valid rule path" -- see [`is_rule_not_defined`]): `"entry point <path> failed to
//!    evaluate: <regorus's own error text>"`.
//! 2. **The rule returned a value of the wrong shape** (not a boolean for `allow`, not a set
//!    for `deny`, and not `Value::Undefined` either): `"entry point <path> returned <shape
//!    description>, expected <a boolean|a set of strings>"`.
//! 3. **A `deny` set contained a non-string element**: `"entry point <deny path> produced a
//!    set containing a non-string element: <shape description>"`.
//!
//! This is exactly the defect the `%q`-in-`sprintf` mistake in this module's own test-writing
//! history nearly shipped silently (see this crate's task report): the earlier
//! `eval_string_set_rule` mapped *any* `Err` to an empty `Vec`, so a `deny` rule that failed to
//! evaluate was byte-for-byte indistinguishable in the ledger from a `deny` rule that cleanly
//! produced no reasons. [`tests::evaluation_error_in_allow_is_a_named_reason_not_a_silent_deny`],
//! [`tests::deny_rule_returning_the_wrong_shape_is_a_named_reason`] and
//! [`tests::deny_set_containing_a_non_string_element_is_a_named_reason`] each provoke one of
//! the three shapes above with a fixture policy and assert the exact reason text (not merely
//! that the decision denied, since "denied" also passes against the unfixed code -- an
//! evaluation failure and an ordinary denial must be visibly different in the ledger).
//!
//! # `decision_id`
//!
//! [`compute_decision_id`] is `SHA-256( len(policy_hash) || policy_hash || len(input_json) ||
//! input_json || evaluated_tai_ns_as_8_be_bytes )` -- the first two preimages length-prefixed
//! (variable-length strings), the third fixed at 8 bytes (`i64::to_be_bytes`) so it needs no
//! prefix. Deterministic: no UUID, no randomness, anywhere in this module -- the only
//! `evaluate` any test recomputing a `decision_id` needs to reproduce it exactly is the same
//! `policy_hash`, the same `PolicyInput`, and the same `evaluated_tai_ns` (from the injected
//! [`crate::clock::Clock`], never a direct wall-clock read).
//!
//! # A fresh `Engine` per evaluation
//!
//! [`evaluate`] constructs a brand-new `regorus::Engine` every call and shares nothing
//! between calls. This is not an optimization left on the table -- it is the property that
//! makes two evaluations over equal inputs produce byte-identical decisions: an `Engine`
//! that accumulated state across calls (a cache, a previous input still attached, anything)
//! would risk one evaluation's outcome depending on what ran before it, which is exactly what
//! `crates/av-command/src/ledger.rs`'s own determinism test (mirrored at the `authority`
//! layer, see `crates/av-command/src/authority.rs`) exists to catch.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use av_cdm::pb::{PolicyDecision, PolicyInput};
use openssl::sha::sha256;
use thiserror::Error;

use crate::clock::Clock;

/// OPA-compatible entry point for the boolean allow decision. See the module doc's "Entry
/// points and deny-by-default" section.
pub const ALLOW_ENTRYPOINT: &str = "data.altavista.authority.allow";
/// OPA-compatible entry point for the deny reason set. See the module doc.
pub const DENY_ENTRYPOINT: &str = "data.altavista.authority.deny";

/// Every way loading a [`PolicyBundle`] can fail. Never silently produces an empty,
/// allow-everything bundle.
#[derive(Debug, Error)]
pub enum PolicyLoadError {
    /// A filesystem error while reading the bundle directory or one of its files (including
    /// a `.rego` file whose bytes are not valid UTF-8 -- Rego source is text).
    #[error("reading policy bundle at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The bundle directory (recursively) contains no file ending in `.rego`. A typed error,
    /// per this milestone's own rule: never treated as an empty, allow-everything bundle.
    #[error("policy bundle directory {0:?} contains no .rego file")]
    NoRegoFiles(PathBuf),
}

/// A loaded set of `.rego` policy files plus their content-addressed [`policy_hash`]. See the
/// module doc's "The bundle and its hash" section for the exact hash preimage.
///
/// [`policy_hash`]: PolicyBundle::policy_hash
#[derive(Debug, Clone)]
pub struct PolicyBundle {
    /// (path relative to the bundle root, `/`-separated; Rego source text), sorted by the
    /// path's bytes -- the exact order [`compute_policy_hash`] hashed and the exact order
    /// [`evaluate`] loads files into the `regorus::Engine`.
    files: Vec<(String, String)>,
    policy_hash: String,
}

impl PolicyBundle {
    /// Loads every `.rego` file found recursively under `dir`, sorted by relative path bytes,
    /// and computes [`Self::policy_hash`] over them. `Err(`[`PolicyLoadError::NoRegoFiles`]`)`
    /// if none are found -- never `Ok` with an empty bundle.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, PolicyLoadError> {
        let root = dir.as_ref();
        let raw = collect_rego_files(root)?;
        let policy_hash = compute_policy_hash(&raw);
        let mut files = Vec::with_capacity(raw.len());
        for (relative_path, bytes) in raw {
            let source = String::from_utf8(bytes).map_err(|e| PolicyLoadError::Io {
                path: root.join(&relative_path),
                source: io::Error::new(io::ErrorKind::InvalidData, e.to_string()),
            })?;
            files.push((relative_path, source));
        }
        Ok(Self { files, policy_hash })
    }

    /// SHA-256 hex digest of the bundle's files -- see the module doc for the exact preimage.
    pub fn policy_hash(&self) -> &str {
        &self.policy_hash
    }

    /// The loaded files, in the same sorted order the hash was computed over.
    pub fn files(&self) -> &[(String, String)] {
        &self.files
    }
}

/// Recursively collects every `.rego` file under `root`, returning `(path relative to root
/// with `/` separators, raw file bytes)` pairs sorted by the relative path's bytes. `Err(`
/// [`PolicyLoadError::NoRegoFiles`]`)` if the result would be empty.
fn collect_rego_files(root: &Path) -> Result<Vec<(String, Vec<u8>)>, PolicyLoadError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| PolicyLoadError::Io { path: dir.clone(), source: e })?;
        for entry in entries {
            let entry = entry.map_err(|e| PolicyLoadError::Io { path: dir.clone(), source: e })?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_some_and(|ext| ext == "rego") {
                let bytes = fs::read(&path).map_err(|e| PolicyLoadError::Io { path: path.clone(), source: e })?;
                let relative = path
                    .strip_prefix(root)
                    .expect("path was found by walking root, so it is under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((relative, bytes));
            }
        }
    }
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    if out.is_empty() {
        return Err(PolicyLoadError::NoRegoFiles(root.to_path_buf()));
    }
    Ok(out)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The exact preimage the module doc's "The bundle and its hash" section describes.
fn compute_policy_hash(files: &[(String, Vec<u8>)]) -> String {
    let mut buf = Vec::new();
    for (path, bytes) in files {
        let path_bytes = path.as_bytes();
        buf.extend_from_slice(&(path_bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(path_bytes);
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
    }
    hex_encode(&sha256(&buf))
}

/// The one function that turns a `PolicyInput` into JSON -- see the module doc's "The
/// canonical input document" section for the exact shape and why this same string is both
/// the evaluator's input and the `decision_id` hash preimage.
pub fn canonical_input_json(input: &PolicyInput) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("command_class".to_string(), serde_json::Value::String(input.command_class.clone()));
    obj.insert("command_id".to_string(), serde_json::Value::String(input.command_id.clone()));
    obj.insert("entity_id".to_string(), serde_json::Value::String(input.entity_id.clone()));
    obj.insert("envelope_id".to_string(), serde_json::Value::String(input.envelope_id.clone()));
    obj.insert("hazardous".to_string(), serde_json::Value::Bool(input.hazardous));

    if let Some(label) = &input.label {
        let mut label_obj = serde_json::Map::new();
        let caveats = label.caveats.iter().cloned().map(serde_json::Value::String).collect();
        label_obj.insert("caveats".to_string(), serde_json::Value::Array(caveats));
        label_obj.insert("marking".to_string(), serde_json::Value::String(label.marking.clone()));
        obj.insert("label".to_string(), serde_json::Value::Object(label_obj));
    }

    if let Some(rate) = &input.rate {
        let mut rate_obj = serde_json::Map::new();
        let mut counts_obj = serde_json::Map::new();
        for (class, count) in &rate.counts_by_class {
            counts_obj.insert(class.clone(), serde_json::Value::Number((*count).into()));
        }
        rate_obj.insert("counts_by_class".to_string(), serde_json::Value::Object(counts_obj));
        rate_obj.insert("window_ns".to_string(), serde_json::Value::Number(rate.window_ns.into()));
        obj.insert("rate".to_string(), serde_json::Value::Object(rate_obj));
    }

    serde_json::to_string(&serde_json::Value::Object(obj))
        .expect("a document built only from strings, bools, numbers and maps of the same never fails to serialize")
}

/// `SHA-256( len(policy_hash) || policy_hash || len(input_json) || input_json ||
/// evaluated_tai_ns_as_8_be_bytes )` -- see the module doc's "`decision_id`" section.
fn compute_decision_id(policy_hash: &str, canonical_input_json: &str, evaluated_tai_ns: i64) -> String {
    let mut buf = Vec::new();
    let policy_hash_bytes = policy_hash.as_bytes();
    buf.extend_from_slice(&(policy_hash_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(policy_hash_bytes);
    let input_bytes = canonical_input_json.as_bytes();
    buf.extend_from_slice(&(input_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(input_bytes);
    buf.extend_from_slice(&evaluated_tai_ns.to_be_bytes());
    hex_encode(&sha256(&buf))
}

/// `true` exactly when `err` is the specific `anyhow::Error` regorus's `eval_rule_in_path`
/// produces when the queried path was never referenced anywhere in the loaded bundle at all
/// (`bail!("not a valid rule path")`, checked against `compiled_policy.rule_paths` *before*
/// any evaluation is attempted -- vendored source, `src/interpreter.rs`'s
/// `eval_rule_in_path`). That specific case means "this bundle defines no such rule," which is
/// this module's deny-by-default case, not an evaluation failure -- every other `Err` is a
/// genuine failure during evaluation and must be surfaced (see the module doc's "Evaluation
/// failures are never swallowed" section).
///
/// This is admittedly a string match against a message regorus's own doc comment (and this
/// module's own [`policy_dependency_probe`]) treats as part of its pinned 0.12.0 behaviour,
/// not a stable public API regorus promises never to reword. If a future regorus upgrade
/// changes this exact text, every rule-not-defined case would start being misreported as an
/// evaluation failure instead -- loud, not silent, so a version bump that changes this text
/// would be caught by [`tests::a_policy_with_no_allow_rule_denies`] (its reason would stop
/// containing `"default-deny"`), not silently miscategorized.
fn is_rule_not_defined(err: &impl std::fmt::Display) -> bool {
    err.to_string().contains("not a valid rule path")
}

/// A short, deterministic description of a Rego value's shape, for the "wrong shape" reason
/// text the module doc's "Evaluation failures are never swallowed" section describes. Never
/// includes the value's own content (which could be large, or -- for an `Object`/`Array` --
/// not meaningfully renderable as a short string); only its kind.
fn describe_shape(value: &regorus::Value) -> &'static str {
    match value {
        regorus::Value::Null => "null",
        regorus::Value::Bool(_) => "a boolean",
        regorus::Value::Number(_) => "a number",
        regorus::Value::String(_) => "a string",
        regorus::Value::Array(_) => "an array",
        regorus::Value::Set(_) => "a set",
        regorus::Value::Object(_) => "an object",
        regorus::Value::Undefined => "undefined",
    }
}

/// Evaluates `rule_path` and resolves it to `(allow, extra_reasons)`: `allow` is `true` only
/// for `Ok(Value::Bool(true))`; every other case -- `false`, `Undefined`, the rule not being
/// defined at all ([`is_rule_not_defined`]), a genuine evaluation error, or a value of the
/// wrong shape -- makes `allow` `false`, and the latter two also push a named failure reason
/// (see the module doc). Used for [`ALLOW_ENTRYPOINT`] only.
fn resolve_allow_rule(engine: &mut regorus::Engine, rule_path: &str) -> (bool, Vec<String>) {
    match engine.eval_rule(rule_path.to_string()) {
        Ok(regorus::Value::Bool(b)) => (b, Vec::new()),
        Ok(regorus::Value::Undefined) => (false, Vec::new()),
        Ok(other) => (false, vec![format!("entry point {rule_path} returned {}, expected a boolean", describe_shape(&other))]),
        Err(e) if is_rule_not_defined(&e) => (false, Vec::new()),
        Err(e) => (false, vec![format!("entry point {rule_path} failed to evaluate: {e}")]),
    }
}

/// Evaluates `rule_path` and resolves it to a `Vec<String>` of reasons: the `deny` set's own
/// string members, plus a named failure reason for each of the three "Evaluation failures are
/// never swallowed" shapes (a rule error, a non-set result, or a set element that is not a
/// string) -- never an empty `Vec` standing in for "something went wrong and I have no idea
/// what." Order is whatever `regorus::Set` iterates in; [`evaluate`] sorts the combined
/// `reasons` before it reaches a [`PolicyDecision`]. Used for [`DENY_ENTRYPOINT`] only.
fn resolve_deny_rule(engine: &mut regorus::Engine, rule_path: &str) -> Vec<String> {
    match engine.eval_rule(rule_path.to_string()) {
        Ok(regorus::Value::Set(set)) => set
            .iter()
            .map(|v| match v {
                regorus::Value::String(s) => s.to_string(),
                other => format!("entry point {rule_path} produced a set containing a non-string element: {}", describe_shape(other)),
            })
            .collect(),
        Ok(regorus::Value::Undefined) => Vec::new(),
        Ok(other) => vec![format!("entry point {rule_path} returned {}, expected a set of strings", describe_shape(&other))],
        Err(e) if is_rule_not_defined(&e) => Vec::new(),
        Err(e) => vec![format!("entry point {rule_path} failed to evaluate: {e}")],
    }
}

/// Evaluates `input` against `bundle` and returns the resulting [`PolicyDecision`]. See the
/// module doc for entry points, deny-by-default, `decision_id` and the fresh-`Engine`
/// determinism rule. Never panics: a bundle file that fails to compile, or any other engine
/// error, is folded into a denial whose sole reason names the failure, exactly like a policy
/// with no matching `allow` rule -- this function has no `Result` in its signature because
/// every failure mode it can hit is representable as "this input is denied, and here is why."
pub fn evaluate(bundle: &PolicyBundle, input: &PolicyInput, clock: &dyn Clock) -> PolicyDecision {
    let evaluated_tai_ns = clock.now_tai_ns();
    let canonical_json = canonical_input_json(input);
    let decision_id = compute_decision_id(bundle.policy_hash(), &canonical_json, evaluated_tai_ns);

    let mut engine = regorus::Engine::new();
    let mut setup_failure: Option<String> = None;
    for (path, source) in bundle.files() {
        if let Err(e) = engine.add_policy(path.clone(), source.clone()) {
            setup_failure = Some(format!("policy file {path:?} failed to compile: {e}"));
            break;
        }
    }
    if setup_failure.is_none() {
        if let Err(e) = engine.add_data(regorus::Value::new_object()) {
            setup_failure = Some(format!("failed to attach the (empty) data document: {e}"));
        }
    }
    if setup_failure.is_none() {
        if let Err(e) = engine.set_input_json(&canonical_json) {
            setup_failure = Some(format!("failed to set the policy input document: {e}"));
        }
    }

    if let Some(reason) = setup_failure {
        return PolicyDecision {
            decision_id,
            allow: false,
            policy_hash: bundle.policy_hash().to_string(),
            reasons: vec![reason],
            matched_rule_path: DENY_ENTRYPOINT.to_string(),
            evaluated_tai_ns,
            input: Some(input.clone()),
        };
    }

    // Both entry points, every time, unconditionally -- see the module doc's "Entry points,
    // and `deny` always wins" section for why this is no longer a short-circuit on `allow`.
    let (allow_rule_result, mut reasons) = resolve_allow_rule(&mut engine, ALLOW_ENTRYPOINT);
    reasons.extend(resolve_deny_rule(&mut engine, DENY_ENTRYPOINT));
    reasons.sort();

    // final_allow = allow_rule_result ∧ deny_set.is_empty() -- a non-empty `reasons` (a real
    // deny reason, or an evaluation-failure reason from either entry point) always overrides
    // a `true` allow_rule_result.
    let allow = allow_rule_result && reasons.is_empty();

    if !allow && reasons.is_empty() {
        reasons.push(format!(
            "no rule at {ALLOW_ENTRYPOINT} matched this input (default-deny: a policy that does not allow is never treated as allow)"
        ));
    }

    PolicyDecision {
        decision_id,
        allow,
        policy_hash: bundle.policy_hash().to_string(),
        reasons,
        matched_rule_path: if allow { ALLOW_ENTRYPOINT.to_string() } else { DENY_ENTRYPOINT.to_string() },
        evaluated_tai_ns,
        input: Some(input.clone()),
    }
}

/// The `authority:` block a profile declares for this crate's policy bundle (A1.2,
/// `profiles/README.md`'s conventions; see `profiles/execution.yaml`). `allow_entrypoint`/
/// `deny_entrypoint` are carried as configuration (so a profile's declared entry points can
/// be checked against [`ALLOW_ENTRYPOINT`]/[`DENY_ENTRYPOINT`] rather than silently drifting
/// from them) even though [`evaluate`] itself always queries the fixed constants today --
/// see this crate's own final task report for why that split was chosen over threading a
/// profile-supplied entry-point string through `evaluate`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ProfileAuthorityConfig {
    /// Repo-relative directory of `.rego` files, e.g. `"profiles/policies/authority"`.
    pub policy_dir: String,
    pub allow_entrypoint: String,
    pub deny_entrypoint: String,
    /// Width of the trailing rate window, nanoseconds -- fed to [`crate::rate::RateSource`].
    pub rate_window_ns: i64,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct ProfileDocument {
    authority: ProfileAuthorityConfig,
}

/// Reads just the `authority:` block out of a full profile YAML document's text (e.g. the
/// real contents of `profiles/execution.yaml`). Every other top-level key (`imagery`,
/// `dynamics_backend`, `planes`, ...) is ignored -- `serde`/`serde_yaml` skip unrecognized
/// fields by default, so this does not need a second copy of the whole profile schema, only
/// the one block this crate actually reads.
///
/// Uses `serde_yaml` (already a workspace dependency of `crates/av-kernel` and
/// `crates/av-sweep` at the same `"0.9"` version this crate declares -- checked with `cargo
/// tree` before adding it here, see this task's final report) rather than hand-rolling a YAML
/// subset parser or having every caller read the profile's own scalar lines with `grep`.
pub fn load_profile_authority_config(yaml: &str) -> Result<ProfileAuthorityConfig, serde_yaml::Error> {
    Ok(serde_yaml::from_str::<ProfileDocument>(yaml)?.authority)
}

/// Pins the exact `regorus` build this crate's `Cargo.toml` was set up with (moved here from
/// `crates/av-command/src/lib.rs` per this task's own instruction to keep what it pins, now
/// that this module is the real Rego evaluator rather than a placeholder): `default-features
/// = false`, `features = ["arc", "regex"]`, evaluating a policy end to end with **no `std`
/// feature** -- see this module's doc for why `std` must never be turned on. If this probe
/// ever fails to compile or fails at runtime, that is a signal the `regorus` dependency line
/// in `Cargo.toml` drifted from what was verified at setup -- check `features`/
/// `default-features` there before touching anything else.
#[cfg(test)]
mod policy_dependency_probe {
    #[test]
    fn regorus_evaluates_a_policy_without_std() {
        let mut engine = regorus::Engine::new();
        engine
            .add_policy(
                "probe.rego".to_string(),
                "package probe\n\nallow if { input.x == 1 }\n".to_string(),
            )
            .expect("policy compiles");
        engine
            .add_data(regorus::Value::from_json_str("{}").expect("data"))
            .expect("data added");
        engine
            .set_input_json("{\"x\": 1}")
            .expect("input parses");
        let v = engine.eval_rule("data.probe.allow".to_string()).expect("rule evaluates");
        assert_eq!(v, regorus::Value::Bool(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::TestClock;
    use av_cdm::pb::{Label, PolicyInputRate};
    use std::collections::BTreeMap;

    fn write_policy(dir: &Path, relative: &str, contents: &str) {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().expect("has a parent")).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("av-command-policy-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn base_input() -> PolicyInput {
        PolicyInput {
            command_id: "cmd-1".to_string(),
            entity_id: "sat-1".to_string(),
            command_class: "mode".to_string(),
            hazardous: false,
            envelope_id: String::new(),
            label: None,
            rate: Some(PolicyInputRate { counts_by_class: BTreeMap::new(), window_ns: 3_600_000_000_000 }),
        }
    }

    /// **Stability and sensitivity**, both required by this task: the same directory loaded
    /// twice yields the same `policy_hash`, and changing one byte of one policy file changes
    /// it.
    #[test]
    fn hash_stable_across_two_loads_and_changes_on_one_byte() {
        let dir = tmp_dir("hash-stability");
        write_policy(&dir, "command.rego", "package altavista.authority\n\nallow if { input.command_class == \"mode\" }\n");

        let a = PolicyBundle::load(&dir).unwrap();
        let b = PolicyBundle::load(&dir).unwrap();
        assert_eq!(a.policy_hash(), b.policy_hash(), "two loads of the same directory must hash identically");

        write_policy(&dir, "command.rego", "package altavista.authority\n\nallow if { input.command_class == \"modeX\" }\n");
        let c = PolicyBundle::load(&dir).unwrap();
        assert_ne!(a.policy_hash(), c.policy_hash(), "one changed byte must change the hash");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bundle_directory_with_no_rego_file_is_a_typed_error() {
        let dir = tmp_dir("no-rego");
        write_policy(&dir, "README.md", "not a policy file");
        let err = PolicyBundle::load(&dir).unwrap_err();
        assert!(matches!(err, PolicyLoadError::NoRegoFiles(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A1's own acceptance line: "a policy with no `allow` rule denies."
    #[test]
    fn a_policy_with_no_allow_rule_denies() {
        let dir = tmp_dir("no-allow-rule");
        write_policy(&dir, "empty.rego", "package altavista.authority\n\n# Deliberately defines nothing: no allow, no deny.\n");
        let bundle = PolicyBundle::load(&dir).unwrap();
        let clock = TestClock::new(1_000);
        let decision = evaluate(&bundle, &base_input(), &clock);
        assert!(!decision.allow, "an empty-but-valid policy must deny, never allow");
        assert_eq!(decision.reasons.len(), 1, "{:?}", decision.reasons);
        assert!(decision.reasons[0].contains("default-deny"), "{:?}", decision.reasons);
        assert_eq!(decision.matched_rule_path, DENY_ENTRYPOINT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Question 203(b)'s required tripwire.** Provokes the real condition through regorus
    /// itself -- a bundle that defines rules (so the engine compiles and evaluates
    /// successfully), then evaluates a rule path that bundle never references anywhere,
    /// through the exact same `Engine::eval_rule` -> `eval_rule_in_path` path
    /// [`resolve_allow_rule`]/[`resolve_deny_rule`] use -- and asserts on **regorus's own
    /// error text**, not a local constant. If this assertion ever fails, it means the
    /// `regorus` version actually linked (pinned `=0.12.0` in `Cargo.toml`, specifically so a
    /// `cargo update` cannot silently drift this) changed the exact wording
    /// `is_rule_not_defined` matches (`"not a valid rule path"`,
    /// `regorus-0.12.0/src/interpreter.rs::eval_rule_in_path`'s `bail!` text) -- which means
    /// [`is_rule_not_defined`] would stop recognizing a rule-not-defined case for what it is,
    /// and **every** such case (not just this test's own fixture) would be reported as a
    /// genuine evaluation failure instead of this module's deny-by-default case. Do not weaken
    /// this assertion or delete it to "fix" a failing build after a regorus upgrade -- fix
    /// [`is_rule_not_defined`]'s match text (and re-verify [`a_policy_with_no_allow_rule_
    /// denies`] still fails without it, per that test's own doc comment) instead, then update
    /// this test and the `Cargo.toml` pin together.
    #[test]
    fn is_rule_not_defined_matches_regorus_own_error_text_for_a_path_never_referenced_in_the_bundle() {
        let mut engine = regorus::Engine::new();
        engine
            .add_policy("probe.rego".to_string(), "package altavista.authority\n\nallow if { input.x == 1 }\n".to_string())
            .expect("a bundle that does define something still compiles");
        engine.add_data(regorus::Value::new_object()).expect("empty data document attaches");
        engine.set_input_json("{\"x\": 1}").expect("input parses");

        // "data.altavista.authority.nonexistent_rule" is never referenced anywhere in the
        // loaded bundle above -- the real, unmodified condition is_rule_not_defined exists to
        // recognize, provoked through regorus itself, not asserted against a stand-in string.
        let err = engine.eval_rule("data.altavista.authority.nonexistent_rule".to_string()).expect_err("evaluating an undefined rule path is an error");

        assert!(
            err.to_string().contains("not a valid rule path"),
            "regorus's own error text for an undefined rule path changed from \"not a valid rule path\" to {:?} -- \
             `crate::policy::is_rule_not_defined` (crates/av-command/src/policy.rs) matches this exact prose, pinned \
             to regorus =0.12.0 in Cargo.toml specifically for this reason (question 203(b)). Update \
             `is_rule_not_defined`'s match text to whatever regorus now says, re-run \
             `a_policy_with_no_allow_rule_denies` with the OLD match text to confirm it still fails loudly (its own \
             doc comment's claim), then update this test's expected text and the Cargo.toml pin comment together. \
             Until fixed, every rule-not-defined case is silently miscategorized as a genuine evaluation failure \
             instead of this module's deny-by-default case.",
            err.to_string()
        );
        assert!(is_rule_not_defined(&err), "is_rule_not_defined must recognize this exact real regorus error: {err}");
    }

    /// **Defect fix, pinned so it cannot regress silently: `deny` always wins over an
    /// unconditionally-true `allow`.** The shipped `command.rego` can never exercise this --
    /// every `allow` rule there is already guarded by `not envelope_claimed` -- so this
    /// fixture exists purely to test the evaluator's own `allow ∧ ¬deny` contract
    /// independently of whether today's shipped policy happens to need it (see the module
    /// doc's "Entry points, and `deny` always wins" section).
    #[test]
    fn deny_wins_over_an_unconditionally_true_allow_rule() {
        let dir = tmp_dir("deny-wins");
        write_policy(
            &dir,
            "command.rego",
            "package altavista.authority\n\nallow := true\n\ndeny contains \"always denied regardless of allow\" if { true }\n",
        );
        let bundle = PolicyBundle::load(&dir).unwrap();
        let clock = TestClock::new(1_000);
        let decision = evaluate(&bundle, &base_input(), &clock);
        assert!(!decision.allow, "a non-empty deny set must override an unconditionally true allow: {decision:?}");
        assert_eq!(decision.reasons, vec!["always denied regardless of allow".to_string()]);
        assert_eq!(decision.matched_rule_path, DENY_ENTRYPOINT);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Defect fix, pinned: an evaluation error in `allow` is a named reason, not a silent
    /// deny.** Provokes a genuine Rego runtime error (regorus's `sprintf` bails on Go's `%q`
    /// verb -- the same limitation this task's own report found while writing the shipped
    /// policy) from inside `allow`'s own body, distinct from "the rule is not defined at all."
    #[test]
    fn evaluation_error_in_allow_is_a_named_reason_not_a_silent_deny() {
        let dir = tmp_dir("allow-errors");
        write_policy(
            &dir,
            "command.rego",
            "package altavista.authority\n\nallow if { sprintf(\"%q\", [\"oops\"]) != \"\" }\n",
        );
        let bundle = PolicyBundle::load(&dir).unwrap();
        let clock = TestClock::new(1_000);
        let decision = evaluate(&bundle, &base_input(), &clock);
        assert!(!decision.allow, "{decision:?}");
        assert_eq!(decision.reasons.len(), 1, "{:?}", decision.reasons);
        assert!(
            decision.reasons[0].starts_with(&format!("entry point {ALLOW_ENTRYPOINT} failed to evaluate: ")),
            "{:?}",
            decision.reasons
        );
        assert!(
            !decision.reasons[0].contains("default-deny"),
            "an evaluation failure must read differently from the no-rule-defined default-deny case: {:?}",
            decision.reasons
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Defect fix, pinned: `deny` returning the wrong shape is a named reason.** `deny` here
    /// is a plain string, not a set -- a policy author's mistake this module must surface
    /// rather than silently treat as "no deny reasons."
    #[test]
    fn deny_rule_returning_the_wrong_shape_is_a_named_reason() {
        let dir = tmp_dir("deny-wrong-shape");
        write_policy(&dir, "command.rego", "package altavista.authority\n\ndeny := \"not a set\"\n");
        let bundle = PolicyBundle::load(&dir).unwrap();
        let clock = TestClock::new(1_000);
        let decision = evaluate(&bundle, &base_input(), &clock);
        assert!(!decision.allow, "{decision:?}");
        assert_eq!(
            decision.reasons,
            vec![format!("entry point {DENY_ENTRYPOINT} returned a string, expected a set of strings")]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Defect fix, pinned: a non-string element inside an otherwise-valid `deny` set is a
    /// named reason**, not silently dropped (the earlier `eval_string_set_rule` used
    /// `filter_map`, which would have discarded exactly this element with no trace at all).
    #[test]
    fn deny_set_containing_a_non_string_element_is_a_named_reason() {
        let dir = tmp_dir("deny-non-string-element");
        write_policy(&dir, "command.rego", "package altavista.authority\n\ndeny contains 42 if { true }\n");
        let bundle = PolicyBundle::load(&dir).unwrap();
        let clock = TestClock::new(1_000);
        let decision = evaluate(&bundle, &base_input(), &clock);
        assert!(!decision.allow, "{decision:?}");
        assert_eq!(
            decision.reasons,
            vec![format!("entry point {DENY_ENTRYPOINT} produced a set containing a non-string element: a number")]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evaluate_is_deterministic_across_independent_engines() {
        let dir = tmp_dir("determinism");
        write_policy(
            &dir,
            "command.rego",
            "package altavista.authority\n\nallow if { input.command_class == \"mode\" }\n",
        );
        let bundle = PolicyBundle::load(&dir).unwrap();
        let clock = TestClock::new(42);
        let input = base_input();
        let d1 = evaluate(&bundle, &input, &clock);
        let d2 = evaluate(&bundle, &input, &clock);
        assert_eq!(d1.decision_id, d2.decision_id);
        assert_eq!(d1.allow, d2.allow);
        assert_eq!(d1.policy_hash, d2.policy_hash);
        assert_eq!(d1.reasons, d2.reasons);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The OPA-compatibility test (A1's acceptance line 5): pins the exact document shape,
    /// not an executed cross-check.** No OPA binary exists on this host and none is fetched
    /// (question 201(a): running the OPA binary was rejected specifically because it would be
    /// a fetched binary at test time; question 154: no network at test time) -- so this test
    /// cannot and does not prove a real `opa eval` would accept this string; it only pins
    /// [`canonical_input_json`]'s output for a known input, so a future change to that
    /// function's shape is caught here rather than discovered later against a real OPA
    /// deployment. Recorded again in this task's own final report, as asked.
    #[test]
    fn canonical_input_json_pins_the_exact_document_shape_a_real_opa_would_receive() {
        let mut counts = BTreeMap::new();
        counts.insert("burn".to_string(), 3u64);
        counts.insert("mode".to_string(), 1u64);
        let input = PolicyInput {
            command_id: "cmd-42".to_string(),
            entity_id: "sat-7".to_string(),
            command_class: "burn".to_string(),
            hazardous: true,
            envelope_id: String::new(),
            label: Some(Label { marking: "CUI".to_string(), caveats: vec!["SP-EXPT".to_string()] }),
            rate: Some(PolicyInputRate { counts_by_class: counts, window_ns: 3_600_000_000_000 }),
        };
        let json = canonical_input_json(&input);
        let expected = "{\"command_class\":\"burn\",\"command_id\":\"cmd-42\",\"entity_id\":\"sat-7\",\"envelope_id\":\"\",\"hazardous\":true,\"label\":{\"caveats\":[\"SP-EXPT\"],\"marking\":\"CUI\"},\"rate\":{\"counts_by_class\":{\"burn\":3,\"mode\":1},\"window_ns\":3600000000000}}";
        assert_eq!(json, expected);
    }

    #[test]
    fn load_profile_authority_config_reads_just_the_authority_block() {
        let yaml = r#"
id: execution
imagery:
  url_template: "./x/{z}/{x}/{y}.png"
authority:
  policy_dir: profiles/policies/authority
  allow_entrypoint: data.altavista.authority.allow
  deny_entrypoint: data.altavista.authority.deny
  rate_window_ns: 3600000000000
planes:
  command: {}
"#;
        let config = load_profile_authority_config(yaml).unwrap();
        assert_eq!(config.policy_dir, "profiles/policies/authority");
        assert_eq!(config.allow_entrypoint, ALLOW_ENTRYPOINT);
        assert_eq!(config.deny_entrypoint, DENY_ENTRYPOINT);
        assert_eq!(config.rate_window_ns, 3_600_000_000_000);
    }
}
