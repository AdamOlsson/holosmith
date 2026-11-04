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
# answers, the sessions list, one session by id, the session screen's path, the session's event
# stream, a prompt's turn, a timeout that does not fire on a short turn, an abort down to
# `abort_completed`, a steer's own frame, a resume's two
# refusals, a delete from its 202 to the row that is gone, an idle session stopped by the sweeper with
# its reason, its event and its log line, a create at the cap evicting the idle session before it
# succeeds, and a token budget on a boot of its own ending the turn that spends it, with no CORS header
# on any answer, and stopping on SIGTERM with exit 0.
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
serve_err="$WORK/serve.err"
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
# every line has its own three-second timeout, so a stalled stream fails the case after one of them
# instead of hanging the run. The line budget is 800 rather than 200 because a stream replays the
# session's whole log from seq 1 and the log grows with every turn this run takes: by the abort and
# steer cases the shared session has had four or five turns, which is 300-odd lines, so a budget sized
# for the log as it was at HS-019 would end the read before the frame the case came for. A stream that
# has not settled after 200 events would be a bug of its own, and 200 events is what 800 lines is.
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
    [ "$count" -lt 800 ] || break
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
  case_events_stream
  case_backend_from_config
  case_provisioning_steps
  case_state_transition_payload
  case_sessions_list
  case_session_detail
  case_session_screen
  case_prompt_turn
  case_turn_timeout
  case_prompt_bad_body
  case_prompt_unknown_session
  case_abort
  case_abort_on_an_idle_session
  case_steer
  case_steer_on_an_idle_session
  case_resume_on_an_idle_session
  case_resume_unknown_session
  case_delete_without_the_client_header
  case_delete_session
  case_idle_stop
  case_delete_unknown_session
  case_create_at_cap_evicts
  case_create_at_cap_refused
  case_token_budget
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
  banner="$(grep -m1 '^database ' "$serve_err" || true)"
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
# HS-008/HS-020: a create at the cap is refused when there is nothing to evict, and since HS-020 it is
# the *last* case because it owns the session whose state it depends on: a session that is still
# provisioning is live and not evictable, and leaving one behind would break any case that came after.
# The first create evicts the previous case's idle session (the cap is one), so the session that holds
# the slot is the one this case just made — one round trip old, against a first provisioning step of
# 200 ms.
case_create_at_cap_refused() {
  local name=case_create_at_cap_refused response body holder
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'

  fetch_body "${name}_holder" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-cap-1" ||
    fail "${name}_holder/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_holder")"
  expect_status "${name}_holder" "$response" 202
  body="$(cat "$WORK/responses/${name}_holder.body")"
  holder="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$holder" ] || fail "${name}_holder/id" "the response names no id: $body"

  fetch "${name}_holder_row" GET "/api/sessions/$holder" "Host: 127.0.0.1" ||
    fail "${name}_holder_row/no_answer" "GET /api/sessions/$holder did not answer"
  body="$(cat "$WORK/responses/${name}_holder_row.body")"
  case "$body" in
    *'"status":"starting"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_holder_row/status" \
        "the refusal needs a live session that is not idle, and this one is not starting: $body"
      ;;
  esac

  fetch_body "$name" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-cap-2" ||
    fail "$name/no_answer" "POST /api/sessions did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 409
  expect_code "$name" "$response" cap_reached
  # The sentence says why nothing was evicted, which is the create's own answer since HS-020.
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *idle*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/message" "the refusal does not say that nothing was idle: $body"
      ;;
  esac
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
  banner="$(sed -n 's/^holosmith .* backend \([a-z]*\)$/\1/p' "$serve_err" | sed -n '1p')"
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

