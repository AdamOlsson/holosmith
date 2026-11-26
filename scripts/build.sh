#!/usr/bin/env bash
# Build the artifact this tree ships: the web app, then cargo, then scripts/check-static.sh on a release.
#
# Usage: scripts/build.sh [--profile dev|release] [--target <triple>]... [--no-check] [--require-clean-tree]
#
#   --profile dev|release  release (the default) writes dist/<triple>/holosmith and its .sha256 beside it;
#                          dev writes target/<triple>/dev/holosmith and never touches dist/
#   --target <triple>      repeatable; the default is x86_64-unknown-linux-musl, the one release target
#   --no-check             skip scripts/check-static.sh (a dev build implies it: that script reads dist/)
#   --require-clean-tree   refuse a build whose git tag would end in -dirty
#
# The musl cross linker a target needs comes from PATH first, then from this tree's toolchains/, and is fetched
# from the URL crates/holosmith/images/agent.Dockerfile pins when neither holds it.

set -euo pipefail

# Prints this script's header comment, whatever its line count.
usage() {
  sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Two refusals, told apart by status: 1 is the tree or the machine, and 2 is this invocation — a caller loops on 2.
refuse() {
  printf 'build.sh: %s\n' "$1" >&2
  exit 1
}

misuse() {
  printf 'build.sh: %s\n' "$1" >&2
  printf 'usage: scripts/build.sh [--profile dev|release] [--target <triple>]... [--no-check] [--require-clean-tree]\n' >&2
  exit 2
}

# The one release target. A second architecture returns through `--target`, which is why the flag is repeatable
# rather than this list being the whole of what the script understands; every value is checked against it below,
# so a triple that is not built is refused by name instead of failing later as a missing linker (ADR-0119).
supported_targets=(x86_64-unknown-linux-musl)

profile=release
run_check=1
require_clean_tree=0
targets=()
while [ $# -gt 0 ]; do
  case "$1" in
    --profile)
      [ $# -ge 2 ] || misuse "--profile needs a value: dev or release"
      profile="$2"
      shift 2
      ;;
    --profile=*)
      profile="${1#--profile=}"
      shift
      ;;
    --target)
      [ $# -ge 2 ] || misuse "--target needs a value"
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
    --require-clean-tree)
      require_clean_tree=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      misuse "unknown argument: $1"
      ;;
  esac
done

case "$profile" in
  release | dev) ;;
  *) misuse "--profile is dev or release, not $profile" ;;
esac
# A dev build never writes dist/, and check-static.sh reads dist/ — so the check is not a choice there.
if [ "$profile" = dev ]; then
  run_check=0
fi

if [ ${#targets[@]} -eq 0 ]; then
  targets=("${supported_targets[@]}")
fi

# Before any toolchain work, so the refusal costs nothing and names the triple rather than leaving a linker-not-found
# to speak for it. A repeated triple is built once: the argument is a set, and building it twice says nothing.
validated=()
for target in "${targets[@]}"; do
  case " ${supported_targets[*]} " in
    *" $target "*) ;;
    *)
      misuse "$target is not a supported release target: this build makes ${supported_targets[*]} only"
      ;;
  esac
  case " ${validated[*]-} " in
    *" $target "*) ;;
    *) validated+=("$target") ;;
  esac
done
targets=("${validated[@]}")

# The machine, before the compiler check and before the web build: one release target is also one architecture, and a
# host that is not it cross-builds an artifact it can then neither run nor check — `scripts/check-static.sh` executes
# the artifact, so the ending would be the shell's "cannot execute binary file" after a full build (ADR-0119). One
# sentence here is the boundary stated rather than discovered, and it names the machine's own answer.
machine="$(uname -m)"
case "$machine" in
  x86_64 | amd64) ;;
  *)
    refuse "this machine answers $machine to uname -m, and this tree builds x86_64-unknown-linux-musl only: the \
artifact could not be run here until the supported set carries $machine's target again"
    ;;
esac

# A release artifact must report the tag it was built from, never a stale one. Builder
# containers have no .git, so there the tag arrives in this variable instead — and a caller
# that set it wins, because a copy of the source without history cannot describe itself
#
# Never export RUSTFLAGS here (ADR-0005): it would disable the per-target rustflags in
# .cargo/config.toml and silently ship a static-PIE binary.
HOLOSMITH_GIT_TAG="${HOLOSMITH_GIT_TAG:-$(git describe --tags --always --dirty 2>/dev/null || echo unknown)}"
export HOLOSMITH_GIT_TAG

