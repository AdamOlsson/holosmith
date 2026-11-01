#!/usr/bin/env bash
# Assert the dist/ artifacts are the plain static binaries this tree built (ADR-0005).
#
# Usage: scripts/check-static.sh [--target <triple>]...
# Reads the artifact's `version` output, so run scripts/build.sh first. Downloads
# nothing: qemu-aarch64-static is a machine-local tool (docs/build.md) and its absence
# prints one SKIP line instead of failing.

set -euo pipefail

# Prints this script's header comment, whatever its line count.
usage() {
  sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

targets=()
while [ $# -gt 0 ]; do
  case "$1" in
    --target)
      [ $# -ge 2 ] || { echo "check-static.sh: --target needs a value" >&2; exit 2; }
      targets+=("$2")
      shift 2
      ;;
    --target=*)
      targets+=("${1#--target=}")
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "check-static.sh: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
if [ ${#targets[@]} -eq 0 ]; then
  targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
fi

fail() {
  printf 'FAIL %s\n' "$*" >&2
  exit 1
}

pin="$(tr -d '[:space:]' < pins/pi.txt)"
[ -n "$pin" ] || fail "pins/pi.txt is empty"

# Criterion 4: the build fetches nothing but the crates.io dependencies in Cargo.lock.
if grep -q 'source = "git+' Cargo.lock; then
  fail "Cargo.lock carries a git source: the build would fetch it"
fi

# A stale dist/ from an older commit passes every other assertion, so compare the tag the
# artifact reports with the tag of this tree. Builder containers copy the source without
# .git and pass HOLOSMITH_GIT_TAG instead, so there the check degrades to one SKIP line.
expected_tag=""
if [ -d .git ] && command -v git >/dev/null 2>&1; then
  expected_tag="$(git describe --tags --always --dirty 2>/dev/null || true)"
fi
if [ -z "$expected_tag" ]; then
  printf 'SKIP git_tag check: no git tag for %s\n' "$repo_root"
fi

# check_version_output <label> <output of `version`>
check_version_output() {
  local label="$1" output="$2" lines keys reported
  lines="$(printf '%s\n' "$output" | wc -l)"
  [ "$lines" -eq 4 ] || fail "$label: expected 4 lines from 'version', got $lines: $output"
  keys="$(printf '%s\n' "$output" | cut -d' ' -f1 | paste -sd, -)"
  [ "$keys" = "version,git_tag,pi_version,schema_version" ] ||
    fail "$label: unexpected key order: $keys"
  reported="$(printf '%s\n' "$output" | sed -n 's/^pi_version //p')"
  [ "$reported" = "$pin" ] ||
    fail "$label: pi_version $reported does not match pins/pi.txt ($pin)"
}

# check_git_tag <label> <output of `version`>
check_git_tag() {
  local label="$1" output="$2" reported
  [ -n "$expected_tag" ] || return 0
  reported="$(printf '%s\n' "$output" | sed -n 's/^git_tag //p')"
  [ "$reported" = "$expected_tag" ] ||
    fail "$label: git_tag $reported is not this tree ($expected_tag): re-run scripts/build.sh"
}

host_arch="$(uname -m)"

for target in "${targets[@]}"; do
  bin="dist/$target/holosmith"
  bin_abs="$repo_root/$bin"
  [ -x "$bin_abs" ] || fail "missing $bin: run scripts/build.sh"

  file_line="$(file -b "$bin_abs")"
  case "$file_line" in
    *"statically linked"*) ;;
    *) fail "$bin is not statically linked: $file_line" ;;
  esac
  if ! segments="$(readelf -l "$bin_abs" 2>&1)"; then
    fail "$bin: readelf -l failed: $segments"
  fi
  case "$segments" in
    *INTERP*) fail "$bin has a PT_INTERP segment: it needs a dynamic loader" ;;
  esac
  if ! dynamic="$(readelf -d "$bin_abs" 2>&1)"; then
    fail "$bin: readelf -d failed: $dynamic"
  fi
  case "$dynamic" in
    *"There is no dynamic section in this file"*) ;;
    *) fail "$bin has a dynamic section: $dynamic" ;;
  esac

  arch="${target%%-*}"
  runner=()
  if [ "$arch" = "$host_arch" ]; then
    label="$target native"
  else
    case "$target" in
      aarch64-unknown-linux-musl) emulator="${HOLOSMITH_QEMU_AARCH64:-qemu-aarch64-static}" ;;
      x86_64-unknown-linux-musl) emulator="${HOLOSMITH_QEMU_X86_64:-qemu-x86_64-static}" ;;
      *) emulator="" ;;
    esac
    if [ -n "$emulator" ] && command -v "$emulator" >/dev/null 2>&1; then
      runner=("$(command -v "$emulator")")
      label="$target via $(basename "$emulator")"
    else
      printf 'SKIP %s execution: %s not found (see docs/build.md)\n' \
        "$target" "${emulator:-an emulator}"
      printf '     %s: %s\n' "$bin" "$file_line"
      continue
    fi
  fi

  plain="$("${runner[@]}" "$bin_abs" version)"
  check_version_output "$label" "$plain"
  check_git_tag "$label" "$plain"

  if command -v bwrap >/dev/null 2>&1; then
    # An empty root: no /lib, no /usr, nothing but the artifact (and the emulator).
    bwrap_args=(--unshare-all --die-with-parent --tmpfs / --proc /proc
      --ro-bind "$bin_abs" /holosmith)
    inner=(/holosmith)
    if [ ${#runner[@]} -gt 0 ]; then
      emulator_in_root="/$(basename "${runner[0]}")"
      bwrap_args+=(--ro-bind "${runner[0]}" "$emulator_in_root")
      inner=("$emulator_in_root" /holosmith)
    fi
    sandboxed="$(bwrap "${bwrap_args[@]}" "${inner[@]}" version)"
    check_version_output "$label in an empty root" "$sandboxed"
    check_git_tag "$label in an empty root" "$sandboxed"
    [ "$sandboxed" = "$plain" ] ||
      fail "$label: empty-root output differs from the plain run: $sandboxed"
    printf 'ok %s: runs with no loader and no userspace dependencies\n' "$target"
  else
    printf 'SKIP %s empty-root run: bwrap not found (see docs/build.md)\n' "$target"
  fi

  printf '     %s: %s\n' "$bin" "$file_line"
  printf '     %s  %s bytes  %s\n' \
    "$(sha256sum "$bin_abs" | cut -d' ' -f1)" \
    "$(stat -c %s "$bin_abs")" \
    "$("${runner[@]}" "$bin_abs" version --json)"
done
