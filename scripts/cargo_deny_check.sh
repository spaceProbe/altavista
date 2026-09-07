#!/usr/bin/env bash
# M6.2 / question 86: run `cargo deny check` against the repo-root `deny.toml`.
#
# `cargo-deny` is not part of a normal Rust toolchain install; this script exists so CI (or
# a human) has one command that (a) uses it if present, (b) tries to install it into the
# CALLING USER's own `~/.cargo/bin` if it is absent and the network allows it, and (c) if
# installation fails (offline, no network, a build failure), says so plainly on stderr and
# exits non-zero -- it never reports success without actually having run the check. See
# this task's own report for whether `cargo-deny` was installable in the environment this
# was built in (it was: `cargo install cargo-deny --locked`, v0.20.2, ~70s from a warm
# registry cache).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

if ! command -v cargo-deny >/dev/null 2>&1; then
    echo "cargo_deny_check.sh: cargo-deny not found on PATH; attempting 'cargo install cargo-deny --locked' into \$HOME/.cargo/bin ..." >&2
    if ! cargo install cargo-deny --locked; then
        echo "cargo_deny_check.sh: FAILED to install cargo-deny (offline, or a build failure -- see the output above)." >&2
        echo "cargo_deny_check.sh: recording this as a real gap, not a pass -- deny.toml is present and correct" \
             "(reviewed by hand / against a machine that does have cargo-deny), but this run did NOT execute it." >&2
        exit 1
    fi
fi

cd "$REPO_ROOT"
echo "cargo_deny_check.sh: $(cargo-deny --version)"
exec cargo deny check