# HS-022: the turn timeout does not fire on a turn that ends. The gate cannot make it *fire* —
# `turn_timeout_min` is whole minutes and the stub's turn lasts about a second, so nothing this artifact
# can be configured with holds a turn open for a minute — and the story's own proof of firing is
# `tests/turn_timeout.rs`'s. What the shipped binary can prove is the other half: with the key set to its
# smallest value, two prompts of one session each settle normally with no reason recorded, and the runner
# wrote no deadline line. Two prompts rather than one because each is a fresh run of the runner: a second
# turn that could not start would show up as a 409 here.
case_turn_timeout() {
  local name=case_turn_timeout response body turn

  for turn in 1 2; do
    fetch_body "${name}_$turn" POST "/api/sessions/$created_id/prompt" '{"prompt":"read the app shell"}' \
      "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
      "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
      fail "${name}_$turn/no_answer" "POST /api/sessions/$created_id/prompt did not answer"
    response="$(answer "${name}_$turn")"
    expect_status "${name}_$turn" "$response" 202

    # The row, not a stream: every turn's settle frame carries the same payload, so a needle would be
    # answered by the *previous* turn's replay (docs/build.md's stream rule). The row is this turn's, and
    # a fired deadline would have written `turn_timeout` into `status_reason` — which is exactly what
    # `"status_reason":null` rules out.
    wait_for_idle_id "${name}_idle_$turn" "$created_id"
    fetch "${name}_row_$turn" GET "/api/sessions/$created_id" "Host: 127.0.0.1" ||
      fail "${name}_row_$turn/no_answer" "GET /api/sessions/$created_id did not answer"
    body="$(cat "$WORK/responses/${name}_row_$turn.body")"
    case "$body" in
      *'"status":"idle","status_reason":null'*)
        assertions=$((assertions + 1))
        ;;
      *)
        fail "${name}_row_$turn/reason" \
          "the turn ended with a reason, and a one-minute deadline cannot have fired: $body"
        ;;
    esac
  done

  # And the runner said nothing about a deadline, which is the runner's own witness.
  if grep -q 'the turn ran past its timeout' "$serve_err"; then
    fail "${name}/fired" \
      "a deadline of a minute fired on turns that end in a second: $(grep 'holosmith::turn' "$serve_err" | sed -n '1,5p')"
  fi
  assertions=$((assertions + 1))
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

# HS-016: abort answers 202 with the running row, the ladder ends the turn off-request, and the
# reason on the transition says which rung fired. The stub honours an abort, so this is rung 1 —
# rung 2 is a constructor argument only a test can set, and `tests/abort.rs` drives it.
case_abort() {
  local name=case_abort response body
  # A turn to abort: the prompt's own turn has settled by the time the previous case returns.
  fetch_body "case_abort_prompt" POST "/api/sessions/$created_id/prompt" \
    '{"prompt":"read the app shell"}' "Host: 127.0.0.1" "Origin: $control_origin" \
    "Content-Type: application/json" "X-Holosmith-Client: 1" \
    "Tailscale-User-Login: $owner" ||
    fail "case_abort_prompt/no_answer" "POST /api/sessions/$created_id/prompt did not answer"
  response="$(answer case_abort_prompt)"
  expect_status case_abort_prompt "$response" 202

  # No body at all, which is what the endpoint takes; the content type is still sent because the
  # guards read it on every request that changes something (ADR-0010).
  fetch "$name" POST "/api/sessions/$created_id/abort" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST /api/sessions/$created_id/abort did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 202
  expect_header "$name" "$response" content-type "application/json"
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *'"status":"running"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/status" "the answer is not the running row a 202 promises: $body"
      ;;
  esac

  # The ladder's progress is events, so the proof is the stream: the turn ends `running -> idle`
  # with the abort's own reason, which is how a client reads which rung fired.
  name=case_abort_stream
  fetch_sse_until "$name" "/api/sessions/$created_id/events" '"reason":"abort_completed"' \
    "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions/$created_id/events did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *'"from":"running","to":"idle","step":null,"reason":"abort_completed"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/transition" "the abort's transition is not in the stream: $body"
      ;;
  esac
}

# HS-016: an abort with no turn to abort is `409 session_idle` — the code the family gained for it —
# because aborting an idle session is not a request that silently does nothing.
case_abort_on_an_idle_session() {
  local name=case_abort_on_an_idle_session response
  fetch "$name" POST "/api/sessions/$created_id/abort" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST an idle session's abort did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 409
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" session_idle
}

