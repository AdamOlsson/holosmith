#!/usr/bin/env bash
# The commands `scripts/verify.sh` and `scripts/gate-quick.sh` must run with the same bytes, and the one
# predicate they must agree on: whether a built web app is present.
#
# Why a file rather than two copies: `verify.sh` is the gate of record and a worker's quick gate may only
# say *broken* — never *proven* — so the only thing that makes its verdict mean anything is that it fails on
# exactly what the gate of record fails on. A clippy flag changed in one and not the other turns a worker's
# green into the parent's red, which is the one failure this arrangement cannot tolerate.
#
# Sourced, never executed, and it sets **no** shell option: both callers keep their own `set -euo pipefail`,
# and a sourced file that changed a caller's options would change what the gate does. The commands are arrays
# rather than functions because `verify.sh`'s `run_phase` prints the command it ran (`ok fmt: cargo fmt --check`);
# a function name would print `gate_fmt` instead and the gate's log would stop naming the command itself.

# The embedded-app assertions in `web.rs` carry a `HOLOSMITH_REQUIRE_WEB=1` arm — a placeholder build is legal
# (ADR-0007), so the tests cannot assert a real app unless a build says one exists. Setting it for the test phase
# only, when a build is present, is what makes those assertions bite rather than pass vacuously.
#
# It is **scoped to one invocation** rather than exported: `scripts/check-embed.sh`'s second case builds with
# `web/dist` removed and *requires* the placeholder, so an exported variable would fail the `embed` phase.
gate_web_present() {
  [ -f web/dist/index.html ]
}

# Read by the callers that source this file, which is why shellcheck sees them as unassigned. Anything else
# in here is a second spelling of the gate, so keep it to the commands, the predicate, and the reasons.
# shellcheck disable=SC2034
GATE_FMT=(cargo fmt --check)
# shellcheck disable=SC2034
GATE_CLIPPY=(cargo clippy --all-targets -- -D warnings)
