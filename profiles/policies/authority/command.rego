# Command authority policy -- av-command's CHECKED gate (docs/aiplane-plan.md milestone
# A1.2; docs/open-questions.md question 201(a)/53). Loaded from this directory by the
# profile (profiles/execution.yaml's `authority:` block), evaluated in process by the
# `regorus` crate (crates/av-command/src/policy.rs) at entry points
# `data.altavista.authority.allow` (bool) and `data.altavista.authority.deny` (a set of
# reason strings) -- OPA-compatible, so a deployed OPA binary can evaluate this exact file
# unchanged if it ever replaces the in-process evaluator.
#
# THIS FILE IS A SECURITY ARTIFACT. It is what actually decides whether a proposed command
# reaches AUTHORIZED; a change here gets the same review a change to av-command's Rust
# source gets, not a lighter one just because it is not Rust.
#
# What this file must do, matching A1's own policy-fixture acceptance test exactly:
#   1. Admit one command class outright: "mode".
#   2. Reject one command class outright, with a reason: "payload".
#   3. Rate-limit a third: "burn" -- admitted while its own recent-submission count (real
#      ledger history the check edge supplies as `input.rate.counts_by_class`, never a
#      caller-supplied guess) stays under `burn_rate_limit` below, denied once it reaches or
#      exceeds it.
#   4. Refuse ANY command whose `envelope_id` is non-empty, for every class including "mode" --
#      question 53: propose-only stands, no envelope is enabled by this track. This refusal
#      applies before either admission rule below, so a non-empty envelope_id always wins
#      over "mode"'s otherwise-unconditional admission.
package altavista.authority

# The rate-limit threshold for "burn": admitted below this many recent submissions in the
# window the profile declares (profiles/execution.yaml's `authority.rate_window_ns`), denied
# at and above it.
burn_rate_limit := 3

default allow := false

# 1. "mode" is admitted outright, subject only to the envelope refusal (rule 4) below, which
# applies to every class including this one.
allow if {
	input.command_class == "mode"
	not envelope_claimed
}

# 3. "burn" is admitted only while it is under the rate-limit threshold, and, like every
# class, subject to the envelope refusal.
allow if {
	input.command_class == "burn"
	burn_count < burn_rate_limit
	not envelope_claimed
}

# True when this input claims a non-empty envelope_id. Named so both the admission rules'
# `not envelope_claimed` guard and rule 4's deny below read the same underlying check.
envelope_claimed if {
	input.envelope_id != ""
}

# "burn"'s own recent count from real ledger history, defaulting to 0 when the class has no
# entry at all (object.get's third argument) -- never treated as an error or as "admit by
# default" when the count is simply absent.
burn_count := object.get(input.rate.counts_by_class, "burn", 0)

# 2. "payload" is rejected outright, with its own explicit reason -- it is never mentioned in
# any `allow` rule above, so it falls to `default allow := false` regardless, and this rule
# exists purely to give that refusal a named, human-readable reason rather than leaving
# "payload" to the evaluator's own generic default-deny text.
deny contains "command_class payload is not admitted by policy" if {
	input.command_class == "payload"
}

# 3. The rate-limit refusal for "burn", with a reason that names the actual count and
# threshold so an operator reading the ledger sees exactly why.
deny contains sprintf("command_class burn rate-limited: %d recent submissions >= threshold %d", [burn_count, burn_rate_limit]) if {
	input.command_class == "burn"
	burn_count >= burn_rate_limit
}

# 4. Refuses ANY non-empty envelope_id, for every command class -- question 53: propose-only
# stands, no envelope is enabled by this track. The reason text names the question so a
# reviewer reading the ledger's transition reason (which carries this exact string, see
# crates/av-command/src/authority.rs's `format_reason`) can find the rule behind it without
# guessing.
deny contains sprintf("envelope_id \"%s\" is refused: propose-only stands (question 53), no envelope is enabled by this track", [input.envelope_id]) if {
	envelope_claimed
}