# HS-017: a steer hands guidance to the running turn, answers 202 with the row, and is recorded as a
# `turn.steer` frame — which is how the transcript shows what was delivered and when. The abort left the
# session idle, so this case starts its own turn.
case_steer() {
  local name=case_steer response body
  fetch_body "case_steer_prompt" POST "/api/sessions/$created_id/prompt" \
    '{"prompt":"read the app shell"}' "Host: 127.0.0.1" "Origin: $control_origin" \
    "Content-Type: application/json" "X-Holosmith-Client: 1" \
    "Tailscale-User-Login: $owner" ||
    fail "case_steer_prompt/no_answer" "POST /api/sessions/$created_id/prompt did not answer"
  response="$(answer case_steer_prompt)"
  expect_status case_steer_prompt "$response" 202

  fetch_body "$name" POST "/api/sessions/$created_id/steer" '{"guidance":"and the lexer too"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST /api/sessions/$created_id/steer did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 202
  expect_header "$name" "$response" content-type "application/json"
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *'"status":"running"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/status" "the answer is not the running row: $body"
      ;;
  esac

  # The frame the transcript reads, and then the proof that the turn continued: the same stream
  # carries the settle the stub was going to produce anyway.
  name=case_steer_stream
  fetch_sse_until "$name" "/api/sessions/$created_id/events" '"text":"and the lexer too"' \
    "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions/$created_id/events did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 200
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *'event: turn.steer'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/kind" "the steer's frame is not on the stream: $body"
      ;;
  esac

  # The turn continued: the steer did not end it, and the row reaches idle on its own. The row is the
  # question rather than a stream needle, because a settle's payload is the same shape for every turn
  # and the replay would answer with the previous turn's.
  wait_for_idle case_steer_settles
}

# HS-017: a steer with no running turn is `409 session_idle`, the code HS-016 added for it.
case_steer_on_an_idle_session() {
  local name=case_steer_on_an_idle_session response
  fetch_body "$name" POST "/api/sessions/$created_id/steer" '{"guidance":"too late"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST an idle session's steer did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 409
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" session_idle
}

# wait_for_idle <label>: poll `created_id`'s row until it says idle, bounded and counted once.
wait_for_idle() {
  wait_for_idle_id "$1" "$created_id"
}

# wait_for_idle_id <label> <id>: the same for any session, which HS-020's eviction case needs because
# it makes its own pair rather than using `created_id`. A case that needs a session idle before it
# acts cannot use a stream needle: every turn's settle carries the same payload, and the replay would
# answer with an earlier turn's.
wait_for_idle_id() {
  local label="$1" id="$2" body
  for _ in $(seq 1 100); do
    fetch "$label" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "$label/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/$label.body")"
    case "$body" in
      *'"status":"idle"'*)
        assertions=$((assertions + 1))
        return 0
        ;;
    esac
    sleep 0.1
  done
  fail "$label/status" "the session never reached idle: $body"
}

# HS-018: a resume with nothing to bring back is `409 session_idle`. The happy path cannot be driven
# from this harness: nothing in the API produces `stopped` yet — the policies that do are HS-020's,
# HS-021's and HS-044's — so `tests/resume.rs` proves the round trip against a row the table stopped.
case_resume_on_an_idle_session() {
  local name=case_resume_on_an_idle_session response
  fetch "$name" POST "/api/sessions/$created_id/resume" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST an idle session's resume did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 409
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" session_idle
}

# HS-018: a resume for a session the table does not hold is ADR-0008's 404 `not_found`.
case_resume_unknown_session() {
  local name=case_resume_unknown_session response
  fetch "$name" POST "/api/sessions/0000000000000000/resume" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "POST an unknown session's resume did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" not_found
}

# wait_for_gone <label> <id>: poll one session's row until it answers `404 not_found`, bounded.
#
# The removal is a background task's (HS-019), so the 202 is not the proof that it happened: the row
# is. Counted once, like every other wait here, so a passing case adds one assertion rather than one
# per poll.
wait_for_gone() {
  local label="$1" id="$2" response
  for _ in $(seq 1 100); do
    fetch "$label" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "$label/no_answer" "GET /api/sessions/$id did not answer"
    response="$(answer "$label")"
    if printf '%s\n' "$response" | sed -n '1s/^HTTP\/1\.[01] 404.*/found/p' | grep -q found; then
      assertions=$((assertions + 1))
      return 0
    fi
    sleep 0.1
  done
  fail "$label/status" "the session was never removed: $response"
}

