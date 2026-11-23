#!/usr/bin/env bash
# The whole gate, in a container on this machine's daemon (ADR-0109).
#
# Usage: scripts/verify-in-container.sh [<scripts/verify.sh argument>...]
#
# Every argument and every status is the gate's own: `--keep`, `--no-build`, `--help` and an unknown flag all reach
# `scripts/verify.sh` verbatim, so `--help` prints the gate's usage and this script parses nothing. Exit 0 is a green
# gate, 1 a red one, and 2 is a misuse: this script refusing to start (root, no `docker` CLI, no daemon, no image, no
# workspace version, a tree the daemon cannot see), or `scripts/verify.sh`'s own unknown argument. No operator may read
# a 2 as a red tree — and 125, 126, 127 and 137 are `docker run`'s own codes, for docker failing, a container command
# that is not executable, one that is not there, and a container the kernel killed, so none of those is the gate's
# verdict either.
#
# Seven facts about the `docker run` below are not preferences; each is the one shape that measures, and each was
# measured on this host (Docker 29.9.2, Ubuntu 24.04) — the first six here, and `TMPDIR`'s directory in its own
# paragraph below:
#
# * `--network host`. Four sites dial a container's own address on a Docker network: `case_preview`'s forward,
#   `case_forward`'s container-up check, and `case_server_images`' two proxy checks. `--network none` answers `Network
#   unreachable` at the first of them and a bridge `fetch failed`, because Docker drops container-to-container traffic
#   between bridges; in the host's namespace the same fetch answers `HTTP/1.1 200 OK`.
# * `--group-add`, with the socket's own group. Without it every `docker` call in there answers `permission denied
#   while trying to connect to the docker API`. The group is read from the socket rather than named, because it is
#   the socket's mode that decides it. A gid is only a number, so the machine's **group file** is the next fact: the
#   name that gid has is what `server deploy` reads.
# * the machine's `/etc/group` at its own path. `id -nG` names the groups of the process, and an image whose group file
#   has no entry for the socket's gid prints `node 984` with `id: cannot find name for group ID 984` and exits **1** —
#   measured: `server deploy`'s step 2 then fails naming no groups, because that step compares the words
#   against `docker` (`Account::may_open`) and its `ask()` treats a non-zero status as no answer at all. Measured in a
#   full run as `FAIL case_server_deploy/partial/step2`. Mounted, the same call answers `adamolsson docker` and exits
#   0. `/etc/passwd` stays the image's, and the paragraph at the mount says what mounting it was measured to cost.
# * the `docker` CLI and its CLI-plugin directory, each at its own path. No image ships either, and `docker build` —
#   which `holosmith build`, `case_smoke` and `case_server_images` all run — needs the buildx plugin.
# * the tree at **its own absolute path**, the git dirs and the common dir at theirs, and **every root the gate's cleanup
#   reads** at its own. A container that had the tree at `/repo` asked the daemon to mount `/repo/pins/pi.txt` and got an
#   empty root-owned directory with no error anywhere, because a bind source is resolved in the *host's* namespace
#   (ADR-0035). The tree is not the only path that rule protects: in a linked worktree `.git` is a pointer file to
#   `<common>/worktrees/<name>`, so the git dir and the common dir are two more host paths the daemon resolves, and the
#   gate's own cleanup locks, claims and sweeps under every root `git worktree list` names, which can be a sibling
#   checkout this tree does not contain. All of them are mounted below.
# * the invoking uid and gid, never 0: the artifact refuses a root invocation before it parses argv (ADR-0090), and
#   the session cases assume the gate's uid is theirs.
#
# `scripts/verify.sh`'s work dir is a `mktemp -d` under `TMPDIR`, and a failing run's evidence is that directory, so
# `TMPDIR` has to outlive the container: it is `<repo_root>/target/tmp-verify-in-container` on this machine — created
# here, mounted, and never removed here, so a failed run's work survives this script for the next run to clean up.
# Its **mount point is `/verify-work`**, and that is arithmetic rather than taste: `unix_socket_fits` caps an
# `AF_UNIX` name at 107 bytes and takes `$WORK` out of that budget, this checkout is 74 bytes, and
# `<repo_root>/target/tmp-verify-in-container` is 105 with `mktemp -d` adding fifteen more — so a same-path `TMPDIR`
# under a deep checkout turns fourteen cases into `SKIP`s the host run does not print. The first line below names both
# paths, which is the mapping a failed run's `kept work dir:` line needs (ADR-0109).
#
# `--init` is in because the gate is stopped with signals: without it the container's PID 1 is bash, and bash as PID 1
# was measured ignoring `docker kill --signal TERM` — still `Up` five seconds later — where tini forwarded it and the
# container exited 143. `-i` is out because nothing reads the gate's stdin: every `read` in `scripts/verify.sh` reads a
# redirection (`<&fd`, `<<<`, `< <(...)`), never file descriptor 0.
#
# The environment is not inherited — `docker run` starts from the image's — so what is forwarded is a short list of
# machine-local inputs no case can derive. `HOLOSMITH_TEST_IMAGE` says which image the container cases measure. The
# two emulators are mounted at a fixed path inside and forwarded as **that** path, because it is the file the container
# can execute: `scripts/check-static.sh` takes `HOLOSMITH_QEMU_AARCH64` as a command and guards on `command -v`, so a
# host path would resolve to nothing and the run would print `SKIP aarch64-unknown-linux-musl execution`, which the
# host run does not print. `DOCKER_HOST` is forwarded when it is not a unix socket: a daemon on this machine is reached
# through `--network host` and resolves this tree because the filesystem is the same one, and a remote one ends at the
# tree refusal below. Everything else is the scripts' own (`HOLOSMITH_GATE_RUN`, `HOLOSMITH_REQUIRE_WEB`,
# `HOLOSMITH_GIT_TAG`, `HOLOSMITH_CRATE_SET`, `HOLOSMITH_LOG`, …) and is left for the scripts to set, so no marker, tag
# or flag from outside changes what the gate in there decides.
#
# Two knobs are this script's own: `HOLOSMITH_VERIFY_IMAGE` names the image to run in, defaulting to
# `agent-image:<the version in Cargo.toml>` — the tag `server deploy` builds — and `DOCKER_HOST` is the one way the
# daemon is named, defaulting to `unix:///var/run/docker.sock`. `DOCKER_CONTEXT` is refused rather than resolved: it
# names a daemon by context, and this script has no socket to mount for one, so the container would measure the default
# socket while the checks here measured the context's — the silent wrong answer the paragraph above exists to prevent.
# Run it as the account that owns the tree: the gate writes `target/`, `dist/` and `web/node_modules` as that account,
# and a root invocation is refused below.

