#!/usr/bin/env bash
# The one command that proves this tree builds, that its code passes, and that the artifact it
# ships serves (ADR-0003).
#
# Usage: scripts/verify.sh [--keep] [--no-build]
# Six phases, cheapest first: cargo fmt --check, cargo clippy --all-targets -- -D warnings,
# cargo test, scripts/build.sh (which runs scripts/build-web.sh and scripts/check-static.sh),
# scripts/check-embed.sh, then serve: dist/<host-triple>/holosmith on a random port with a temp
# config and a temp data dir, answering /api/health, the schema version, the app shell, the 404
# envelope, every browser-facing guard refusal (ADR-0010), the create endpoint's 202/422/409
# answers, the sessions list, one session by id, the session screen's path, and the session's
# event stream, with no CORS header on any answer, and stopping on SIGTERM with exit 0.
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
created_id=""
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

# fetch_body <label> <method> <path> <body> [Header: value]...: the same fetch with a request
# body. `Content-Length` is written explicitly and the body follows the header block: a bare
# `Content-Length` would leave the server waiting for bytes until the read deadline. The length is
# the body's bytes, not its characters, because `${#body}` counts characters and its result is
# locale-dependent: a non-ASCII body would send a short length and hang the reader.
fetch_body() {
  local label="$1" method="$2" path="$3" body="$4"
  shift 4
  local -a sent=("$@")
  local request="$method $path HTTP/1.1"$'\r\n' fd header
  for header in "${sent[@]}"; do
    request+="$header"$'\r\n'
  done
  request+="Content-Length: $(printf '%s' "$body" | wc -c)"$'\r\n'$'Connection: close\r\n\r\n'
  request+="$body"
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

# fetch_stream <label> <path> [Header: value]...: a GET whose answer never ends, so it cannot use
# `fetch`: an event stream sends its header block at once and then nothing until the first event,
# and `fetch` would sit in `timeout 10 cat` until the deadline and then report a failure. This
# reads the header block line by line with bash's own `read -t 3` and stops at the blank line, so
# the read ends when the headers do and nothing is thrown away: an early close, a 404 or a
# plain-text refusal is still saved and asserted like any other answer.
fetch_stream() {
  local label="$1" path="$2"
  shift 2
  local -a sent=("$@")
  local request="GET $path HTTP/1.1"$'\r\n' fd header line
  for header in "${sent[@]}"; do
    request+="$header"$'\r\n'
  done
  request+=$'Connection: close\r\n\r\n'
  exec {fd}<>"/dev/tcp/127.0.0.1/$port" || return 1
  printf '%s' "$request" >&"$fd"
  : >"$WORK/responses/$label.http"
  while IFS= read -r -t 3 line <&"$fd"; do
    printf '%s\n' "$line" >>"$WORK/responses/$label.http"
    # The empty line ends the header block; the body is deliberately not read.
    [ "$line" != $'\r' ] || break
  done
  exec {fd}<&-
  sed '1,/^\r$/d' "$WORK/responses/$label.http" >"$WORK/responses/$label.body"
}

# fetch_sse_until <label> <path> <needle> [Header: value]...: the header block like
# `fetch_stream`, then body lines until one contains <needle>. An event stream never ends, so
# "read until the event that settles the question" is the only bounded way to watch a producer:
# every line has its own three-second timeout and the loop stops after 200 of them, so a stream
# that never sends the needle fails the case instead of hanging the run.
fetch_sse_until() {
  local label="$1" path="$2" needle="$3"
  shift 3
  local -a sent=("$@")
  local request="GET $path HTTP/1.1"$'\r\n' fd header line count=0
  for header in "${sent[@]}"; do
    request+="$header"$'\r\n'
  done
  request+=$'Connection: close\r\n\r\n'
  exec {fd}<>"/dev/tcp/127.0.0.1/$port" || return 1
  printf '%s' "$request" >&"$fd"
  : >"$WORK/responses/$label.http"
  while IFS= read -r -t 3 line <&"$fd"; do
    printf '%s\n' "$line" >>"$WORK/responses/$label.http"
    case "$line" in
      *"$needle"*) break ;;
    esac
    count=$((count + 1))
    [ "$count" -lt 200 ] || break
  done
  exec {fd}<&-
  sed '1,/^\r$/d' "$WORK/responses/$label.http" >"$WORK/responses/$label.body"
}

