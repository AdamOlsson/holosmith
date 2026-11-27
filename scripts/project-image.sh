#!/usr/bin/env bash
# The tag of this repository's own project image, and the build that makes it — without a daemon for the first.
#
# Usage: scripts/project-image.sh tag|ensure
#
#   tag      print `hs-holosmith:<digest>` on stdout and nothing else. No daemon and no build: this is the name a
#            session's resolver would compose for this tree, which is what `scripts/verify-in-container.sh` needs
#            before it can hand a container an image.
#   ensure   build that tag with `docker build -f docker/Dockerfile … .` when it is not on the machine, then print
#            the tag. The build's own output goes to **stderr**, so a caller can read stdout for the name.
#
# The digest is `projectimage::tag`'s, composed here a second time: a full SHA-256 over an 8-byte big-endian length
# prefix of the recipe's bytes, the recipe's bytes, and then the base image's tag. The prefix is what makes the split
# between the two unambiguous — `b"a" + "bc"` and `b"ab" + "c"` are one string otherwise — and the base tag is in the
# digest because the same recipe over a moved base is a different image (ADR-0123 D2). `crates/holosmith/tests/images.rs`
# is what holds the two spellings together: it runs `tag` and compares the answer with `projectimage::resolve`'s over
# the same tree, so a digest written differently here fails a test rather than a session.
#
# **`HOLOSMITH_BASE_IMAGE` names the base the tag is composed over**, and it is how a caller hands this tree the base
# its own release deployed: `holosmith update` resolves the candidate tree's recipe against the base image the running
# release has, and passes that here so the tag it names and the tag this script would build are one string. Unset, the
# base is `holosmith-base:<the version in VERSION>` — the image a `server deploy` on this tree builds.
#
# **`HOLOSMITH_RUST_IMAGE` is read from `pins/rust.txt`**, the one copy of the reference: the project recipe has a rust
# stage and declares the argument, and a literal here would be the second copy ADR-0118 removed.
#
# Exit 2 is a refusal — this script's own misuse, a tree it cannot read, a machine with no daemon — and never a red
# gate, so a caller can tell "nothing was measured" from "the tree failed". A build that fails is the build's own
# sentence, on stderr.

set -euo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# refuse <sentence>: one sentence naming the fix, to stderr, exit 2.
refuse() {
  printf 'project-image: %s\n' "$1" >&2
  exit 2
}

# The project's name, which is the repository's own: the tag `projectimage::resolve` would compose for this tree, with
# `env!("CARGO_PKG_NAME")` as the other half of the pair.
project=holosmith
recipe=docker/Dockerfile

[ -f "$recipe" ] || refuse "$recipe is not a file, so this tree has no project image to name: the recipe is what the tag's digest is taken over"
[ -r "$recipe" ] || refuse "$recipe is not readable, so this tree's project image cannot be named: a recipe nobody read is not a recipe that is absent"

if [ -n "${HOLOSMITH_BASE_IMAGE:-}" ]; then
  base="$HOLOSMITH_BASE_IMAGE"
else
  version="$(tr -d '[:space:]' <VERSION 2>/dev/null || true)"
  [ -n "$version" ] ||
    refuse "VERSION names no version, so there is no base tag to compose this tree's project image over: write the version this tree is, or set HOLOSMITH_BASE_IMAGE to the base image the tag should be built on"
  base="holosmith-base:$version"
fi

# The 8-byte big-endian length prefix: `projectimage::tag` writes `(recipe.len() as u64).to_be_bytes()` and then the
# bytes, so each byte is the length shifted down and masked. `%b` is what turns the escapes into the bytes themselves.
bytes="$(wc -c <"$recipe" | tr -d '[:space:]')"
prefix=""
for shift in 7 6 5 4 3 2 1 0; do
  prefix+="$(printf '\\x%02x' $(((bytes >> (shift * 8)) & 0xff)))"
done

digest="$(
  {
    printf '%b' "$prefix"
    cat "$recipe"
    printf '%s' "$base"
  } | sha256sum | cut -d' ' -f1
)"
tag="hs-$project:$digest"

case "${1:-}" in
  tag)
    printf '%s\n' "$tag"
    ;;
  ensure)
    if docker image inspect "$tag" >/dev/null 2>&1; then
      printf '%s\n' "$tag"
      exit 0
    fi
    command -v docker >/dev/null 2>&1 ||
      refuse "there is no docker CLI on PATH, so $tag cannot be built: install Docker, or run the command this one stands in for on a machine that has it"
    rust_image="$(tr -d '[:space:]' <pins/rust.txt 2>/dev/null || true)"
    [ -n "$rust_image" ] ||
      refuse "pins/rust.txt names no reference, so the rust stage of $recipe has no base to build from: restore the pin, which is the one copy of the reference"
    # The two arguments the recipe declares, and only those: the base it is composed over and the pinned rust stage.
    # `1>&2` because stdout is the answer a caller reads, and a build's own output is progress rather than an answer.
    docker build \
      -f "$recipe" \
      -t "$tag" \
      --build-arg "HOLOSMITH_BASE_IMAGE=$base" \
      --build-arg "HOLOSMITH_RUST_IMAGE=$rust_image" \
      . 1>&2 ||
      refuse "the build of $tag failed, and its own output above says where: nothing was tagged, so a gate that needed this image has not run"
    printf '%s\n' "$tag"
    ;;
  *)
    refuse "usage: scripts/project-image.sh tag|ensure — the first prints this tree's project image tag and the second builds it when it is missing"
    ;;
esac
