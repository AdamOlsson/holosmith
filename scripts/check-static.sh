#!/usr/bin/env bash
# Assert the dist/ artifacts are the plain static binaries this tree built (ADR-0005),
# carry the real app rather than its placeholder, serve it from an empty root (ADR-0007), and
# write its own schema there (ADR-0009).
#
# Usage: scripts/check-static.sh [--target <triple>]...
# Reads the artifact's `version` and `serve` output, so run scripts/build.sh first. Downloads
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
  [ "$lines" -eq 4 ] || fail "$label: expected 4 lines from 'version', got $lines: $output"
  keys="$(printf '%s\n' "$output" | cut -d' ' -f1 | paste -sd, -)"
  [ "$keys" = "version,git_tag,pi_version,schema_version" ] ||
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

# check_serve_in_empty_root <label> <bwrap args...> -- <inner command...>
#
# Serves the app from the same empty root, fetches `/` and `/api/health` over loopback, and
# requires the served page to be byte-for-byte this tree's web/dist/index.html: the artifact
# ships the app, and the app is this tree's.
check_serve_in_empty_root() {
  local label="$1"
  shift
  local -a prefix=()
  while [ "$1" != "--" ]; do
    prefix+=("$1")
    shift
  done
  shift
  local -a inner=("$@")
  local dir addr port schema_version
  dir="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$dir'" RETURN
  # The data dir is bound into the sandbox, so anything `serve` wrote there lands where this
  # script can see it: the sandbox root is a writable tmpfs, so a check that looked at the host
  # copy without the bind could never fail.
  mkdir -p "$dir/data"

  # --share-net puts the sandbox beside the host's loopback, which is the only way to fetch
  # the port it bound; the filesystem is still empty. `--backend stub` because this run is about the
  # artifact's own linking, not about a machine's choice: a Docker run checks the host first (HS-024),
  # and this sandbox has no daemon, no `git` and no `df` — which is the point of the empty root.
  bwrap "${prefix[@]}" --bind "$dir/data" /data --share-net "${inner[@]}" \
    serve --backend stub --port 0 --data-dir /data >"$dir/serve.log" 2>"$dir/serve.err" &
  local pid=$!
  local i
  for i in $(seq 1 200); do
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

  # While the process runs the database is in WAL, and nothing else is written under the data
  # root. The -wal is the proof the schema was written through the WAL, not around it.
  local running wanted
  running="$(find "$dir/data" -mindepth 1 -maxdepth 1 -type f -printf '%f\n' | sort)"
  for wanted in app.db app.db-wal; do
    printf '%s\n' "$running" | grep -qx "$wanted" ||
      fail "$label in an empty root: '$wanted' is missing while serving: $running"
  done
  local unexpected
  unexpected="$(printf '%s\n' "$running" | grep -vx -e app.db -e app.db-wal -e app.db-shm || true)"
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
  # bwrap exits with the signal's own status (143) instead of forwarding the child's, so that
  # this proves the process stopped; the exit code 0 itself is proved by tests/serve_cli.rs.
  wait "$pid" 2>/dev/null || true

  local reported_version
  reported_version="$(sed -n 's/^version //p' <<<"$plain")"
  case "$health" in
    "{\"name\":\"holosmith\",\"version\":\"$reported_version\",\"role\":\"both\",\"uptime_s\":"*"}") ;;
    *) fail "$label in an empty root: /api/health answered $health" ;;
  esac

  cmp -s "$dir/served.html" web/dist/index.html ||
    fail "$label in an empty root: / is not web/dist/index.html"

  # After the stop the data root holds the database and, because the sandbox reports the
  # signal's own status instead of a clean exit, at most its two WAL sidecars — never anything
  # else, and app.db is still the schema.
  local left
  left="$(printf '%s\n' "$(find "$dir/data" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort)")"
  printf '%s\n' "$left" | grep -qx app.db ||
    fail "$label in an empty root: app.db is gone after the stop: $left"
  unexpected="$(printf '%s\n' "$left" | grep -vx -e app.db -e app.db-wal -e app.db-shm || true)"
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

  if grep -q "$placeholder_marker" "$bin_abs"; then
    fail "$bin carries the placeholder page: it was built without web/dist (run scripts/build.sh)"
  fi

  # Both Dockerfiles are `include_str!`ed into the binary (HS-048), because a deploy runs on a machine that has
  # the artifact and nothing else. Each file's marker comment is the evidence: read from the source, for the
  # placeholder marker's reason — a reworded marker must fail here loudly rather than leave this grep looking for
  # a string no file carries any more.
  for dockerfile in proxy agent; do
    marker="$(sed -n "s/.*# holosmith: embedded \\(.*\\) image (HS-048).*/\\1/p" \
      "crates/holosmith/images/$dockerfile.Dockerfile")"
    [ "$marker" = "$dockerfile" ] ||
      fail "cannot read the $dockerfile marker from crates/holosmith/images/$dockerfile.Dockerfile"
    grep -q "holosmith: embedded $marker image (HS-048)" "$bin_abs" ||
      fail "$bin does not carry the $marker Dockerfile: server deploy's step 3 would have no recipe to build"
  done

  # The migrations are compiled in (ADR-0009): a string literal survives strip and LTO, so its
  # absence means the artifact would start with no schema at all. Greped straight out of the
  # file rather than through `strings | grep -q`, which `pipefail` turns into a failure: grep
  # stops reading at the match and `strings` dies of SIGPIPE.
  grep -q 'CREATE TABLE schema_version' "$bin_abs" ||
    fail "$bin does not carry the initial migration: the migrations were not embedded"

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
    check_serve_in_empty_root "$label" "${bwrap_args[@]}" -- "${inner[@]}"
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
