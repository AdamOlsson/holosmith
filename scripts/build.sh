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
    printf 'On a host, put the musl cross toolchains on PATH — this tree ships no rust-toolchain.toml to supply them:\n' >&2
    printf '  . ~/.pi/toolchains/env.sh && scripts/build.sh\n' >&2
    printf 'Inside a container (the builder image for `holosmith build`, the agent image for a session) nothing is\n' >&2
    printf 'missing: both cross toolchains are installed there, so a linker absent in that container is an image fault\n' >&2
    printf 'rather than a setup step.\n' >&2
    exit 1
  fi
done
unset -f linker_missing

# The compiler, before anything is compiled: the image that runs this artifact is built from `pins/rust.txt`, and
# an artifact compiled by another rustc is not the one that image holds. The pin is one value holding the tag and
# the digest, so the version is the part before the first `-`; a literal here would be a second copy of it. A patch
# difference is not cosmetic — two rustcs are two compilers — so the whole version has to match, which is stricter
# than `scripts/gate-quick.sh` on purpose: that gate compares the `major.minor` line only, so a patch difference is
# caught here and nowhere else before a release.
rust_reference="$(tr -d '[:space:]' < "$repo_root/pins/rust.txt")"
pinned_rust="${rust_reference%%-*}"
# A `cargo` that is not there answers 127, and `set -e` over the pipeline's `pipefail` ends the script on the
# assignment before the refusal can speak — so the absence is a fact this reads first, as `gate-quick.sh` does.
# The sentence a machine with no toolchain needs is "there is no toolchain", not a status with nothing printed.
cargo_rust=""
if command -v cargo >/dev/null 2>&1; then
  cargo_rust="$(cargo --version 2>/dev/null | awk '{print $2}')"
fi
if [ "$cargo_rust" != "$pinned_rust" ]; then
  if [ -n "$cargo_rust" ]; then
    printf 'build.sh: cargo %s is not the rustc %s that pins/rust.txt holds.\n' \
      "$cargo_rust" "${pinned_rust:-unknown}" >&2
  else
    printf 'build.sh: cargo is not on PATH, so there is no toolchain to compile an artifact with.\n' >&2
    printf 'pins/rust.txt holds the rustc %s the artifact has to be built by.\n' "${pinned_rust:-unknown}" >&2
  fi
  printf 'An artifact compiled by another rustc is not the one the agent image runs, so it is not built here:\n' >&2
  printf '  . ~/.pi/toolchains/env.sh && scripts/build.sh\n' >&2
  printf 'That puts a toolchain first on PATH, which fixes a shadowed cargo and cannot move a version: the file\n' >&2
  printf 'points rustup at a toolchains directory whose default is `stable`, so the compiler moves under\n' >&2
  printf '`rustup update` there — or pin the host to it: rustup toolchain install %s && rustup default %s\n' \
    "$pinned_rust" "$pinned_rust" >&2
  printf 'Inside the builder image (`holosmith build`) the pinned toolchain is the image itself, so a mismatch\n' >&2
  printf 'there is an image fault rather than a setup step.\n' >&2
  exit 1
fi

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
  digest="$(sha256sum "dist/$target/holosmith" | cut -d' ' -f1)"
  # The digest file beside the artifact, in the shape `holosmith build` writes it (`builder::DIGEST`,
  # "<digest>  holosmith", what `sha256sum -c` reads): `promote::read_candidate` refuses a candidate
  # with no sha256sum line beside it, and one artifact layout with two writers is the rule. Written
  # through a temporary and renamed for the same reason the binary is: a truncated digest file is a
  # candidate no promotion can read.
  printf '%s  %s\n' "$digest" "holosmith" > "dist/$target/holosmith.sha256.tmp"
  mv -f "dist/$target/holosmith.sha256.tmp" "dist/$target/holosmith.sha256"
  printf '%s  %s (%s bytes)\n' \
    "$digest" \
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
