#!/usr/bin/env bash
# Assert the dist/ artifacts are the plain static binaries this tree built (ADR-0005),
# carry the real app rather than its placeholder, serve it from an empty root (ADR-0007), and
# write its own schema there (ADR-0009).
#
# Usage: scripts/check-static.sh [--target <triple>]...
# Reads the artifact's `version` and `serve` output, so run scripts/build.sh first. Downloads
# nothing: the empty-root run is a container, so a machine with no Docker daemon prints one SKIP line.

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
  targets=(x86_64-unknown-linux-musl)
fi

fail() {
  printf 'FAIL %s\n' "$*" >&2
  exit 1
}

# What a `fail` — which exits the script, leaving the EXIT trap as the only hook — has to take back: the container a
# serve run holds while it runs, the image the empty-root check built, and the directory that image was built from. The
# last two are also removed where that check ends, so a run that passes leaves the machine as it found it (ADR-0108).
empty_root_container="holosmith-check-static-serve-$$"
empty_root_image=""
empty_root_build=""

# Both scratch directories live under target/, because a bind source has to be a path the daemon can see: a
# `mktemp -d` under a tmpfs /tmp becomes an empty root-owned directory with no error anywhere (AGENTS.md). Made once
# here, so each `mktemp` is made from a directory that exists.
mkdir -p "$repo_root/target"
cleanup() {
  docker rm --force "$empty_root_container" >/dev/null 2>&1 || true
  if [ -n "$empty_root_image" ]; then
    docker image rm -f "$empty_root_image" >/dev/null 2>&1 || true
    empty_root_image=""
  fi
  if [ -n "$empty_root_build" ]; then
    rm -rf "$empty_root_build"
    empty_root_build=""
  fi
  return 0
}
trap cleanup EXIT

# The empty-root run is a container, so it needs the two things a container needs. One sentence per machine rather
# than one per target: nothing below this point can measure that run without either.
empty_root_skip=""
if ! command -v docker >/dev/null 2>&1; then
  empty_root_skip="no docker CLI (the empty-root run is a container)"
elif ! docker info >/dev/null 2>&1; then
  empty_root_skip="no Docker daemon answers (the empty-root run is a container)"
fi

# The marker build.rs writes into a placeholder page. Read from the source of truth
# (crates/holosmith/build_support/web_embed.rs) rather than restated, so rewording the constant
# cannot leave this grep silently looking for a string that no longer exists. A shipped
# artifact that contains it was built without web/dist.
placeholder_marker="$(sed -n 's/.*PLACEHOLDER_MARKER: &str = "\(.*\)";/\1/p' \
  crates/holosmith/build_support/web_embed.rs)"
[ -n "$placeholder_marker" ] ||
  fail "cannot read PLACEHOLDER_MARKER from crates/holosmith/build_support/web_embed.rs"

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
  [ "$lines" -eq 5 ] || fail "$label: expected 5 lines from 'version', got $lines: $output"
  keys="$(printf '%s\n' "$output" | cut -d' ' -f1 | paste -sd, -)"
  [ "$keys" = "version,git_tag,release,pi_version,schema_version" ] ||
    fail "$label: unexpected key order: $keys"
  reported="$(printf '%s\n' "$output" | sed -n 's/^pi_version //p')"
  [ "$reported" = "$pin" ] ||
    fail "$label: pi_version $reported does not match pins/pi.txt ($pin)"
}

# fetch <port> <path>: one HTTP/1.1 GET over /dev/tcp, so the check needs no curl. The read has
# a deadline: a connection the server never closes would otherwise hang the whole check.
fetch() {
  local port="$1" path="$2" fd
  exec {fd}<>"/dev/tcp/127.0.0.1/$port" || return 1
  printf 'GET %s HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n' "$path" >&"$fd"
  timeout 10 cat <&"$fd"
  exec {fd}<&-
}

