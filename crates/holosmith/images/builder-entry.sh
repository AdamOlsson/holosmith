#!/bin/sh
# What the pinned builder container runs. One argument: the target triple.
#
# The source is mounted **read-only** at `/src` and copied into the container's own `/build`, so a build cannot
# write to the operator's tree — not `target/`, not `node_modules/`, and not the repository itself. The copy is the
# same tree minus the three directories a build regenerates, because a stale one from the host would be a build
# whose output nobody can explain.
#
# The build itself is `scripts/build.sh`, unchanged: one recipe for the artifact, run in here instead of on the
# host. `--no-check` because `scripts/check-static.sh` boots the artifact inside a bubblewrap sandbox and fetches
# from loopback — a second namespace this container has no business needing; the two static assertions it makes are
# `holosmith build`'s own (it reads the ELF header of the file that lands) and `case_build`'s.
set -eu

target="${1:?the target triple is the only argument}"
# Set rather than defaulted: `docker run --user <uid>` gives a user with no passwd entry, and the runtime answers
# `HOME=/` for it — which npm reads as its cache directory and then cannot write.
HOME=/build/home
export HOME

mkdir -p /build/src "$HOME"

# `.git` is left behind on purpose: a builder has no history to describe, and `holosmith build` passes the tag the
# source tree has as `HOLOSMITH_GIT_TAG`, which `scripts/build.sh` honours.
tar -C /src \
  --exclude=./target --exclude=./dist --exclude=./web/node_modules --exclude=./web/dist --exclude=./.git \
  -cf - . | tar -C /build/src -xf -
cd /build/src

scripts/build.sh --target "$target" --no-check

# The artifact is written beside its final name and renamed, so a run that fails between the two leaves no file
# that looks like a release. The host hashes these bytes and puts them in place.
cp "dist/$target/holosmith" /out/.holosmith.new