# wait_for_reason_id <label> <id> <reason>: poll one session's row until it is `idle` *and* carries
# that reason, bounded and counted once.
#
# The writer commits every 25 ms, so "the row answered" and "the log can be read" are different
# moments; this waits on the pair the case goes on to assert, so a passing case cannot have read a
# body it did not mean. The reason is a word, not a sentence: no test pins a message (`http/error.rs`).
wait_for_reason_id() {
  local label="$1" id="$2" reason="$3" body
  for _ in $(seq 1 100); do
    fetch "$label" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "$label/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/$label.body")"
    if printf '%s\n' "$body" | grep -q '"status":"idle"' &&
      printf '%s\n' "$body" | grep -q "\"status_reason\":\"$reason\""; then
      assertions=$((assertions + 1))
      return 0
    fi
    sleep 0.1
  done
  fail "$label/reason" "the session never reached idle with reason $reason: $body"
}

# wait_for_stopped_id <label> <id>: poll one session's row until it says stopped, bounded and counted
# once. The idle stop is a timer's, not a request's (HS-021), so its budget is the config's bound plus
# one tick rather than a fraction of a second: 100 seconds for the gate's one-minute `idle_stop_min`.
wait_for_stopped_id() {
  local label="$1" id="$2" body
  for _ in $(seq 1 100); do
    fetch "$label" GET "/api/sessions/$id" "Host: 127.0.0.1" ||
      fail "$label/no_answer" "GET /api/sessions/$id did not answer"
    body="$(cat "$WORK/responses/$label.body")"
    case "$body" in
      *'"status":"stopped"'*)
        assertions=$((assertions + 1))
        return 0
        ;;
    esac
    sleep 1
  done
  fail "$label/status" "the session never reached stopped: $body"
}

# remove_session <label> <id>: delete a session and wait for its row to be gone, so a case that made
# one of its own leaves the machine as it found it.
remove_session() {
  local label="$1" id="$2" response
  fetch "$label" DELETE "/api/sessions/$id" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$label/no_answer" "DELETE /api/sessions/$id did not answer"
  response="$(answer "$label")"
  expect_status "$label" "$response" 202
  wait_for_gone "${label}_gone" "$id"
}

# HS-019: the delete flow's happy path, against the stub backend whose snapshot and delete both
# succeed. The 202 promises a `deleting` row and the removal happens off the request, so the proof is
# two answers: the 202 itself, and the 404 that follows it. This case runs last among the cases that
# name `created_id`, because it is the one that removes it.
case_delete_session() {
  local name=case_delete_session response body
  fetch "$name" DELETE "/api/sessions/$created_id" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "DELETE /api/sessions/$created_id did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 202
  expect_header "$name" "$response" content-type "application/json"
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *'"status":"deleting"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/status" "the answer is not the deleting row a 202 promises: $body"
      ;;
  esac

  wait_for_gone "${name}_gone" "$created_id"

  # Nothing a client can read names the session any more: not one row, and not the list.
  fetch "$name" GET /api/sessions "Host: 127.0.0.1" ||
    fail "$name/no_answer" "GET /api/sessions did not answer"
  expect_status "$name" "$(answer "$name")" 200
  body="$(cat "$WORK/responses/$name.body")"
  case "$body" in
    *"$created_id"*)
      fail "$name/listed" "the removed session is still listed: $body"
      ;;
    *'"sessions":[]'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "$name/listed" "the list is not the empty collection: $body"
      ;;
  esac
}

