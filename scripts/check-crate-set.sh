#!/usr/bin/env bash
# Is this tree's `Cargo.lock` the crate set this project image vendored? The image replaces crates-io with
# `/opt/cargo-vendor` and sets `[net] offline = true` (`docker/Dockerfile`, this project's own recipe), so a build that
# needs a crate the directory does not hold fails as "no matching package named `rand` found", over "location searched:
# directory source `/opt/cargo-vendor` (which is replacing registry `crates-io`)" — which reads as a dependency problem
# rather than as the stale image it is — and `cargo update` then exits 0 having changed nothing. So a session whose marker
# names another lock is a stale image, and saying so in one line is the difference between an image rebuild and a hunt
# for a code bug.
#
#   scripts/check-crate-set.sh    exit 0: this tree's lock is the crate set, or there is no marker at all
#                                 exit 3: the marker is a different Cargo.lock's digest — the image is stale
#
# **This is this repository's own check, and not a product mechanism.** This repository is a project like any other
# (ADR-0126) and its image is the one `docker/Dockerfile` builds, so this file plus that recipe's `sha256sum` clause is
# how *this project* finds a stale image: both gates run it before their first phase. Nothing in the `holosmith` binary
# knows the marker's path, invokes this file, or compares a project's crate set — a project that vendors content writes
# its own two halves by copying these, and this is the reference implementation (ADR-0127). That is why the sentence
# below names no project but this one, and why "the image" in it means this repository's.
#
# Absent means "not a session": a host carries no marker because it fetches its crates, so that is a pass and not an
# error. `HOLOSMITH_CRATE_SET` overrides the marker's path, which is what makes the mismatch measurable without root
# and without an image; only the path moves, never the sentence. Nothing goes to stdout: a preflight that chatters is
# noise in every run's first lines, and the one failure prints its sentence on stderr.
#
# The marker's path is a literal with one reader of its content — this script — and one test that asserts the recipe
# writes it, `crates/holosmith/tests/images.rs`; the one writer is the recipe's `sha256sum Cargo.lock` line. A spelled
# literal is cheaper to follow than a shared definition a Dockerfile's `RUN` would have to carry too, so the path is
# spelled here rather than sourced from `scripts/gates.sh`.

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

printf "check-crate-set: %s names a different Cargo.lock (%s) than this tree's (%s), so a crate this tree needs is missing from the image and cargo reports it as not found in the registry index: rebuild this project image on this host (holosmith image build holosmith --force), because this is a stale image and not a code fix.\n" \
  "$marker" "$vendored" "$current" >&2
exit 3