# check_serve_in_empty_root <label> <image> <command...>
#
# Serves the app from the same empty root, fetches `/` and `/api/health` over loopback, and
# requires the served page to be byte-for-byte this tree's web/dist/index.html: the artifact
# ships the app, and the app is this tree's.
check_serve_in_empty_root() {
  local label="$1" image="$2"
  shift 2
  local -a inner=("$@") marker_args=()
  local dir addr port schema_version
  dir="$(mktemp -d "$repo_root/target/check-static-empty-root-XXXXXX")"
  # shellcheck disable=SC2064
  trap "rm -rf '$dir'" RETURN
  # The data dir is bound into the container, so anything `serve` wrote there lands where this
  # script can see it: the image's own root is a writable overlay, so a check that looked at that
  # copy without the bind could never fail.
  mkdir -p "$dir/data"

  # --network host puts the container's listener on the host's loopback, which is the only way
  # `/dev/tcp` can fetch the port it bound; the filesystem is still empty. `--user` because the
  # artifact refuses uid 0 before it parses a verb (ADR-0090) and a scratch image defaults to root.
  # `--backend stub` because this run is about the artifact's own linking, not about a machine's
  # choice: a Docker run checks the host first, and this image has no daemon, no `git` and no `df`
  # — which is the point of the empty root.
  #
  # The gate's run marker goes into the container when the gate is what ran this script, because
  # `--rm` fires only when the serve process ends: a run killed with SIGKILL runs no EXIT trap, so
  # the run this script started would leave a container serving that nothing in this tree names. A
  # container's process is a host process, so with the marker in its environ the sweep a killed
  # run's next run makes (scripts/gate-cleanup.sh, ADR-0089) reaches it, kills it, and `--rm`
  # removes the container. Only when it is set: scripts/build.sh runs this outside any gate.
  if [ -n "${HOLOSMITH_GATE_RUN:-}" ]; then
    marker_args=(--env "HOLOSMITH_GATE_RUN=$HOLOSMITH_GATE_RUN")
  fi
  docker run --rm --name "$empty_root_container" --network host \
    --user "$(id -u):$(id -g)" "${marker_args[@]}" --volume "$dir/data:/data" "$image" "${inner[@]}" \
    serve --backend stub --port 0 --data-dir /data >"$dir/serve.log" 2>"$dir/serve.err" &
  local pid=$!
  for _ in $(seq 1 200); do
    grep -q '^control_listen ' "$dir/serve.err" 2>/dev/null && break
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.05
  done
  addr="$(sed -n 's/^control_listen //p' "$dir/serve.err" | head -1)"
  if [ -z "$addr" ]; then
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    fail "$label in an empty root: serve printed no control_listen line: $(cat "$dir/serve.err")"
  fi
  port="${addr##*:}"

  # The database is the eighth startup line (ADR-0009): the artifact opens and migrates its own
  # schema, at the path item 5 names. The number comes from the `version` output this script
  # already parsed, so a new migration cannot fail here with a message naming the wrong cause.
  schema_version="$(sed -n 's/^schema_version //p' <<<"$plain")"
  grep -qx "database /data/app.db schema $schema_version" "$dir/serve.err" ||
    fail "$label in an empty root: serve did not report the database it opened: $(cat "$dir/serve.err")"

  # The set under the data root, before and after the stop: the database, its WAL sidecars and the admin
  # socket's directory — `<data root>/run`, 0700, made by `serve` when it binds (ADR-0086 D3: it sits on the
  # root's filesystem and outlives the process, and a socket a stop leaves behind is removed at the next bind,
  # `drain::listen`). The -wal is the proof the schema was written through the WAL, not around it.
  #
  # One list for both listings, and each entry carries its type letter, so a directory standing where a file
  # belongs (or the reverse) cannot pass. Before this, the running arm filtered to `-type f` and the stopped arm
  # did not, which is how `run/` slipped past one and failed the other.
  local -a allowed=('app.db f' 'app.db-wal f' 'app.db-shm f' 'run d' 'run/drain.sock s')
  printf '%s\n' "${allowed[@]}" > "$dir/allowed"
  # The socket is bound before the control listener answers (it is bound beside the policies, ahead of the
  # listeners), so the wait above has already covered it; the bound is here so a slow bind is a wait and not a
  # failure that names the wrong cause.
  local wanted running
  for _ in $(seq 1 200); do
    [ -S "$dir/data/run/drain.sock" ] && break
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.05
  done
  running="$(find "$dir/data" -mindepth 1 -maxdepth 2 -printf '%P %y\n' | sort)"
  for wanted in 'app.db f' 'app.db-wal f' 'run d' 'run/drain.sock s'; do
    printf '%s\n' "$running" | grep -qx "$wanted" ||
      fail "$label in an empty root: '$wanted' is missing while serving: $running"
  done
  local unexpected
  unexpected="$(printf '%s\n' "$running" | grep -vxF -f "$dir/allowed" || true)"
  [ -z "$unexpected" ] ||
    fail "$label in an empty root: serve wrote $unexpected under the data dir"

  if ! fetch "$port" / >"$dir/response"; then
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    fail "$label in an empty root: GET / did not answer"
  fi
  if ! fetch "$port" /api/health >"$dir/health.response"; then
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    fail "$label in an empty root: GET /api/health did not answer"
  fi
  sed '1,/^\r$/d' "$dir/response" >"$dir/served.html"
  local health
  health="$(sed '1,/^\r$/d' "$dir/health.response")"
  kill -TERM "$pid" 2>/dev/null || true
  # The client forwards the signal to the container's own process and then exits with its status, so
  # this proves the process stopped and says nothing about how; the exit code 0 itself is proved by
  # tests/serve_cli.rs.
  wait "$pid" 2>/dev/null || true

  local reported_version
  reported_version="$(sed -n 's/^version //p' <<<"$plain")"
  case "$health" in
    "{\"name\":\"holosmith\",\"version\":\"$reported_version\",\"role\":\"both\",\"uptime_s\":"*"}") ;;
    *) fail "$label in an empty root: /api/health answered $health" ;;
  esac

  cmp -s "$dir/served.html" web/dist/index.html ||
    fail "$label in an empty root: / is not web/dist/index.html"

  # After the stop the data root holds the database, at most its two WAL sidecars — a process cut off mid-write
  # leaves them behind — and the admin directory with the socket it bound. app.db is still the schema, and nothing
  # outside the list above was written.
  local left
  left="$(find "$dir/data" -mindepth 1 -maxdepth 2 -printf '%P %y\n' | sort)"
  printf '%s\n' "$left" | grep -qx 'app.db f' ||
    fail "$label in an empty root: app.db is gone after the stop: $left"
  unexpected="$(printf '%s\n' "$left" | grep -vxF -f "$dir/allowed" || true)"
  [ -z "$unexpected" ] ||
    fail "$label in an empty root: after the stop the data dir holds $unexpected"
  printf 'ok %s: serves the app from an empty root\n' "$label"
}