# HS-021: the idle stop. A session left alone stops costing money, and it is the artifact's *own*
# timer that does it — no request asks for this — so the case creates one session, waits out the
# one-minute `idle_stop_min` the config sets, and asserts four things: the row is `stopped` with the
# reason, the transition is in the transcript a client replays, the sweeper wrote its one log line, and
# the slot the session held is free.
#
# "Frees a slot" is asserted *without* a second policy in the way: a `stopped` row holds no slot, so the
# next create at the cap of one simply inserts. If the sweep had not run, that row would still be `idle`,
# the create would evict it with reason `evicted`, and the re-read below would say so — which is why the
# re-read is the assertion.
#
# `idle_stop_min = 1` is the *process's* setting, so every case in this run has a sweeper behind it: no
# case may leave a session idle for a minute (ADR-0025). This one waits a minute on purpose, which is
# why it runs where the table is otherwise empty.
#
# It costs about a minute: `idle_stop_min` is whole minutes and there is deliberately no shorter lever
# (ADR-0025). It runs right after `case_delete_session`, which is the first moment the table is empty,
# because the cap is one; it removes what it made, so the cap cases that follow still find the free
# slot they assume.
case_idle_stop() {
  local name=case_idle_stop response body idle fresh
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'

  fetch_body "${name}_created" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-idle-1" ||
    fail "${name}_created/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_created")"
  expect_status "${name}_created" "$response" 202
  body="$(cat "$WORK/responses/${name}_created.body")"
  idle="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$idle" ] || fail "${name}_created/id" "the response names no id: $body"
  wait_for_idle_id "${name}_created" "$idle"

  wait_for_stopped_id "${name}_stopped" "$idle"
  body="$(cat "$WORK/responses/${name}_stopped.body")"
  case "$body" in
    *'"status_reason":"idle_stop"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_stopped/reason" "the stopped row does not say why: $body"
      ;;
  esac

  # The fourth bullet's first half: the transition, in the transcript, with the reason.
  fetch_sse_until "${name}_stream" "/api/sessions/$idle/events" '"reason":"idle_stop"' \
    "Host: 127.0.0.1" ||
    fail "${name}_stream/no_answer" "GET /api/sessions/$idle/events did not answer"
  response="$(answer "${name}_stream")"
  expect_status "${name}_stream" "$response" 200
  expect_header "${name}_stream" "$response" content-type "text/event-stream"
  body="$(cat "$WORK/responses/${name}_stream.body")"
  case "$body" in
    *'"from":"idle","to":"stopped","step":null,"reason":"idle_stop"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_stream/transition" \
        "the idle stop's transition is not in the session's transcript: $body"
      ;;
  esac

  # And the second half: one log line per stop, naming the session, for an operator reading journald.
  if grep -q "holosmith::sweeper: the session sat idle for too long and was stopped session=$idle" \
    "$serve_err"; then
    assertions=$((assertions + 1))
  else
    fail "${name}/log" \
      "the sweeper wrote no line for $idle: $(grep 'holosmith::sweeper' "$serve_err" | sed -n '1,5p')"
  fi

  # The slot is free: a create at the cap of one is accepted, and the session it did not have to evict
  # still says `idle_stop` rather than `evicted`.
  fetch_body "${name}_fresh" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-idle-2" ||
    fail "${name}_fresh/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_fresh")"
  expect_status "${name}_fresh" "$response" 202
  body="$(cat "$WORK/responses/${name}_fresh.body")"
  fresh="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$fresh" ] || fail "${name}_fresh/id" "the response names no id: $body"
  [ "$fresh" != "$idle" ] ||
    fail "${name}_fresh/id" "the create answered the session the sweep had stopped: $body"

  fetch "${name}_stopped_row" GET "/api/sessions/$idle" "Host: 127.0.0.1" ||
    fail "${name}_stopped_row/no_answer" "GET /api/sessions/$idle did not answer"
  body="$(cat "$WORK/responses/${name}_stopped_row.body")"
  case "$body" in
    *'"status":"stopped"'*'"status_reason":"idle_stop"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_stopped_row/status" \
        "the create evicted the session the sweep had already stopped: $body"
      ;;
  esac

  # Nothing left behind: the cap is one, and the cases after this one assume a free slot.
  remove_session "${name}_removed_idle" "$idle"
  remove_session "${name}_removed_fresh" "$fresh"
}

# HS-019: a delete for a session the table does not hold is ADR-0008's 404 `not_found` — the
# deterministic half of "a second call returns 202/404": while the row exists a repeat is a 202, and
# once it is gone it is this.
case_delete_unknown_session() {
  local name=case_delete_unknown_session response
  fetch "$name" DELETE /api/sessions/0000000000000000 \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "$name/no_answer" "DELETE an unknown session did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 404
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" not_found
}

# HS-019: the first `DELETE` route in this crate passes the guards like every other request that
# changes something. The origin is right and the client header is missing, because the guards check
# the origin first and a request missing both would prove `bad_origin` instead.
case_delete_without_the_client_header() {
  local name=case_delete_without_the_client_header response
  fetch "$name" DELETE /api/sessions/0000000000000000 \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" ||
    fail "$name/no_answer" "DELETE without the client header did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 403
  expect_header "$name" "$response" content-type "application/json"
  expect_code "$name" "$response" bad_client
}

