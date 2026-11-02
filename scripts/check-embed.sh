#!/usr/bin/env bash
# Prove the three embed cases: the app present, absent, and required (ADR-0007).
#
# Usage: scripts/check-embed.sh
# A script rather than a Rust test because the third case runs a nested `cargo build`, which
# deadlocks on the target lock inside `cargo test`. Builds into a temp target directory, so
# this tree's artifacts are untouched.

set -euo pipefail

# Prints this script's header comment, whatever its line count.
usage() {
  sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

case "${1:-}" in
  '') ;;
  -h|--help)
    usage
    exit 0
    ;;
  *)
    echo "check-embed.sh: unknown argument: $1" >&2
    exit 2
    ;;
esac

target_dir="$(mktemp -d)"
work="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '$target_dir' '$work'" EXIT

fail() {
  printf 'FAIL %s\n' "$*" >&2
  exit 1
}

build() {
  # No `--quiet`: cargo does not replay a build script's `cargo:warning=` line in quiet
  # mode, and that warning is one of the three cases.
  cargo build --target-dir "$target_dir" --manifest-path crates/holosmith/Cargo.toml
}

# 1. A built app is embedded, with no warning.
if [ ! -f web/dist/index.html ]; then
  scripts/build-web.sh >/dev/null
fi
if ! build 2>"$work/present.err"; then
  fail "a build with web/dist present must succeed: $(cat "$work/present.err")"
fi
if grep -q 'the web app was not built' "$work/present.err"; then
  fail "a build with web/dist present warned about the app: $(cat "$work/present.err")"
fi
printf 'ok the app is embedded when web/dist is there\n'

# 2. No app: a warning and the placeholder.
empty="$work/empty"
mkdir -p "$empty"
if ! HOLOSMITH_WEB_DIST="$empty" build 2>"$work/absent.err"; then
  fail "a build without web/dist must still succeed: $(cat "$work/absent.err")"
fi
grep -q 'the web app was not built' "$work/absent.err" ||
  fail "a build without web/dist must warn: $(cat "$work/absent.err")"
printf 'ok a missing app is a warning and a placeholder\n'

# 3. No app and HOLOSMITH_REQUIRE_WEB=1: the build fails.
if HOLOSMITH_WEB_DIST="$empty" HOLOSMITH_REQUIRE_WEB=1 build 2>"$work/required.err"; then
  fail "HOLOSMITH_REQUIRE_WEB=1 must fail the build without the app"
fi
grep -q 'HOLOSMITH_REQUIRE_WEB=1 but the web app was not built' "$work/required.err" ||
  fail "the failure must name the missing app: $(cat "$work/required.err")"
printf 'ok a required app that is missing fails the build\n'