# check_git_tag <label> <output of `version`>
check_git_tag() {
  local label="$1" output="$2" reported
  [ -n "$expected_tag" ] || return 0
  reported="$(printf '%s\n' "$output" | sed -n 's/^git_tag //p')"
  [ "$reported" = "$expected_tag" ] ||
    fail "$label: git_tag $reported is not this tree ($expected_tag): re-run scripts/build.sh"
}

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

  if grep -q "$placeholder_marker" "$bin_abs"; then
    fail "$bin carries the placeholder page: it was built without web/dist (run scripts/build.sh)"
  fi

  # Every Dockerfile is `include_str!`ed into the binary, because a deploy runs on a machine that has
  # the artifact and nothing else. Each file's marker comment is the evidence: read from the source, for the
  # placeholder marker's reason — a reworded marker must fail here loudly rather than leave this grep looking for
  # a string no file carries any more.
  for dockerfile in proxy base agent; do
    marker="$(sed -n "s/.*# holosmith: embedded \\(.*\\) image.*/\\1/p" \
      "crates/holosmith/images/$dockerfile.Dockerfile")"
    [ "$marker" = "$dockerfile" ] ||
      fail "cannot read the $dockerfile marker from crates/holosmith/images/$dockerfile.Dockerfile"
    grep -q "holosmith: embedded $marker image" "$bin_abs" ||
      fail "$bin does not carry the $marker Dockerfile: server deploy's step 3 would have no recipe to build"
  done

  # The migrations are compiled in (ADR-0009): a string literal survives strip and LTO, so its
  # absence means the artifact would start with no schema at all. Greped straight out of the
  # file rather than through `strings | grep -q`, which `pipefail` turns into a failure: grep
  # stops reading at the match and `strings` dies of SIGPIPE.
  grep -q 'CREATE TABLE schema_version' "$bin_abs" ||
    fail "$bin does not carry the initial migration: the migrations were not embedded"

  # Every target this tree builds is the host's own — `.cargo/config.toml` names one linker, and it is this
  # machine's — so there is no emulator to run an artifact under and no `SKIP` where one is missing (ADR-0119).
  label="$target native"

  plain="$("$bin_abs" version)"
  check_version_output "$label" "$plain"
  check_git_tag "$label" "$plain"

  if [ -n "$empty_root_skip" ]; then
    printf 'SKIP %s empty-root run: %s\n' "$target" "$empty_root_skip"
  else
    # An empty root: no /lib, no /usr, nothing but the artifact. The build context is two lines and one copied
    # file, so the image's filesystem *is* the artifact — which is what the probe below asserts rather than leaves
    # implied.
    empty_root_build="$(mktemp -d "$repo_root/target/check-static-image-XXXXXX")"
    install -m 0755 "$bin_abs" "$empty_root_build/holosmith"
    printf 'FROM scratch\nCOPY holosmith /holosmith\n' >"$empty_root_build/Dockerfile"
    inner=(/holosmith)
    empty_root_image="holosmith-check-static-$target:$$"
    docker build -q -t "$empty_root_image" "$empty_root_build" >"$empty_root_build/build.out" \
      2>"$empty_root_build/build.err" ||
      fail "$target: docker could not build the empty-root image: $(cat "$empty_root_build/build.err")"
    # The empty-root claim is that the artifact ran **alone**. `FROM scratch` plus one `COPY` leaves the image no
    # shell to run and no loader to find, so the only program it can execute is the artifact — and a run that asks
    # for the shell a userspace dependency would need says so by failing, naming the file that is not there.
    if docker run --rm --entrypoint /bin/sh "$empty_root_image" -c true \
      >/dev/null 2>"$empty_root_build/shell.err"; then
      fail "$label in an empty root: the image carries a shell, so what ran there is not the artifact alone"
    fi
    grep -q '/bin/sh' "$empty_root_build/shell.err" ||
      fail "$label in an empty root: /bin/sh did not fail as a missing program: $(cat "$empty_root_build/shell.err")"
    sandboxed="$(docker run --rm --user "$(id -u):$(id -g)" "$empty_root_image" "${inner[@]}" version)"
    check_version_output "$label in an empty root" "$sandboxed"
    check_git_tag "$label in an empty root" "$sandboxed"
    [ "$sandboxed" = "$plain" ] ||
      fail "$label: empty-root output differs from the plain run: $sandboxed"
    check_serve_in_empty_root "$label" "$empty_root_image" "${inner[@]}"
    printf 'ok %s: runs with no loader and no userspace dependencies\n' "$target"
    docker image rm -f "$empty_root_image" >/dev/null 2>&1 || true
    empty_root_image=""
    rm -rf "$empty_root_build"
    empty_root_build=""
  fi

  printf '     %s: %s\n' "$bin" "$file_line"
  printf '     %s  %s bytes  %s\n' \
    "$(sha256sum "$bin_abs" | cut -d' ' -f1)" \
    "$(stat -c %s "$bin_abs")" \
    "$("$bin_abs" version --json)"
done
