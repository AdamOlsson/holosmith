#!/usr/bin/env bash
# The one command that proves this tree builds, that its code passes, and that the artifact it
# ships serves (ADR-0003).
#
# Usage: scripts/verify.sh [--keep] [--no-build]
# Six phases, cheapest first: cargo fmt --check, cargo clippy --all-targets -- -D warnings,
# cargo test, scripts/build.sh (which runs scripts/build-web.sh and scripts/check-static.sh),
# scripts/check-embed.sh, then serve: dist/<host-triple>/holosmith on a random port with a temp
# config and a temp data dir, answering /api/health, the schema version, the app shell, the 404
# envelope and every browser-facing guard refusal (ADR-0010), with no CORS header on any answer,
# and stopping on SIGTERM with exit 0.
# --no-build skips the two artifact phases; --keep keeps the work dir of a successful run. On
# failure this prints FAIL <assertion> and keeps the work dir (config, banner, every answer).
# Downloads nothing and needs neither curl nor jq: every request is a /dev/tcp fetch. Put the
# toolchains on PATH first (docs/build.md).

set -euo pipefail

# Prints this script's header comment, whatever its line count.
usage() {
  sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}' "${BASH_SOURCE[0]}"
}

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

keep=0
build=1
while [ $# -gt 0 ]; do
  case "$1" in
    --keep)
      keep=1
      shift
      ;;
    --no-build)
      build=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "verify.sh: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

# The owner the bad_owner case needs. Every other setting is the default: --port and --data-dir
# override the two that would otherwise point at this machine.
owner="verify@example"

WORK="$(mktemp -d)"
mkdir -p "$WORK/responses"
serve_pid=""
assertions=0

# The only cleanup path: the work dir is removed on success, kept on failure (and with --keep),
# and a server a failure left running is stopped here rather than outliving the run.
cleanup() {
  local status=$?
  if [ -n "$serve_pid" ] && kill -0 "$serve_pid" 2>/dev/null; then
    kill -TERM "$serve_pid" 2>/dev/null || true
    wait "$serve_pid" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] || [ "$keep" -eq 1 ]; then
    printf 'work dir kept: %s\n' "$WORK" >&2
    return 0
  fi
  rm -rf "$WORK"
  return 0
}
trap cleanup EXIT

# fail <assertion> [detail]: the failure contract. The EXIT trap prints the work dir after this.
fail() {
  local assertion="$1" detail="${2:-}"
  printf 'FAIL %s\n' "$assertion" >&2
  [ -z "$detail" ] || printf '     %s\n' "$detail" >&2
  exit 1
}

# A phase, named in the assertion it fails under, so a broken build says FAIL build/failed.
run_phase() {
  local phase="$1"
  shift
  printf '== %s ==\n' "$phase"
  "$@" || fail "$phase/failed" "$* exited non-zero"
  printf 'ok %s: %s\n' "$phase" "$*"
}

# check <name> <expected> <actual>
check() {
  local name="$1" expected="$2" actual="$3"
  [ "$actual" = "$expected" ] || fail "$name" "expected '$expected', got '$actual'"
  assertions=$((assertions + 1))
}

# expect_status <name> <response> <code>: parses the status line.
expect_status() {
  local name="$1" response="$2" expected="$3" status
  status="$(printf '%s\n' "$response" |
    sed -n '1s/^HTTP\/1\.[01] \([0-9][0-9][0-9]\).*/\1/p')"
  check "$name/status" "$expected" "$status"
}

# expect_code <name> <response> <code>: the `code` of the ADR-0008 envelope, read with sed so the
# harness needs no jq.
expect_code() {
  local name="$1" response="$2" expected="$3" code
  code="$(printf '%s\n' "$response" |
    sed '1,/^\r$/d' |
    sed -n 's/.*"code": *"\([A-Za-z0-9_]*\)".*/\1/p')"
  check "$name/code" "$expected" "$code"
}