# Before the compiler check and before the web build, because a dirty tree costs a millisecond here and several
# minutes there. The tag is the artifact's own claim about itself, and `-dirty` is a claim no second build can
# reproduce: a promotion plants such a binary under a tag, so a release build may refuse one outright.
if [ "$require_clean_tree" -eq 1 ]; then
  case "$HOLOSMITH_GIT_TAG" in
    *-dirty)
      refuse "this tree is dirty, so the artifact would report $HOLOSMITH_GIT_TAG, and a binary nobody can reproduce is not a release"
      ;;
  esac
fi

# The compiler, before anything is compiled: the image that runs this artifact is built from `pins/rust.txt`, and
# an artifact compiled by another rustc is not the one that image holds. The pin is one value holding the tag and
# the digest, so the version is the part before the first `-`; a literal here would be a second copy of it. A patch
# difference is not cosmetic — two rustcs are two compilers — so the whole version has to match, which is stricter
# than `scripts/gate-quick.sh` on purpose: that gate compares the `major.minor` line only, so a patch difference is
# caught here and nowhere else before a release. The driver and the compiler are two answers, and the artifact is
# the pin's only when both are.
rust_reference="$(tr -d '[:space:]' < "$repo_root/pins/rust.txt")"
pinned_rust="${rust_reference%%-*}"
for program in cargo rustc; do
  # A program that is not there answers 127, and `set -e` over the pipeline's `pipefail` ends the script on the
  # assignment before the refusal can speak — so the absence is a fact this reads first, as `gate-quick.sh` does.
  # The sentence a machine with no toolchain needs is "there is no toolchain", not a status with nothing printed.
  program_rust=""
  if command -v "$program" >/dev/null 2>&1; then
    program_rust="$("$program" --version 2>/dev/null | awk '{print $2}')"
  fi
  if [ "$program_rust" = "$pinned_rust" ]; then
    continue
  fi
  if [ -n "$program_rust" ]; then
    printf 'build.sh: %s --version answers %s, and pins/rust.txt holds the rustc %s this artifact is built by.\n' \
      "$program" "$program_rust" "${pinned_rust:-unknown}" >&2
  else
    printf 'build.sh: %s is not on PATH, so there is no toolchain to compile an artifact with.\n' "$program" >&2
    printf 'pins/rust.txt holds the rustc %s the artifact has to be built by.\n' "${pinned_rust:-unknown}" >&2
  fi
  printf 'An artifact compiled by another rustc is not the one the agent image runs, so it is not built here:\n' >&2
  printf '  . ~/.pi/toolchains/env.sh && scripts/build.sh\n' >&2
  printf 'That puts a toolchain first on PATH, which fixes a shadowed program and cannot move a version: the file\n' >&2
  printf 'points rustup at a toolchains directory whose default is `stable`, so the compiler moves under\n' >&2
  printf '`rustup update` there — or pin the host to it: rustup toolchain install %s && rustup default %s\n' \
    "$pinned_rust" "$pinned_rust" >&2
  printf 'Inside the builder image (`holosmith build`) the pinned toolchain is the image itself, so a mismatch\n' >&2
  printf 'there is an image fault rather than a setup step.\n' >&2
  exit 1
done

# The musl cross linker is named by .cargo/config.toml, whose one target is the triple's own first field — so a musl
# target cannot be built without its linker, and the failure a reader would otherwise get is a linker-not-found that
# reads like a code error.
linker_for() {
  case "$1" in
    *-unknown-linux-musl) printf '%s-linux-musl-gcc\n' "${1%%-*}" ;;
    *) printf '\n' ;;
  esac
}

