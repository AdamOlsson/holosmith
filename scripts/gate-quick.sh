#!/usr/bin/env bash
# The gate a worker runs, and the only one it runs: fmt, clippy, the unit tests, and the named integration
# binaries — never the whole suite. `scripts/verify.sh` is the proof and it belongs to the parent, because
# relinking every test binary costs about a minute and running them about two and a half: a worker that
# re-runs `cargo test` two or three times spends its turn in the linker instead of on the change.
#
#   scripts/gate-quick.sh --list                 the names it accepts, one per crates/holosmith/tests/*.rs
#   scripts/gate-quick.sh <name> [<name>...]      fmt, clippy, lib, then each named binary
#   scripts/gate-quick.sh --lib-only              fmt, clippy and lib only, no integration binary
#
# Name every binary whose module you touched. A change that names none runs `--lib-only` and says so in its
# report: the footer counts the binaries that did not run, so an under-named change is visible in the run's own
# last line rather than hidden behind a green. Nothing here derives a name from a diff — a heuristic that guessed
# wrong would be a green that means nothing, and guessing is the worker's job to state, not the script's to hide.
#
# The toolchain is the gate's own: fmt and clippy under a different rustfmt answer a different question, so a
# cargo that is not the pinned 1.99 line is refused with the sentence that fixes it (AGENTS.md).
# GATE_QUICK_ALLOW_TOOLCHAIN=1 runs anyway, and repeats the mismatch in the footer so it cannot be missed.
#
# Exit 0 green, 1 a phase failed (its last line names the phase, and the binary when it was one), 2 misuse: no
# name, an unknown name, cargo absent or the wrong version, `--lib-only` or `--list` combined with names.
# There is no `--all` and no `--full` on purpose: `cargo test` is that, and a second spelling of the whole suite
# is the thing this script exists to stop a worker from reaching for.

set -euo pipefail

# Prints this script's header comment, whatever its line count.
usage() {
  sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

# misuse <sentence>: exit 2, with the one-line shape a caller can copy. Distinguishable from 1 on purpose:
# 1 means the tree is broken, 2 means this invocation was, and a worker must never read either as a pass.
misuse() {
  printf 'gate-quick: %s\n' "$1" >&2
  printf 'usage: scripts/gate-quick.sh [--list | --lib-only | <test-binary>...]\n' >&2
  exit 2
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
# shellcheck source=scripts/gates.sh
. "$repo_root/scripts/gates.sh"

list=0
lib_only=0
names=()
while [ $# -gt 0 ]; do
  case "$1" in
    --list) list=1 ;;
    --lib-only) lib_only=1 ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*) misuse "unknown argument: $1" ;;
    *) names+=("$1") ;;
  esac
  shift
done

# The names, from the same listing `cargo test` builds from: one `tests/*.rs` is one binary, and a directory
# under `tests/` (support, fixtures, golden) is not one. `--list`, the unknown-name error and the footer's
# total all read this one function, so the three can never disagree about how many binaries there are.
test_binaries() {
  local file
  for file in crates/holosmith/tests/*.rs; do
    file="${file##*/}"
    printf '%s\n' "${file%.rs}"
  done | sort
}

known="$(test_binaries)"
total="$(printf '%s\n' "$known" | wc -l | tr -d ' ')"

if [ "$list" -eq 1 ]; then
  [ "${#names[@]}" -eq 0 ] || misuse "--list takes no test binaries: it prints the names to choose from"
  printf '%s\n' "$known"
  exit 0
fi

if [ "$lib_only" -eq 1 ] && [ "${#names[@]}" -gt 0 ]; then
  misuse "--lib-only takes no test binaries: name them instead, or drop the flag"
fi
if [ "$lib_only" -eq 0 ] && [ "${#names[@]}" -eq 0 ]; then
  misuse "no test binary named — --list prints the names, --lib-only runs without any"
fi

# Before cargo is asked anything, so a typo costs a millisecond instead of a lib build. The list comes with the
# error because the names are the part a worker does not remember, and looking them up by failing twice is the
# turn this script is trying to give back. `--list` is the path for a worker who knows it does not know.
for name in "${names[@]}"; do
  # A here-string rather than a pipe: `grep -q` closes early, and under `pipefail` the writer's SIGPIPE would
  # turn a match into a 141 (`verify.sh` learned this the hard way, in expect_no_cors).
  if ! grep -qx "$name" <<<"$known"; then
    printf 'gate-quick: no test binary named %s\n' "$name" >&2
    printf 'gate-quick: these are the names (scripts/gate-quick.sh --list prints them too):\n' >&2
    printf '%s\n' "$known" | sed 's/^/  /' >&2
    exit 2
  fi
