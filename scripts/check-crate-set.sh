#!/usr/bin/env bash
# Is this tree's `Cargo.lock` the crate set the agent image vendored? The image replaces crates-io with
# `/opt/cargo-vendor` and sets `[net] offline = true` (`crates/holosmith/images/agent.Dockerfile`), so a crate that
# directory does not hold fails as "the crate 'rand' could not be found in registry index" — which reads as a broken
# registry — and `cargo update` then exits 0 having changed nothing. So a session whose marker names another lock is
# a stale image, and saying so in one line is the difference between an image rebuild and a hunt for a code bug.
#
#   scripts/check-crate-set.sh    exit 0: this tree's lock is the vendored set, or there is no marker at all
#                                 exit 3: the marker is a different Cargo.lock's digest — the image is stale
#
# Absent means "not a session": a host carries no marker because it fetches its crates, so that is a pass and not an
# error. `HOLOSMITH_CRATE_SET` overrides the marker's path, which is what makes the mismatch measurable without root
# and without an image; only the path moves, never the sentence. Nothing goes to stdout: a preflight that chatters is
# noise in every run's first lines, and the one failure prints its sentence on stderr.
#
# The marker's path is a literal with three readers — this script, `scripts/verify.sh`'s `case_agent_toolchain`, and
# `crates/holosmith/tests/images.rs` — and its one writer is the recipe's `sha256sum Cargo.lock` line. Three readers
# of a literal are cheaper to follow than a shared definition a Dockerfile's `RUN` would have to carry too, so the
# path is spelled here rather than sourced from `scripts/gates.sh`.

set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

marker="${HOLOSMITH_CRATE_SET:-/opt/cargo-vendor/.holosmith-cargo-lock}"

if [ ! -f "$marker" ]; then
  exit 0
fi

vendored="$(tr -d '\n' <"$marker")"
current="$(sha256sum Cargo.lock | cut -d' ' -f1)"

if [ "$vendored" = "$current" ]; then
  exit 0
fi

printf "check-crate-set: %s names a different Cargo.lock (%s) than this tree's (%s), so a crate this tree needs is missing from the image and cargo reports it as not found in the registry index: rebuild agent-image on this host, because this is a stale image and not a code fix.\n" \
  "$marker" "$vendored" "$current" >&2
exit 3