# expect_header <name> <response> <header> <expected>: one header's value, compared exactly.
expect_header() {
  local name="$1" response="$2" header="$3" expected="$4" value
  value="$(printf '%s\n' "$response" | sed '/^\r$/q' | tr -d '\r' |
    { grep -i "^${header}:" || true; } | sed -n '1s/^[^:]*: *//p')"
  check "$name/$header" "$expected" "$value"
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
  case_events_table
  case_app_shell
  case_api_not_found
  case_guard_bad_host
  case_guard_bad_origin
  case_guard_bad_client
  case_guard_bad_owner
  case_guard_passes
  case_health_is_exempt
  case_create_session
  case_create_is_idempotent
  case_create_bad_project
  case_create_cap_reached
  case_events_stream
  case_backend_from_config
  case_provisioning_steps
  case_state_transition_payload
  case_sessions_list
  case_session_detail
  case_session_screen
  case_prompt_turn
  case_prompt_bad_body
  case_prompt_unknown_session
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

# HS-009's acceptance is the `events` table and the `last_seq` counter it bumps. Same byte grep as
# `case_sessions_table`, and the same glob over `app.db*`: while serve is running the schema may
# still be in `app.db-wal`, and `ALTER TABLE ... ADD COLUMN` rewrites the `sessions` entry in
# `sqlite_master` to carry `last_seq`. The index grep is `sqlite_autoindex_events` rather than a
# named index of our own: SQLite derives that name from the table name and builds it for the
# composite primary key, so it is both stable and the index the range reads actually use (S4).
case_events_table() {
  local name=case_events_table file
  for file in "$WORK"/data/app.db*; do
    [ -f "$file" ] || continue
    if grep -aq 'CREATE TABLE events' "$file" &&
      grep -aq 'sqlite_autoindex_events' "$file" &&
      grep -aq 'last_seq' "$file"; then
      assertions=$((assertions + 1))
      return 0
    fi
  done
  fail "$name/table" \
    "no app.db file carries CREATE TABLE events, the primary key's sqlite_autoindex_events index and sessions.last_seq: $(ls -A "$WORK/data" 2>/dev/null)"
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

# HS-008: a create answers 202 with the row, a `Location` and the event stream's URL. The id is
# parsed out of the body for the cases below; the body is the row, so it carries the status and
# the key's row too.
case_create_session() {
  local name=case_create_session response body
  fetch_body "$name" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-create-1" ||
    fail "$name/no_answer" "POST /api/sessions did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 202
  body="$(cat "$WORK/responses/$name.body")"
  created_id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$created_id" ] ||
    fail "$name/id" "the response names no 16-character id: $body"
  assertions=$((assertions + 1))
  expect_header "$name" "$response" Location "/api/sessions/$created_id"
  case "$body" in
    *'"status":"starting"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/status" "the response is not a starting session: $body"
      ;;
  esac
  case "$body" in
    *"\"events\":\"/api/sessions/$created_id/events\""*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/events" "the response names no event stream: $body"
      ;;
  esac
}

# HS-008: the same key answers the same session with a second 202, not a second row.
case_create_is_idempotent() {
  local name=case_create_is_idempotent response body id
  fetch_body "$name" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-create-1" ||
    fail "$name/no_answer" "POST /api/sessions did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 202
  body="$(cat "$WORK/responses/$name.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  check "$name/id" "$created_id" "$id"
}

# HS-008: a project that is not one path segment is refused, and the code says so.
case_create_bad_project() {
  local name=case_create_bad_project response
  fetch_body "$name" POST /api/sessions \
    '{"project":"..","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-create-2" ||
    fail "$name/no_answer" "POST /api/sessions did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 422
  expect_code "$name" "$response" bad_project
}

# HS-008: one live session against the boot config's cap of one, so a fresh create is refused
# with 409 `cap_reached` (nothing is evictable until HS-020).
case_create_cap_reached() {
  local name=case_create_cap_reached response
  fetch_body "$name" POST /api/sessions \
    '{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-create-3" ||
    fail "$name/no_answer" "POST /api/sessions did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 409
  expect_code "$name" "$response" cap_reached
}

# HS-010: the `events` URL a create answers with is a server-sent event stream. No producer exists
# yet, so the case proves the route and its content type rather than an event; the stream never
# sends EOF, which is why this uses `fetch_stream` and not `fetch`.
case_events_stream() {
  local name=case_events_stream response
  fetch_stream "$name" "/api/sessions/$created_id/events" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions/$created_id/events did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  expect_header "$name" "$response" content-type "text/event-stream"
  expect_header "$name" "$response" cache-control "no-cache"
}

# HS-011: the backend is a machine setting, so a config file saying `backend = "stub"` is what
# this boot runs on, with no flag. The banner is what proves it took effect; the provisioning case
# below is what proves the stub is the backend that actually provisioned the session.
case_backend_from_config() {
  local name=case_backend_from_config banner
  banner="$(sed -n 's/^holosmith .* backend \([a-z]*\)$/\1/p' "$WORK/serve.err" | sed -n '1p')"
  check "$name/banner" "stub" "$banner"
}

# HS-011: a create hands off to the provisioner, which reports the four steps and ends the session
# idle. The stream is read until the event that says `idle`, so the case proves the order a browser
# sees and that the last thing it is told is the state the story requires.
case_provisioning_steps() {
  local name=case_provisioning_steps response body steps
  fetch_sse_until "$name" "/api/sessions/$created_id/events" '"to":"idle"' "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions/$created_id/events did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  body="$(cat "$WORK/responses/$name.body")"
  # `grep -o` rather than a sed range: the four step events carry a step and the fifth carries
  # `null`, so the words alone are the sequence the story asks for.
  steps="$(printf '%s\n' "$body" | grep -o '"step":"[a-z]*"' |
    sed 's/.*:"//; s/"$//' | tr '\n' ' ' | sed 's/ $//')"
  check "$name/steps" "worktree network container pi" "$steps"
  case "$body" in
    *'"to":"idle","step":null'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/idle" "no event reports the session idle: $body"
      ;;
  esac
}