# HS-020: at the cap, a create stops the least recently active idle session and takes its slot. The
# case makes its own pair — by the time it runs, the session the earlier cases used has been deleted,
# so the slot is free — and it is the last case, so nothing after it depends on what it leaves.
#
# "Nothing is deleted by the cap" is asserted twice: the row is still there and `stopped`, and its
# stream still replays its transcript, whose last event is the eviction itself.
case_create_at_cap_evicts() {
  local name=case_create_at_cap_evicts response body victim fresh
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'

  fetch_body "${name}_victim" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-evict-1" ||
    fail "${name}_victim/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_victim")"
  expect_status "${name}_victim" "$response" 202
  body="$(cat "$WORK/responses/${name}_victim.body")"
  victim="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$victim" ] || fail "${name}_victim/id" "the response names no id: $body"
  wait_for_idle_id "${name}_victim" "$victim"

  # The cap is one, so this create finds the machine full — and the session holding the slot is idle,
  # which is exactly the case that evicts instead of refusing.
  fetch_body "${name}_fresh" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-evict-2" ||
    fail "${name}_fresh/no_answer" "POST /api/sessions did not answer"
  response="$(answer "${name}_fresh")"
  expect_status "${name}_fresh" "$response" 202
  body="$(cat "$WORK/responses/${name}_fresh.body")"
  fresh="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$fresh" ] || fail "${name}_fresh/id" "the response names no id: $body"
  [ "$fresh" != "$victim" ] ||
    fail "${name}_fresh/id" "the create answered the session it had to replace: $body"
  assertions=$((assertions + 1))

  fetch "${name}_victim_row" GET "/api/sessions/$victim" "Host: 127.0.0.1" ||
    fail "${name}_victim_row/no_answer" "GET /api/sessions/$victim did not answer"
  response="$(answer "${name}_victim_row")"
  expect_status "${name}_victim_row" "$response" 200
  body="$(cat "$WORK/responses/${name}_victim_row.body")"
  case "$body" in
    *'"status":"stopped"'*'"status_reason":"evicted"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_victim_row/status" \
        "the evicted session is not stopped with its reason: $body"
      ;;
  esac

  fetch_sse_until "${name}_victim_stream" "/api/sessions/$victim/events" '"reason":"evicted"' \
    "Host: 127.0.0.1" ||
    fail "${name}_victim_stream/no_answer" "GET /api/sessions/$victim/events did not answer"
  response="$(answer "${name}_victim_stream")"
  expect_status "${name}_victim_stream" "$response" 200
  expect_header "${name}_victim_stream" "$response" content-type "text/event-stream"
  body="$(cat "$WORK/responses/${name}_victim_stream.body")"
  case "$body" in
    *'"from":"idle","to":"stopped","step":null,"reason":"evicted"'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_victim_stream/transition" \
        "the eviction's transition is not in the victim's transcript: $body"
      ;;
  esac

  # Settle the session that took the victim's slot, so the next case knows the state it starts from:
  # the cap is one, and a refusal needs the live session to be *not* idle.
  wait_for_idle_id "${name}_fresh" "$fresh"
}

