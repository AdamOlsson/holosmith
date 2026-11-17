#!/usr/bin/env bash
# Build the static musl artifacts and copy them to dist/<target-triple>/holosmith.
#
# Usage: scripts/build.sh [--target <triple>]... [--no-check]
# Needs the musl cross toolchains; on a host that keeps them under ~/.pi/toolchains this
# script finds them itself, and any other host puts them on PATH first (AGENTS.md).

set -euo pipefail

# Prints this script's header comment, whatever its line count.
usage() {
  sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

targets=()
run_check=1
while [ $# -gt 0 ]; do
  case "$1" in
    --target)
      [ $# -ge 2 ] || { echo "build.sh: --target needs a value" >&2; exit 2; }
      targets+=("$2")
      shift 2
      ;;
    --target=*)
      targets+=("${1#--target=}")
      shift
      ;;
    --no-check)
      run_check=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "build.sh: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
if [ ${#targets[@]} -eq 0 ]; then
  targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl)
fi

# The musl cross linkers are named by .cargo/config.toml, so a musl target cannot be built
# without them. A reader who clones this tree has no reason to know the `. ~/.pi/toolchains/env.sh`
# line, and the failure it prevents is a linker-not-found that reads like a code error — so when a
# requested musl target's linker is missing, source that file if it is there. It is a no-op under
# verify.sh and in the builder container, where PATH already carries them, and wherever it cannot
# help the sentence below names the fix rather than letting cargo fail first.
linker_for() {
  case "$1" in
    *-unknown-linux-musl) printf '%s-linux-musl-gcc\n' "${1%%-*}" ;;
    *) printf '\n' ;;
  esac
}

linker_missing() {
  local linker
  linker="$(linker_for "$1")"
  [ -n "$linker" ] && ! command -v "$linker" >/dev/null 2>&1
}

if [ -f "$HOME/.pi/toolchains/env.sh" ]; then
  for target in "${targets[@]}"; do
    if linker_missing "$target"; then
      # shellcheck disable=SC1091
      . "$HOME/.pi/toolchains/env.sh"
      break
    fi
  done
fi

for target in "${targets[@]}"; do
  if linker_missing "$target"; then
    printf 'build.sh: %s is not on PATH, and %s needs it.\n' \
      "$(linker_for "$target")" "$target" >&2
    printf 'Put the musl cross toolchains on PATH; on a host with ~/.pi/toolchains, run it as:\n' >&2
    printf '  . ~/.pi/toolchains/env.sh && scripts/build.sh\n' >&2
    exit 1
  fi
done
unset -f linker_missing

# A release artifact must report the tag it was built from, never a stale one. Builder
# containers have no .git, so there the tag arrives in this variable instead — and a caller
# that set it wins, because a copy of the source without history cannot describe itself
#
# Never export RUSTFLAGS here (ADR-0005): it would disable the per-target rustflags in
# .cargo/config.toml and silently ship a static-PIE binary.
HOLOSMITH_GIT_TAG="${HOLOSMITH_GIT_TAG:-$(git describe --tags --always --dirty 2>/dev/null || echo unknown)}"
export HOLOSMITH_GIT_TAG

printf 'git tag: %s\n' "$HOLOSMITH_GIT_TAG"
printf 'targets: %s\n' "${targets[*]}"

# The app is built first and embedded in the binary ("Build and release"). From here on a
# missing web/dist is a build failure, never a silently shipped placeholder (ADR-0007).
"$repo_root/scripts/build-web.sh"
export HOLOSMITH_REQUIRE_WEB=1

for target in "${targets[@]}"; do
  echo "building $target"
  cargo build --release --locked --target "$target"
  # Copy beside the artifact and rename, so an interrupted run never leaves a truncated
  # binary at the path the promotion pipeline reads.
  mkdir -p "dist/$target"
  cp "target/$target/release/holosmith" "dist/$target/holosmith.tmp"
  mv -f "dist/$target/holosmith.tmp" "dist/$target/holosmith"
  printf '%s  %s (%s bytes)\n' \
    "$(sha256sum "dist/$target/holosmith" | cut -d' ' -f1)" \
    "dist/$target/holosmith" \
    "$(stat -c %s "dist/$target/holosmith")"
done

if [ "$run_check" -eq 1 ]; then
  check_args=()
  for target in "${targets[@]}"; do
    check_args+=(--target "$target")
  done
  "$repo_root/scripts/check-static.sh" "${check_args[@]}"
fi