done

# The toolchain, before any phase: a green fmt under a different rustfmt is not the gate's answer.
if ! command -v cargo >/dev/null 2>&1; then
  printf 'gate-quick: cargo is not on PATH: . ~/.pi/toolchains/env.sh first (AGENTS.md)\n' >&2
  exit 2
fi
cargo_version="$(cargo --version 2>/dev/null | awk '{print $2}')"
toolchain_note=""
case "$cargo_version" in
  1.99.*) ;;
  *)
    if [ "${GATE_QUICK_ALLOW_TOOLCHAIN:-0}" != "1" ]; then
      printf 'gate-quick: cargo %s is not the pinned 1.99 toolchain, so fmt and clippy would answer a different\n' \
        "${cargo_version:-unknown}" >&2
      printf '            question than the gate of record: . ~/.pi/toolchains/env.sh first (AGENTS.md)\n' >&2
      printf '            GATE_QUICK_ALLOW_TOOLCHAIN=1 runs anyway and repeats this in the footer.\n' >&2
      exit 2
    fi
    toolchain_note="cargo ${cargo_version:-unknown} is not 1.99 (GATE_QUICK_ALLOW_TOOLCHAIN=1), "
    ;;
esac

# Two runs in one worktree queue here rather than interleave: cargo's own lock covers the build, but the test
# binaries bind real ports and write real fixtures, so the lock is the whole run's, not the compiler's.
if command -v flock >/dev/null 2>&1; then
  mkdir -p "$repo_root/target"
  exec 9>"$repo_root/target/gate-quick.lock"
  flock 9
else
  printf 'gate-quick: flock not found — running unlocked (two runs in one worktree may collide)\n' >&2
fi

SECONDS=0
ran=0

# phase <label> <display> <command...>: the gate's own shape, cheapest first, with the display read as a
# sentence rather than as the command array, so the web flag's wrapper does not obscure what ran.
phase() {
  local label="$1" display="$2"
  shift 2
  local rc=0
  printf '== %s ==\n' "$label"
  "$@" || rc=$?
  if [ "$rc" -ne 0 ]; then
    printf 'gate-quick: %s failed (%s exited %d) - the tree is not proven\n' "$label" "$display" "$rc" >&2
    exit 1
  fi
  printf 'ok %s: %s\n' "$label" "$display"
}

phase fmt "${GATE_FMT[*]}" "${GATE_FMT[@]}"
phase clippy "${GATE_CLIPPY[*]}" "${GATE_CLIPPY[@]}"
# The lib tests are the crate's own unit tests, and they are the one place a quick gate can be broad for free:
# 649 of them run in five seconds, so nothing about them is why the full suite is expensive.
if gate_web_present; then
  phase lib 'cargo test --lib (HOLOSMITH_REQUIRE_WEB=1)' env HOLOSMITH_REQUIRE_WEB=1 cargo test --lib
else
  phase lib 'cargo test --lib' cargo test --lib
fi

timeout_s="${GATE_QUICK_TIMEOUT:-600}"
for name in "${names[@]}"; do
  printf '== %s ==\n' "$name"
  rc=0
  timeout "$timeout_s" cargo test --test "$name" || rc=$?
  case "$rc" in
    0) ;;
    124)
      # `timeout`'s own code, said in words: a turn_timeout or preview test that hangs would otherwise read as
      # an ordinary failure, and the worker would go looking in its diff for something that is not there.
      printf 'gate-quick: %s exceeded %ss\n' "$name" "$timeout_s" >&2
      exit 1
      ;;
    *)
      printf 'gate-quick: %s failed (cargo test --test %s exited %d) - the tree is not proven\n' \
        "$name" "$name" "$rc" >&2
      exit 1
      ;;
  esac
  ran=$((ran + 1))
  printf 'ok %s: cargo test --test %s\n' "$name" "$name"
done

# The footer is the contract with a worker, and its count is what makes under-naming visible: a change that
# touched two modules and named one binary reports `1 of 57`, in the worker's own output, where its report and
# the parent's review can both see it. The not-proven sentence is only on the green path — a run that failed says
# nothing about what it proved, and its last line is that failure.
if [ "$lib_only" -eq 1 ]; then
  set_list="lib"
else
  set_list="lib + ${names[*]}"
fi
printf 'gate-quick: %ss, %s%s (%d of %s test binaries run) - the tree is not proven; the parent runs scripts/verify.sh.\n' \
  "$SECONDS" "$toolchain_note" "$set_list" "$ran" "$total"