# expect_no_cors <name> <response>: no access-control-* header, and no Vary that implies one.
expect_no_cors() {
  local name="$1" response="$2" headers
  headers="$(printf '%s\n' "$response" | sed '/^\r$/q' | tr -d '\r')"
  if printf '%s\n' "$headers" | grep -qi '^access-control-'; then
    fail "$name/no_cors" "the answer carried a CORS header: $headers"
  fi
  if printf '%s\n' "$headers" | grep -qi '^vary:.*origin'; then
    fail "$name/no_cors" "the answer carried Vary: origin: $headers"
  fi
  assertions=$((assertions + 1))
}

# fetch <label> <method> <path> [Header: value]...: one HTTP/1.1 request over /dev/tcp, so the
# harness needs no curl. The answer is saved as $WORK/responses/<label>.http with the body alone
# in <label>.body, and the read has a deadline: a connection the server never closes would
# otherwise hang the run.
fetch() {
  local label="$1" method="$2" path="$3"
  shift 3
  local -a sent=("$@")
  local request="$method $path HTTP/1.1"$'\r\n' fd header
  for header in "${sent[@]}"; do
    request+="$header"$'\r\n'
  done
  request+=$'Connection: close\r\n\r\n'
  exec {fd}<>"/dev/tcp/127.0.0.1/$port" || return 1
  printf '%s' "$request" >&"$fd"
  if ! timeout 10 cat <&"$fd" >"$WORK/responses/$label.http"; then
    exec {fd}<&-
    return 1
  fi
  exec {fd}<&-
  sed '1,/^\r$/d' "$WORK/responses/$label.http" >"$WORK/responses/$label.body"
}

# The answer fetch saved for <label>: status line, headers and body.
answer() {
  cat "$WORK/responses/$1.http"
}

# Every answer a browser gets from the booted artifact: one function per case, run in this
# order. Adding a case is one case_* function and one line in serve_cases, and every later
# story adds at least one (docs/build.md).
#
# "No CORS header, ever" is deliberately *not* one of them: it sweeps every answer the cases
# above saved, so it runs after the loop instead of being a member of it. That way a case
# appended by a later story is covered without anyone having to remember to place it before
# this one.
serve_cases=(
  case_health
  case_schema_version
  case_sessions_table
  case_app_shell
  case_api_not_found
  case_guard_bad_host
  case_guard_bad_origin
  case_guard_bad_client
  case_guard_bad_owner
  case_guard_passes
  case_health_is_exempt
)

# The health payload: exactly four keys, and the uptime is not predictable, so the assertion is
# the prefix and the closing brace (the shape tests/http_contract.rs uses).
case_health() {
  local name=case_health response content_type body expected
  fetch "$name" GET /api/health "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/health did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  content_type="$(printf '%s\n' "$response" | sed '/^\r$/q' | tr -d '\r' |
    sed -n 's/^[Cc]ontent-[Tt]ype: *//p')"
  check "$name/content_type" "application/json" "$content_type"
  body="$(cat "$WORK/responses/$name.body")"
  expected="{\"name\":\"holosmith\",\"version\":\"$version\",\"role\":\"both\",\"uptime_s\":"
  case "$body" in
    "$expected"*"}")
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/body" "health answered $body"
      ;;
  esac
}

# The schema the artifact reports is the one `version --json` reports, at the data root this run
# gave it. One exact line, because a wrong path means `--data-dir` was ignored and the run would
# have opened /var/lib/holosmith instead (ADR-0009).
case_schema_version() {
  local name=case_schema_version banner database
  database="$WORK/data/app.db"
  banner="$(grep -m1 '^database ' "$WORK/serve.err" || true)"
  check "$name/banner" "database $database schema $schema_version" "$banner"
  [ -f "$database" ] ||
    fail "$name/app_db" "serve did not write $database: $(ls -A "$WORK/data" 2>/dev/null)"
  assertions=$((assertions + 1))
}