# HS-023's proof from the artifact: a ceiling under one turn's spend ends the turn that spends it, and
# the spend is persisted on the row.
#
# The ceiling cannot live in the one config the cases above share — with `max_session_tokens` set there,
# every prompt above would end `budget_exceeded` and a dozen assertions would be reading the budget
# instead of what they are named for. So this case stops the server and boots the same artifact again on
# a config and a data root of its own, which is also what makes it own every row it asserts: nothing
# before it is on that machine, and nothing after it may assume the first boot's state.
case_token_budget() {
  local name=case_token_budget response body id
  local create_body='{"project":"holosmith","model":"claude-sonnet-4.5","thinking":"high"}'

  stop_serve
  # The stub's turn reports 1540 tokens (1200 in, 340 out), so 1000 is spent by its `message.end` and
  # the turn that reported it is the one that ends. No `turn_timeout_min` and no `idle_stop_min`: this
  # boot is about one policy, and the two timers would only make the wait longer.
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\nmax_session_tokens = 1000\n' "$owner" \
    >"$WORK/config-budget.toml"
  start_serve "$WORK/config-budget.toml" "$WORK/data-budget"

  fetch_body "$name" POST /api/sessions "$create_body" \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" "Idempotency-Key: verify-budget-1" ||
    fail "$name/no_answer" "POST /api/sessions did not answer"
  response="$(answer "$name")"
  expect_status "$name" "$response" 202
  body="$(cat "$WORK/responses/$name.body")"
  id="$(printf '%s\n' "$body" | sed -n 's/.*"id":"\([0-9a-f]\{16\}\)".*/\1/p')"
  [ -n "$id" ] || fail "$name/id" "the response names no id: $body"
  wait_for_idle_id "${name}_idle" "$id"

  fetch_body "${name}_prompt" POST "/api/sessions/$id/prompt" '{"prompt":"read the app shell"}' \
    "Host: 127.0.0.1" "Origin: $control_origin" "Content-Type: application/json" \
    "X-Holosmith-Client: 1" "Tailscale-User-Login: $owner" ||
    fail "${name}_prompt/no_answer" "POST /api/sessions/$id/prompt did not answer"
  expect_status "${name}_prompt" "$(answer "${name}_prompt")" 202

  # The reason reaches the log, which is the transcript's copy of it. Which of the two endings records
  # it is the poll's to decide (it may not fall between the message's end and the turn's), so the case
  # asserts the reason and not the path it took — that half is `tests/token_budget.rs`.
  fetch_sse_until "${name}_stream" "/api/sessions/$id/events" '"reason":"budget_exceeded"' \
    "Host: 127.0.0.1" ||
    fail "${name}_stream/no_answer" "GET /api/sessions/$id/events did not answer"
  expect_status "${name}_stream" "$(answer "${name}_stream")" 200

  # The row: the reason, the session back in `idle`, and the two counts the writer persisted from the
  # `message.end` the turn reported.
  wait_for_reason_id "${name}_row" "$id" budget_exceeded
  body="$(cat "$WORK/responses/${name}_row.body")"
  case "$body" in
    *'"usage_in":1200'*'"usage_out":340'*)
      assertions=$((assertions + 1))
      ;;
    *)
      fail "${name}_row/usage" "the turn's usage was not persisted on the row: $body"
      ;;
  esac
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

# start_serve [config] [data dir]: boot the artifact and read the two values the cases need from its
# banner. `case_token_budget` boots a second time with a config of its own, so both files are
# parameters, and so is the banner it reads: a case that ran against the first boot must not read the
# second's.
start_serve() {
  local config="${1:-$WORK/config.toml}" data="${2:-$WORK/data}" tag=serve
  [ "$data" = "$WORK/data" ] || tag="serve-$(basename "$data")"
  serve_err="$WORK/$tag.err"
  # --port 0 asks the kernel for the port, and it is also what makes serve re-derive
  # control_origin from the bound address (ADR-0007): the passing case can then send exactly
  # the origin the guard compares. --data-dir keeps the run out of /var/lib/holosmith.
  "$artifact" serve --config "$config" --port 0 --data-dir "$data" \
    >"$WORK/$tag.out" 2>"$serve_err" &
  serve_pid=$!
  for _ in $(seq 1 200); do
    grep -q '^control_listen ' "$serve_err" 2>/dev/null && break
    kill -0 "$serve_pid" 2>/dev/null || break
    sleep 0.05
  done

  # Both read from the banner, never assumed: the port the kernel gave, and the origin the
  # guard compares.
  control_listen="$(sed -n 's/^control_listen //p' "$serve_err" | sed -n '1p')"
  [ -n "$control_listen" ] ||
    fail "serve/banner" "serve printed no control_listen line: $(cat "$serve_err")"
  port="${control_listen##*:}"
  control_origin="$(sed -n 's/^control_origin //p' "$serve_err" | sed -n '1p')"
  [ -n "$control_origin" ] ||
    fail "serve/banner" "serve printed no control_origin line: $(cat "$serve_err")"
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

  # The owner the bad_owner case needs, the cap the create cases need, the backend: `stub` from the
  # *file*, so `--backend` is not what makes this boot work (HS-011's acceptance is both spellings;
  # `tests/serve_cli.rs` covers the flag). And the idle stop at its shortest bound, which is one
  # minute: `idle_stop_min` is whole minutes on purpose, so `case_idle_stop` costs that minute rather
  # than a knob that would be a second spelling of the policy (ADR-0025).
  printf 'owner = "%s"\nmax_live_sessions = 1\nbackend = "stub"\nidle_stop_min = 1\nturn_timeout_min = 1\n' "$owner"   \
    >"$WORK/config.toml"

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