# The pinned tarball for one architecture, into `toolchains/`. The URL and the digest are read out of the recipe
# that fetches the same bytes at image-build time, so the digest has one copy in the tree and a bump of it is one
# edit both readers follow (ADR-0120).
fetch_toolchain() {
  local arch="$1" url digest tarball recipe
  recipe="$repo_root/crates/holosmith/images/agent.Dockerfile"
  url="https://musl.cc/$arch-linux-musl-cross.tgz"
  # The range is the `RUN` that fetches this architecture's tarball, and the value is the `echo "…" | sha256sum -c
  # -` line inside it: a recipe whose fetch changes shape leaves this empty, which is the refusal below rather
  # than a fetch nothing checked.
  digest="$(sed -n "/$arch-linux-musl-cross\\.tgz/,/sha256sum -c -/p" "$recipe" \
    | sed -n 's/.*echo "\([0-9a-f]\{64\}\) .*/\1/p' | head -1)"
  [ -n "$digest" ] ||
    refuse "$recipe pins no digest for $arch-linux-musl-cross.tgz, so the fetch could not be checked"

  mkdir -p "$repo_root/toolchains"
  tarball="$repo_root/toolchains/$arch-linux-musl-cross.tgz"
  printf 'fetching %s\n' "$url"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 4 --retry-delay 2 --retry-all-errors -o "$tarball" "$url" || {
      rm -f "$tarball"
      refuse "$url did not answer, so there is no $arch-linux-musl-gcc to build with"
    }
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$tarball" "$url" || {
      rm -f "$tarball"
      refuse "$url did not answer, so there is no $arch-linux-musl-gcc to build with"
    }
  else
    refuse "neither curl nor wget is on PATH, so $url cannot be fetched; put $arch-linux-musl-gcc on PATH instead"
  fi
  # The digest is what makes the fetch safe, so it is checked before anything is extracted: a mismatched tarball is
  # refused and leaves nothing behind that a later run could mistake for a toolchain.
  if ! printf '%s  %s\n' "$digest" "$tarball" | sha256sum -c - >/dev/null 2>&1; then
    rm -f "$tarball"
    refuse "$url's bytes do not match the digest $recipe pins, so nothing was extracted"
  fi
  tar -C "$repo_root/toolchains" -xzf "$tarball"
  rm -f "$tarball"
}

# PATH first and always: a session's container carries the linker already and has no network, so the fetch is never
# reached there. This tree's `toolchains/` is second, and nothing outside the tree is ever searched.
resolve_linker() {
  local target="$1" arch linker bundled
  arch="${target%%-*}"
  linker="$(linker_for "$target")"
  [ -n "$linker" ] || refuse "$target names no musl cross linker for .cargo/config.toml"
  if command -v "$linker" >/dev/null 2>&1; then
    return 0
  fi
  bundled="$repo_root/toolchains/$arch-linux-musl-cross/bin"
  if [ ! -x "$bundled/$linker" ]; then
    fetch_toolchain "$arch"
  fi
  PATH="$bundled:$PATH"
}

for target in "${targets[@]}"; do
  resolve_linker "$target"
  printf 'linker: %s\n' "$(command -v "$(linker_for "$target")")"
done

printf 'git tag: %s\n' "$HOLOSMITH_GIT_TAG"
printf 'profile: %s\n' "$profile"
printf 'targets: %s\n' "${targets[*]}"

# The app is built first and embedded in the binary ("Build and release"). From here on a
# missing web/dist is a build failure, never a silently shipped placeholder (ADR-0007).
"$repo_root/scripts/build-web.sh"
export HOLOSMITH_REQUIRE_WEB=1

cargo_args=()
if [ "$profile" = release ]; then
  cargo_args=(--release)
else
  cargo_args=(--profile dev)
fi

for target in "${targets[@]}"; do
  echo "building $target"
  cargo build "${cargo_args[@]}" --locked --target "$target"
  # Copy beside the artifact and rename, so an interrupted run never leaves a truncated
  # binary at the path the promotion pipeline reads.
  if [ "$profile" = dev ]; then
    # cargo names the dev profile's output directory `debug`, and the artifact is placed where the profile's name
    # says instead: `dist/` is what a promotion reads, so a debug path is the one place a mistake cannot reach it.
    mkdir -p "target/$target/dev"
    cp "target/$target/debug/holosmith" "target/$target/dev/holosmith.tmp"
    mv -f "target/$target/dev/holosmith.tmp" "target/$target/dev/holosmith"
    printf '%s\n' "$repo_root/target/$target/dev/holosmith"
  else
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
  fi
done

if [ "$run_check" -eq 1 ]; then
  check_args=()
  for target in "${targets[@]}"; do
    check_args+=(--target "$target")
  done
  "$repo_root/scripts/check-static.sh" "${check_args[@]}"
fi