# HS-012: the state machine's `session.status` payload. The four step reports claim no state
# change (`from` and `to` are both `starting`), and the fifth reports the transition the table
# allows and the story requires: `starting` to `idle`, with no step and no reason.
case_state_transition_payload() {
  local name=case_state_transition_payload response body last
  fetch_sse_until "$name" "/api/sessions/$created_id/events" '"to":"idle"' "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions/$created_id/events did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  body="$(cat "$WORK/responses/$name.body")"
  last="$(printf '%s\n' "$body" | grep -o '{"from":[a-z"{}:, -]*}' |
    sed -n '$p')"
  check "$name/last_payload" '{"from":"starting","to":"idle","step":null,"reason":null}' "$last"
  case "$body" in
    *'"from":"starting","to":"starting","step":"worktree"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/step_payload" "a step report does not claim the unchanged state: $body"
      ;;
  esac
}

# HS-013: the sessions list. A `GET`, so only the host check applies, and the body carries the
# session `case_create_session` made plus the machine's network default, which is the create form's
# checkbox initial state. Read last, because it needs `$created_id`.
case_sessions_list() {
  local name=case_sessions_list response body
  fetch "$name" GET /api/sessions "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  expect_header "$name" "$response" content-type "application/json"
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *'"sessions":[{'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/sessions" "the body carries no sessions array with a row: $body"
      ;;
  esac
  case "$body" in
    *"$created_id"*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/session" "the body does not carry $created_id: $body"
      ;;
  esac
  # The boot config does not set `network_default`, so the machine's default is off and the value is
  # pinned: a renamed or inverted field would otherwise still "appear".
  case "$body" in
    *'"network_default":false'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/network_default" "the body does not carry network_default:false: $body"
      ;;
  esac
  # The row's *state* is deliberately not pinned: the provisioner HS-011 starts runs while these
  # cases run, so `starting` or `idle` are both correct by the time this answer is read.
}

