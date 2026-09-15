//! D4: `Command.id` and `Command.idempotency_key` are derived by SHA-256, through the
//! `openssl` crate against the system OpenSSL (ADR-004's crypto rule: no `sha2`, no `ring`),
//! over a canonical preimage of the inputs the proposal was made from -- copying
//! `crates/av-gateway/src/query_id.rs`'s own convention byte for byte: one function builds a
//! canonical JSON document (a `serde_json::Map` built key by key, sorted, fixed key set, no
//! insignificant whitespace -- never derived from a struct whose field order could silently
//! change), and a second hashes it with a length-prefixed preimage via `openssl::sha::
//! sha256`. No wall clock, no randomness, anywhere in this module (D8): the SAME rule
//! evaluation, over the SAME scored run, on any host, at any time, produces the SAME
//! `Command.id`/`idempotency_key` -- this crate's own acceptance evidence 3 ("the same run
//! always yields the same proposal") depends on exactly this.
//!
//! `Command.id` and `idempotency_key` are two DIFFERENT digests over the identical set of
//! inputs, distinguished by an explicit `"purpose"` field folded into the canonical document
//! -- not the same hash reused for both fields. Reusing one hash for both would make a
//! command's own id equal to its own idempotency key, which is not itself unsafe here (the
//! kernel-side and service-side duplicate-dispatch guards key on `idempotency_key` alone,
//! never cross-checking it against `id`), but it is also not a property this module wants to
//! assert holds by accident; a distinct tag keeps the two digests independent by
//! construction, the same discipline `query_id`'s own preimage takes with `selector`.

use openssl::sha::sha256;

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Every input a station-keeping burn proposal is made from -- the exact set D3/D4 name:
/// the run identity queried, the scored radius that triggered the rule, and the rule's own
/// declared knobs (never a magic number: every one of these is a CLI argument on the
/// binary that calls this function).
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalInputs<'a> {
    pub run_id: &'a str,
    pub config_hash: &'a str,
    pub score_name: &'a str,
    pub score_value: f64,
    pub reference_radius_m: f64,
    pub threshold_m: f64,
    pub gain_per_s: f64,
    pub max_burn_mps: f64,
    pub entity_id: &'a str,
    pub command_class: &'a str,
    pub model_node_id: &'a str,
    pub model_version: &'a str,
}

/// The exact canonical JSON document a command id/idempotency key is computed over. Public
/// so a test (or a replay tool) can reproduce the preimage independently of
/// [`compute_command_id`]/[`compute_idempotency_key`] themselves.
pub fn canonical_proposal_json(inputs: &ProposalInputs<'_>, purpose: &str) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("purpose".to_string(), serde_json::Value::String(purpose.to_string()));
    obj.insert("run_id".to_string(), serde_json::Value::String(inputs.run_id.to_string()));
    obj.insert("config_hash".to_string(), serde_json::Value::String(inputs.config_hash.to_string()));
    obj.insert("score_name".to_string(), serde_json::Value::String(inputs.score_name.to_string()));
    obj.insert("score_value".to_string(), json_finite_number(inputs.score_value));
    obj.insert("reference_radius_m".to_string(), json_finite_number(inputs.reference_radius_m));
    obj.insert("threshold_m".to_string(), json_finite_number(inputs.threshold_m));
    obj.insert("gain_per_s".to_string(), json_finite_number(inputs.gain_per_s));
    obj.insert("max_burn_mps".to_string(), json_finite_number(inputs.max_burn_mps));
    obj.insert("entity_id".to_string(), serde_json::Value::String(inputs.entity_id.to_string()));
    obj.insert("command_class".to_string(), serde_json::Value::String(inputs.command_class.to_string()));
    obj.insert("model_node_id".to_string(), serde_json::Value::String(inputs.model_node_id.to_string()));
    obj.insert("model_version".to_string(), serde_json::Value::String(inputs.model_version.to_string()));
    serde_json::to_string(&serde_json::Value::Object(obj)).expect("a document built only from strings and finite numbers never fails to serialize")
}

/// `serde_json::Number` has no representation for a non-finite `f64` -- every value this
/// module ever hashes is already checked finite by `crate::rule` before a proposal is even
/// attempted (D3's own typed, counted refusal for a non-finite score), so this is an
/// assertion of an invariant already established upstream, not a second check inventing new
/// behaviour for a case that cannot reach here.
fn json_finite_number(value: f64) -> serde_json::Value {
    serde_json::Number::from_f64(value).map(serde_json::Value::Number).unwrap_or_else(|| panic!("command_id: {value} is not finite; the caller must check finiteness before deriving an id from it"))
}

/// `SHA-256(len(canonical_json) || canonical_json)`, hex-encoded -- see the module doc.
fn compute(inputs: &ProposalInputs<'_>, purpose: &str) -> String {
    let canonical = canonical_proposal_json(inputs, purpose);
    let bytes = canonical.as_bytes();
    let mut buf = Vec::with_capacity(4 + bytes.len());
    buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(bytes);
    hex_encode(&sha256(&buf))
}

/// `Command.id` for this proposal.
pub fn compute_command_id(inputs: &ProposalInputs<'_>) -> String {
    compute(inputs, "command_id")
}

/// `Command.idempotency_key` for this proposal -- a DIFFERENT digest than
/// [`compute_command_id`] over the identical inputs (see the module doc).
pub fn compute_idempotency_key(inputs: &ProposalInputs<'_>) -> String {
    compute(inputs, "idempotency_key")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ProposalInputs<'static> {
        ProposalInputs {
            run_id: "demo_two_instance_frozen_fixture",
            config_hash: "hash-abc",
            score_name: "demo_flt_rmag_at_end",
            score_value: 6_870_517.488_675_741_5,
            reference_radius_m: 6_871_000.0,
            threshold_m: 100.0,
            gain_per_s: 0.001,
            max_burn_mps: 5.0,
            entity_id: "sat-1",
            command_class: "burn",
            model_node_id: "av-proposer.station-keeping",
            model_version: "1.0.0",
        }
    }

    #[test]
    fn command_id_is_deterministic_over_the_same_inputs() {
        let a = compute_command_id(&sample());
        let b = compute_command_id(&sample());
        assert_eq!(a, b);
    }

    #[test]
    fn command_id_and_idempotency_key_differ_over_the_identical_inputs() {
        let inputs = sample();
        assert_ne!(compute_command_id(&inputs), compute_idempotency_key(&inputs));
    }

    #[test]
    fn command_id_is_a_64_character_lowercase_hex_string() {
        let id = compute_command_id(&sample());
        assert_eq!(id.len(), 64);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn command_id_differs_when_any_input_differs() {
        let base = compute_command_id(&sample());
        let mut varied = sample();
        varied.score_value += 1.0;
        assert_ne!(base, compute_command_id(&varied));
        let mut varied = sample();
        varied.run_id = "some-other-run";
        assert_ne!(base, compute_command_id(&varied));
        let mut varied = sample();
        varied.gain_per_s += 0.0001;
        assert_ne!(base, compute_command_id(&varied));
    }

    #[test]
    #[should_panic(expected = "is not finite")]
    fn a_non_finite_input_panics_rather_than_silently_hashing_nan() {
        let mut inputs = sample();
        inputs.score_value = f64::NAN;
        compute_command_id(&inputs);
    }
}