# HS-007's acceptance is "migration adds `sessions`", and nothing else proves the booted
# artifact created it: `case_schema_version` derives the number the artifact reports, which a
# migration bump alone would satisfy. The table's SQL lives in SQLite's `sqlite_master`, so the
# database's own bytes carry it and no `sqlite3` binary is needed — the same byte grep
# scripts/check-static.sh makes for `CREATE TABLE schema_version`. The glob covers the WAL
# sidecars, because while serve is running the schema may still be in `app.db-wal`.
case_sessions_table() {
  local name=case_sessions_table file
  for file in "$WORK"/data/app.db*; do
    [ -f "$file" ] || continue
    if grep -aq 'CREATE TABLE sessions' "$file" &&
      grep -aq 'sessions_status' "$file"; then
      assertions=$((assertions + 1))
      return 0
    fi
  done
  fail "$name/table" \
    "no app.db file carries CREATE TABLE sessions and the sessions_status index: $(ls -A "$WORK/data" 2>/dev/null)"
}

case_app_shell() {
  local name=case_app_shell response
  fetch "$name" GET / "Host: 127.0.0.1" || fail "$name/no_answer" "GET / did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  [ -f web/dist/index.html ] ||
    fail "$name/index_html" "web/dist/index.html is missing: run scripts/build.sh"
  cmp -s "$WORK/responses/$name.body" web/dist/index.html ||
    fail "$name/index_html" "GET / is not this tree's web/dist/index.html"
  assertions=$((assertions + 1))
}

case_api_not_found() {
  local name=case_api_not_found response
  fetch "$name" GET /api/nope "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/nope did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_code "$name" "$response" not_found
}

# Box 1 of HS-005: the `Host` header is not one this machine answers. Only the host is wrong.
case_guard_bad_host() {
  local name=case_guard_bad_host response
  fetch "$name" GET /api/nope "Host: evil.example" ||
    fail "$name/no_answer" "GET /api/nope did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 403
  expect_code "$name" "$response" bad_host
}

# Box 2: a request that changes something arrives without an `Origin`.
case_guard_bad_origin() {
  local name=case_guard_bad_origin response
  fetch "$name" POST /api/nope "Host: 127.0.0.1" "Content-Length: 0" ||
    fail "$name/no_answer" "POST /api/nope did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 403
  expect_code "$name" "$response" bad_origin
}

# Box 3: the origin is the control one and the body is JSON, but there is no client header.
case_guard_bad_client() {
  local name=case_guard_bad_client response
  fetch "$name" POST /api/nope "Host: 127.0.0.1" "Content-Length: 0" \
    "Origin: $control_origin" "Content-Type: application/json" ||
    fail "$name/no_answer" "POST /api/nope did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 403
  expect_code "$name" "$response" bad_client
}

# Box 5: every earlier check is correct, so the code that comes back is the owner's. Getting a
# header wrong here would assert another guard's code under this case's name.
case_guard_bad_owner() {
  local name=case_guard_bad_owner response
  fetch "$name" POST /api/nope "Host: 127.0.0.1" "Content-Length: 0" \
    "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: someone@example" ||
    fail "$name/no_answer" "POST /api/nope did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 403
  expect_code "$name" "$response" bad_owner
}

# The same request with the owner's own login passes all four checks and reaches the router,
# which has no such route yet: 404 is what passing looks like today.
case_guard_passes() {
  local name=case_guard_passes response
  fetch "$name" POST /api/nope "Host: 127.0.0.1" "Content-Length: 0" \
    "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST /api/nope did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_code "$name" "$response" not_found
}

# `is_guard_exempt` is the one exemption list, so the health probe answers a foreign `Host`.
case_health_is_exempt() {
  local name=case_health_is_exempt response
  fetch "$name" GET /api/health "Host: evil.example" ||
    fail "$name/no_answer" "GET /api/health did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
}