set -euo pipefail

# refuse <sentence>: one sentence naming the fix, to stderr, exit 2. 1 is a red gate, so a 2 — this script's refusal,
# or `scripts/verify.sh`'s own unknown argument — can never be read as one.
refuse() {
  printf 'verify-in-container: %s\n' "$1" >&2
  exit 2
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# ---- the refusals, cheapest question first -------------------------------------------------------------
# Root first, because nothing below is worth asking for an invocation the artifact refuses anyway.
[ "$(id -u)" -ne 0 ] ||
  refuse "refusing to run as root, because the artifact refuses a root invocation before it parses argv (ADR-0090): run this as the account that owns $repo_root"

command -v docker >/dev/null 2>&1 ||
  refuse "there is no docker CLI on PATH, so there is nothing here to run the gate in: install Docker, or run scripts/verify.sh on this machine"

docker_host="${DOCKER_HOST:-unix:///var/run/docker.sock}"
# A context is a daemon named rather than addressed, and there is no socket to mount for one. `DOCKER_HOST` wins over it
# when both are set (measured: with the two exported the CLI asked `DOCKER_HOST`'s daemon), so the refusal is for the
# case where the context is the only thing naming a daemon: then the CLI would ask that one and the container would get
# the default socket.
[ -z "${DOCKER_CONTEXT:-}" ] || [ -n "${DOCKER_HOST:-}" ] || [ "$DOCKER_CONTEXT" = default ] ||
  refuse "DOCKER_CONTEXT=$DOCKER_CONTEXT names a daemon this script cannot hand to a container, which would get the default socket instead: unset DOCKER_CONTEXT and point DOCKER_HOST at that daemon's endpoint"

docker info >/dev/null 2>&1 ||
  refuse "no Docker daemon answers at $docker_host: start it, or point DOCKER_HOST at the one the gate should measure against"

# The image's tag is the workspace version, because that is what `server deploy` builds (`images::agent_image`,
# `agent-image:<version>`) — read from the file rather than from the artifact, which need not exist yet.
version="$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml)"
[ -n "$version" ] ||
  refuse "Cargo.toml names no workspace version, so there is no agent-image tag to default to: set HOLOSMITH_VERIFY_IMAGE to the image the gate should run in"
image="${HOLOSMITH_VERIFY_IMAGE:-agent-image:$version}"
docker image inspect "$image" >/dev/null 2>&1 ||
  refuse "the image $image is not on this daemon: run 'holosmith server deploy' to build it, or set HOLOSMITH_VERIFY_IMAGE to the one the gate should run in"

# A tree the daemon cannot see, which is the ticket's own failure: a bind source the daemon resolves to nothing is an
# empty directory rather than an error (ADR-0035), so every case in there would then read a tree that is not the one
# this script is in. The check is the mount itself — one container whose whole job is to report whether the file a
# Holosmith tree cannot exist without is there — because a heuristic on the path would be a guess about a namespace
# only the daemon can answer for. `--network none` because it asks the daemon nothing else, and `/bin/sh` because an
# image without a shell cannot run the gate at all: it reads as this refusal, and its own first line would say so.
docker run --rm --network none \
  --user "$(id -u):$(id -g)" \
  --volume "$repo_root:$repo_root:ro" \
  --workdir "$repo_root" \
  --entrypoint /bin/sh \
  "$image" -c 'test -r Cargo.toml' >/dev/null 2>&1 ||
  refuse "the daemon cannot see $repo_root, so the tree would be an empty directory inside the container: run this on the machine that owns the daemon, from a path its daemon reads"

# ---- the mounts that make the inside the same machine as the outside -----------------------------------
# The host's docker CLI at its own path, and the CLI-plugin directory beside the CLI's exec root — docker searches
# `<exec_root>/lib/docker/cli-plugins` and `<exec_root>/libexec/docker/cli-plugins`, so deriving them from the CLI's
# own location is what a machine spelling them elsewhere needs, and a machine with neither has no buildx for its own
# `docker build` either.
mounts=()
cli="$(command -v docker)"
mounts+=(--volume "$cli:$cli:ro")
exec_root="$(dirname -- "$(dirname -- "$cli")")"
for plugins in "$exec_root/lib/docker/cli-plugins" "$exec_root/libexec/docker/cli-plugins"; do
  if [ -d "$plugins" ]; then
    mounts+=(--volume "$plugins:$plugins:ro")
  fi
done

# The tree's git dirs, because a linked worktree's `.git` is a pointer file to `<common>/worktrees/<name>` and a bind
# source is resolved in the host's namespace (ADR-0035): the tree mount carries the pointer and nothing it points at, so
# `git -C "$repo_root"` in there answers `fatal: not a git repository`, and the two cases that copy the tracked tree
# with `git ls-files` would fail rather than SKIP. Both paths come from git, so a normal checkout — where they are
# inside the tree, which is mounted — needs neither, and each is skipped when it resolves under the tree. Read-write,
# measured: git refreshes the index (`update-index --refresh` rewrote it here), and a read-only mount makes the same
# write `fatal: Unable to create …/index.lock: Read-only file system`, a failure the host run never prints. A tree git
# cannot answer for is left as it was, because that tree fails the host run too.
if git_dir="$(git -C "$repo_root" rev-parse --absolute-git-dir 2>/dev/null)" &&
  git_common_dir="$(git -C "$repo_root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; then
  for git_path in "$git_dir" "$git_common_dir"; do
    case "$git_path/" in
      "$repo_root"/*) ;;
      *) mounts+=(--volume "$git_path:$git_path") ;;
    esac
  done

  # The cleanup's roots too, because the gate's cleanup keeps its state per repository rather than per tree:
  # `scripts/gate-cleanup.sh` locks `<root>/target/gate-cleanup.lock` — one lock per repository, since a run sweeps its
  # sibling worktrees' orphans too — claims its record under a root, and takes those roots from `git worktree list`.
  # So the list is the source here as well, one `--volume` per root that is not already inside the tree and is not a
  # duplicate of one mounted: a normal checkout adds nothing, and a second checkout of this repository under
  # `<main>/../worktrees/<other>` — a path the tree mount does not carry — is mounted at its own.
  #
  # Measured with the tree and both git dirs mounted and the main root not: its first `mkdir -p '<main>/target'`
  # answered `Permission denied` (the path does not exist in there, so nothing can be created under it), `--claim`
  # claimed no record, and `scripts/verify.sh` printed `warn gate-cleanup: no run record was claimed; a killed run of
  # this one will not be cleaned up` — which costs the marker ADR-0089 finds a killed run's children by. A root that is
  # missing altogether is worse than a write that fails: the cleanup refuses at its first root that is not a directory
  # (`not a directory: <path>`, exit 2, empty stdout) *before* the lock and before `--claim` — measured in a probe
  # container with this argv minus one root's `--volume`: `claim rc=2 stdout=[]` and a `not a directory` line — so
  # `HOLOSMITH_GATE_RUN` goes unexported and `case_gate_cleanup`'s `wiring/path` fails: a red gate, not a degraded one.
  # Read-write because everything the cleanup does under a root is a write: the lock file, the record it claims, and the
  # `target/verify-*-<pid>` directories it removes under each root.
  cleanup_roots=()
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    cleanup_roots+=("$line")
  done < <(git -C "$repo_root" worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p')
  # A git that cannot list is the case the common dir's parent is for: the main repository's root, derived the way the
  # cleanup derives it (`<common>/.git` stripped of that suffix), and nothing at all for a common dir of any other
  # shape — which is the tree that cleanup gives no root of its own.
  if [ "${#cleanup_roots[@]}" -eq 0 ]; then
    case "$git_common_dir" in
      */.git) cleanup_roots=("${git_common_dir%/.git}") ;;
    esac
  fi
  mounted_roots=()
  for cleanup_root in "${cleanup_roots[@]}"; do
    case "$cleanup_root/" in
      "$repo_root"/*) continue ;;
    esac
    duplicate=0
    for have in "${mounted_roots[@]}"; do
      [ "$have" = "$cleanup_root" ] && duplicate=1
    done
    [ "$duplicate" -eq 0 ] || continue
    mounted_roots+=("$cleanup_root")
    mounts+=(--volume "$cleanup_root:$cleanup_root")
  done
fi

# The machine's group file, because `--group-add` below hands the container's process a **gid** and `server deploy`'s
# step 2 asks what that gid is *called*: `id -nG` names gid 984 only if `/etc/group` has an entry for it, and without
# one it prints `node 984` with `id: cannot find name for group ID 984` and exits 1 — which `ask()` reads as no answer,
# so step 2 fails `named no groups` and the run stops there. Measured in a full in-container run: `FAIL
# case_server_deploy/partial/step2`, on a host whose socket is gid 984 and whose group file is the only place that gid
# has a name. With this mount the same call answers `adamolsson docker` and exits 0, so `may_open` sees `docker` and the
# status is the machine's. Read-only because nothing here writes it.
#
# `/etc/passwd` is deliberately **not** mounted, and that is measured rather than assumed. It is the file
# `identity::home` reads — "the password entry of the uid, never `$HOME`" (`src/identity.rs`) — so mounting it moves the
# home under every default path from the image's `/home/node` to this operator's `/home/adamolsson`, which is a
# directory docker creates root-owned here as part of the tree's own mount path: `server deploy` with no `--root` then
# answers `install: cannot change permissions of '/home/adamolsson/.local/share/holosmith': Operation not permitted`,
# where the image's passwd gives `/home/node/.local/share/holosmith` and step 2 `changed: 7 actions`. What it would buy
# is `id -un`'s spelling, which only the one printed `usermod` uses (`Account::command`) — and that line is not printed
# on a machine this run can measure, because the container's account is in the socket's group by `--group-add` whether
# the passwd names it or not. Nor does it leave the account read alone: `read_name` asks `id -un` and `ask()` treats a
# non-zero status as no answer, so an invoking uid the image's passwd does not name fails `server deploy`'s step 2
# `named no account` — and `case_server_deploy`'s partial deploy is unshimmed, so such a run prints
# `FAIL case_server_deploy/partial/step2` rather than a SKIP. The image names only `node` (uid 1000), so that is a
# **precondition** of this shape rather than a defect in it, and ADR-0109's "What is not measured" says so.
if [ -f /etc/group ]; then
  mounts+=(--volume /etc/group:/etc/group:ro)
fi

# The socket the container dials, at the conventional path inside, so the CLI in there needs no `DOCKER_HOST`; the
# group is the socket's. An endpoint that is not a unix socket has no file to mount, and the CLI has to be told the
# endpoint instead — the alternative is a container quietly dialling its own default socket.
socket_args=()
case "$docker_host" in
  unix://*)
    socket="${docker_host#unix://}"
    socket_args+=(--group-add "$(stat -c %g "$socket")" --volume "$socket:/var/run/docker.sock")
    ;;
  *)
    socket_args+=(--env "DOCKER_HOST=$docker_host")
    ;;
esac

work_root="$repo_root/target/tmp-verify-in-container"
work_mount=/verify-work
mkdir -p "$work_root"
# The first line, and on stderr so the gate's own report stays the gate's (ADR-0092). Both paths, because a failed
# run's evidence is named by the container's path and lives at this machine's.
printf 'verify-in-container: work dirs under %s on this machine (%s inside the container), kept after a failed run\n' \
  "$work_root" "$work_mount" >&2

forward=(--env "TMPDIR=$work_mount")
if [ -n "${HOLOSMITH_TEST_IMAGE:-}" ]; then
  forward+=(--env "HOLOSMITH_TEST_IMAGE=$HOLOSMITH_TEST_IMAGE")
fi

# The emulators, when this machine has them: no image ships one, and `scripts/check-static.sh` turns a missing one into
# `SKIP <target> execution` — a SKIP the host run does not print. Each is mounted at a fixed path inside and forwarded
# as that path, which is what the container can execute; `command -v` accepts an absolute path and the empty-root run
# copies the runner by path, so a mounted emulator is used exactly as a host one is (measured: `qemu-aarch64 version
# 7.2.0`, and this tree's aarch64 artifact answering `version` under both the mounted and a copied runner).
for suffix in AARCH64 X86_64; do
  case "$suffix" in
    AARCH64) default_emulator=qemu-aarch64-static ;;
    X86_64) default_emulator=qemu-x86_64-static ;;
  esac
  variable="HOLOSMITH_QEMU_$suffix"
  emulator="${!variable:-$default_emulator}"
  emulator_path="$(command -v "$emulator" 2>/dev/null || true)"
  [ -n "$emulator_path" ] || continue
  emulator_inner="/run/verify/$(basename -- "$emulator_path")"
  mounts+=(--volume "$emulator_path:$emulator_inner:ro")
  forward+=(--env "$variable=$emulator_inner")
done

exec docker run --rm --init \
  --network host \
  --user "$(id -u):$(id -g)" \
  "${socket_args[@]}" \
  "${mounts[@]}" \
  --volume "$repo_root:$repo_root" \
  --volume "$work_root:$work_mount" \
  --workdir "$repo_root" \
  "${forward[@]}" \
  "$image" "$repo_root/scripts/verify.sh" "$@"