# HS-014: one session by id, and the screen path that opens with it. The one row is the list's
# element, so the answer is read against `$created_id` and the absent `events` key is asserted: the
# page spells the stream path itself, and a second copy on the row would be a second contract.
case_session_detail() {
  local name=case_session_detail response body
  fetch "$name" GET "/api/sessions/$created_id" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions/$created_id did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  expect_header "$name" "$response" content-type "application/json"
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *"\"id\":\"$created_id\""*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/id" "the row is not $created_id: $body"
      ;;
  esac
  case "$body" in
    *'"events"'*)
      fail "$name/events" "one row carries no events key: $body"
      ;;
    *)
      assertions=$((assertions + 1))
      ;;
  esac

  # An id the table does not hold is ADR-0008's 404 envelope, code and content type included.
  name=case_session_detail_missing
  fetch "$name" GET /api/sessions/0000000000000000 "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET an unknown session did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" not_found

  # A percent-encoded segment that is not UTF-8 cannot name a row, and must not become axum's
  # `text/plain` 400: it is the same envelope as any id the table does not hold (ADR-0008).
  name=case_session_detail_malformed
  fetch "$name" GET /api/sessions/%FF "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET a non-UTF-8 session id did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" not_found
}

# HS-014: the session screen is a path, not a route. `/sessions/<id>` is not a file the app ships,
# so the fallback answers the app shell (ADR-0007) — byte-for-byte the page `case_app_shell` reads,
# which is what makes a reload of one session's screen work with no router installed.
case_session_screen() {
  local name=case_session_screen response
  fetch "$name" GET "/sessions/$created_id" "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /sessions/$created_id did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  [ -f web/dist/index.html ] ||
    fail "$name/index_html" "web/dist/index.html is missing: run scripts/build.sh"
  cmp -s "$WORK/responses/$name.body" web/dist/index.html ||
    fail "$name/index_html" "the screen path is not this tree's web/dist/index.html"
  assertions=$((assertions + 1))
}

# HS-015: one prompt, one turn. The 202 is the row, now `running`, and the stream read until the
# event that says `idle` is the whole script: a turn boundary, a message's deltas, a tool call and
# its output, the message's end and the closing boundary, then the settle that ends the turn.
case_prompt_turn() {
  local name=case_prompt_turn response body kinds
  fetch_body "$name" POST "/api/sessions/$created_id/prompt" '{"prompt":"read the app shell"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST /api/sessions/$created_id/prompt did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 202
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *'"status":"running"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/status" "the answer is not a running session: $body"
      ;;
  esac

  # The stub paces its turn, so the settle arrives after the 202: read the same session's stream
  # until the event that reports it, and the turn's frames are in the replay before it.
  name=case_prompt_stream
  fetch_sse_until "$name" "/api/sessions/$created_id/events" '"from":"running","to":"idle"' \
    "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions/$created_id/events did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  body="$(cat "$WORK/responses/$name.body")"
  # One pass over the body, one event name per line, so the order is the stream's own: every frame
  # of the turn, in the order the script emitted them.
  kinds="$(printf '%s\n' "$body" | tr -d '\r' | sed -n 's/^event: *//p' |
    grep -E '^(turn\.start|message\.delta|tool\.start|tool\.end|message\.end|turn\.end)$' |
    tr '\n' ' ' | sed 's/ $//')"
  # Seven deltas: three thinking, then two text, then two more after the tool call.
  check "$name/kinds" \
    "turn.start message.delta message.delta message.delta message.delta message.delta tool.start tool.end message.delta message.delta message.end turn.end" \
    "$kinds"
  case "$body" in
    *'"from":"idle","to":"running"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/running" "no event reports the prompt's transition: $body"
      ;;
  esac
  case "$body" in
    *'"from":"running","to":"idle","step":null'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/idle" "no event reports the turn settled: $body"
      ;;
  esac
}

# HS-015: a prompt's body is one non-empty `prompt`, and a body that is not is 422 `bad_body` — the
# same envelope the create's validation answers with.
case_prompt_bad_body() {
  local name=case_prompt_bad_body response
  fetch_body "$name" POST "/api/sessions/$created_id/prompt" '{"prompt":""}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST /api/sessions/$created_id/prompt did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 422
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" bad_body
}

# HS-015: a prompt for a session the table does not hold is ADR-0008's 404 `not_found`, decided
# after the body and before any state is read.
case_prompt_unknown_session() {
  local name=case_prompt_unknown_session response
  fetch_body "$name" POST "/api/sessions/0000000000000000/prompt" '{"prompt":"hello"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST an unknown session's prompt did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" not_found
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

  # The owner the bad_owner case needs, the cap the create cases need, and the backend: `stub`
  # from the *file*, so `--backend` is not what makes this boot work (HS-011's acceptance is both
  # spellings; `tests/serve_cli.rs` covers the flag).
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\n' "$owner" >"$WORK/config.toml"

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