# Box 4, over every answer the cases above saved: none of them carries a CORS header, and none
# carries a `Vary` that would imply one. Runs after the case loop rather than as a member of it,
# so nothing answered above is missed and a case appended later is covered automatically.
case_no_cors_header() {
  local file label
  for file in "$WORK"/responses/*.http; do
    label="$(basename "$file" .http)"
    expect_no_cors "$label" "$(cat "$file")"
  done
}

start_serve() {
  # --port 0 asks the kernel for the port, and it is also what makes serve re-derive
  # control_origin from the bound address (ADR-0007): the passing case can then send exactly
  # the origin the guard compares. --data-dir keeps the run out of /var/lib/holosmith.
  "$artifact" serve --config "$WORK/config.toml" --port 0 --data-dir "$WORK/data" \
    >"$WORK/serve.out" 2>"$WORK/serve.err" &
  serve_pid=$!
  for _ in $(seq 1 200); do
    grep -q '^control_listen ' "$WORK/serve.err" 2>/dev/null && break
    kill -0 "$serve_pid" 2>/dev/null || break
    sleep 0.05
  done

  # Both read from the banner, never assumed: the port the kernel gave, and the origin the
  # guard compares.
  control_listen="$(sed -n 's/^control_listen //p' "$WORK/serve.err" | sed -n '1p')"
  [ -n "$control_listen" ] ||
    fail "serve/banner" "serve printed no control_listen line: $(cat "$WORK/serve.err")"
  port="${control_listen##*:}"
  control_origin="$(sed -n 's/^control_origin //p' "$WORK/serve.err" | sed -n '1p')"
  [ -n "$control_origin" ] ||
    fail "serve/banner" "serve printed no control_origin line: $(cat "$WORK/serve.err")"
  printf 'ok serve/banner: listening on %s, control origin %s\n' "$control_listen" "$control_origin"
}

# SIGTERM is the stop signal systemd sends, and the artifact exits 0 on it (ADR-0007). No
# sandbox sits in between here, so unlike scripts/check-static.sh this can assert the code.
stop_serve() {
  kill -TERM "$serve_pid" 2>/dev/null ||
    fail "serve/sigterm" "could not signal the server (pid $serve_pid)"
  local status=0
  wait "$serve_pid" || status=$?
  serve_pid=""
  check "serve/exit" 0 "$status"
}

phase_serve() {
  printf '== serve ==\n'
  case "$(uname -m)" in
    x86_64) host_target=x86_64-unknown-linux-musl ;;
    aarch64) host_target=aarch64-unknown-linux-musl ;;
    *) fail "serve/artifact" "no shipped artifact for $(uname -m): docs/build.md names the two" ;;
  esac
  # The shipped musl artifact, not target/debug: it is the thing that ships, and the one
  # scripts/check-static.sh has already proved. A debug binary would embed whatever web/dist
  # existed when cargo last ran.
  artifact="$repo_root/dist/$host_target/holosmith"
  [ -x "$artifact" ] ||
    fail "serve/artifact" "missing dist/$host_target/holosmith: run scripts/build.sh"

  # What the artifact says about itself, before it serves: the health payload's version and the
  # schema version the banner must report both come from here.
  local json
  json="$("$artifact" version --json)" ||
    fail "serve/version" "$artifact version --json failed"
  version="$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' <<<"$json")"
  schema_version="$(sed -n 's/.*"schema_version":\([0-9][0-9]*\).*/\1/p' <<<"$json")"
  if [ -z "$version" ] || [ -z "$schema_version" ]; then
    fail "serve/version" "cannot read version and schema_version from: $json"
  fi

  # The owner the bad_owner case needs, and the only reason a config file exists at all.
  printf 'owner = "%s"\n' "$owner" >"$WORK/config.toml"

  start_serve
  local case_fn
  for case_fn in "${serve_cases[@]}"; do
    "$case_fn"
  done
  # After every case, so the sweep covers all of them and any case a later story appends.
  case_no_cors_header
  stop_serve
  printf 'ok serve: %d cases + the CORS sweep against dist/%s/holosmith, stopped on SIGTERM with exit 0\n' \
    "${#serve_cases[@]}" "$host_target"
}

command -v cargo >/dev/null 2>&1 ||
  fail "cargo/path" "cargo is not on PATH: . ~/.pi/toolchains/env.sh first (docs/build.md)"

run_phase fmt cargo fmt --check
run_phase clippy cargo clippy --all-targets -- -D warnings
run_phase test cargo test
if [ "$build" -eq 1 ]; then
  # scripts/build.sh runs scripts/build-web.sh first and scripts/check-static.sh last, so this
  # calls it once rather than running npm twice.
  run_phase build scripts/build.sh
  run_phase embed scripts/check-embed.sh
else
  printf 'SKIP build and embed: --no-build (the artifact must be in dist/ already)\n'
fi
phase_serve

printf 'verify: %d assertions, ok\n' "$assertions"
